//! 组织同步编排（kernel 层 async worker）：orgsync 反熵（hello/need/data）
//! 与 keepalive 组织保活。
//!
//! **阶段四A P3（legacy 出站停发）**：org-share 快照推送与 org-pull 反熵
//! 对账的**出站**已删除（能力探测回退一并删）——组织更新传播由 orgsync
//! 承接（写入事件即时 hello + tick 周期 hello；join 走 P2 的 stub 自举 +
//! 收敛等待；移除走 org-member-removed dm）。**入站保留**（旧端/legacy
//! join 回退路径仍可能呼入：host 的 org-pull 响应与 org-share 合入在
//! `host`，P4 才清除）。回滚 = 版本回退（设计 §4）。
//!
//! 线程模型：全部方法为 async，跑在 kernel 内部 tokio runtime 上（事件泵/worker
//! 或门面方法的 `block_on`）。存储经 [`SledStorage`] 克隆句柄访问（线程安全）。
//!
//! 代码组织：本文件为 [`OrgSyncContext`]（worker 与门面共享的句柄包）、worker
//! 主循环与各链路共用的私有辅助；orgsync hello 触发在 `orgsync_hello`，
//! keepalive 周期任务在 `tick`（M6 后零主动外联；P3 起 S2 reconcile 段随
//! legacy pull 出站停发删除）。

mod orgsync_hello;
#[cfg(test)]
mod stall_tests;
mod tick;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tokio::sync::broadcast;

use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode};



/// 组织地址记录的 DHT/gossip 重发间隔（p2p-messages.md §16：周期重发同 §13.2，
/// 即 DHT 记录 TTL 8h 之半）。
const ORG_ADDRESS_REPUBLISH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

/// 自设备稳态周期 hello 兜底间隔：StayConnected 期间即使无任何变更也按此
/// 周期发一次 pdsync-hello，作为投递静默失败（断→连跳变的 Resync hello 丢
/// 失等）的收敛兜底。变更触发的增量 hello 见 [`SelfHelloState::observe`]。
const SELF_DEVICE_HELLO_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// keepalive tick 分阶段超时预算（org-sync-stall-fix §3.2，F6）：单 tick 的
/// 串行链在 join 编排期可吃满到分钟级，队列积压把末段 orgsync-hello 无限
/// 推迟；分阶段 `timeout` 后任一阶段超时即放弃本阶段进入下一阶段（阶段间
/// 无依赖），S3（orgsync-hello，tick 出口语义）恒执行不被前序超时跳过。
/// 总预算 ≤30s（另加 S1 前一次 `local_node_info` 读取，API 层 5s 超时
/// 兜底，§3.3），约小于生产 keepalive 间隔（60s）；e2e 1s 注入下超时轮次
/// 由注入合并（[`inject_keepalive_tick`]）吸收，不积压。
/// （P3：S2 reconcile 预算字段随 legacy pull 出站停发删除。）
#[derive(Clone, Copy, Debug)]
pub(crate) struct TickStageBudgets {
    /// S0 网关/地址发布（正常为本地读写 + 即时返回的 provide）。
    pub gateway_publish: std::time::Duration,
    /// S1 自设备链路（含 Resync 快照 4 连发；最坏 4×dm 超时的截断）。
    pub self_device_link: std::time::Duration,
    /// S3 orgsync-hello（逐端点 dm 直发，正常 <100ms）。
    pub orgsync_hello: std::time::Duration,
}

impl Default for TickStageBudgets {
    fn default() -> Self {
        Self {
            gateway_publish: std::time::Duration::from_secs(10),
            self_device_link: std::time::Duration::from_secs(10),
            orgsync_hello: std::time::Duration::from_secs(10),
        }
    }
}

/// KeepaliveTick 注入合并（org-sync-stall-fix §3.1，F6 背压）：tick 是幂等
/// 周期任务，积压 N 份与 1 份语义相同——已有在飞（已注入未开始处理）的
/// tick 时跳过注入。`in_flight` 标记由事件泵注入侧（本函数）置位、worker
/// 开始处理 tick 时清除（见 [`spawn_worker`]）。PushOrg/SelfHelloNow 是事件
/// 语义，不经过本函数、不合并。返回是否实际注入（发送失败时回退标记）。
pub(crate) fn inject_keepalive_tick(
    tx: &tokio::sync::mpsc::UnboundedSender<OrgSyncRequest>,
    in_flight: &AtomicBool,
) -> bool {
    if in_flight.swap(true, Ordering::SeqCst) {
        return false;
    }
    if tx.send(OrgSyncRequest::KeepaliveTick).is_err() {
        in_flight.store(false, Ordering::SeqCst);
        return false;
    }
    true
}

/// 自设备稳态 hello 触发状态（org-sync tick `maintain_self_device_link` 的
/// StayConnected 分支用；跨 tick 持久，断→连跳变的 Resync 会重置重建基线）。
///
/// 判定来源是**本机个人域写入 digest**（各类目全部记录 pmeta 的本机 nodeId
/// 分量逐记录求和，见 tick.rs `local_personal_write_digest`）：本机写入恒
/// bump 被写记录的本机分量使 digest 严格递增，远端合入（`apply_personal_remote`
/// 家族）落远端 pmeta、不推高本机分量——digest 只对「本机产生的写入」
/// 敏感，远端合入不触发，无回声循环。
#[derive(Default)]
pub(crate) struct SelfHelloState {
    /// 上次观察到的本机写入 digest（None = 尚未建基线）。
    digest: Option<i64>,
    /// 上次发送稳态 hello（或建基线）的时间（ms）。
    last_sent_ms: i64,
}

impl SelfHelloState {
    /// 观察当前 digest 并判定是否应发稳态 hello（命中即推进状态）：
    /// - 首次观察：只建基线不发（进入 StayConnected 前的 Resync 已发 hello，
    ///   且 p2p 重启后的首个 tick 不应因基线缺失误发）；
    /// - digest 变化（本机个人域写入）：发——变更即触发，≤1 tick 传播；
    /// - digest 未变但到达 [`SELF_DEVICE_HELLO_INTERVAL_MS`]：发（周期兜底）。
    fn observe(&mut self, digest: i64, now_ms: i64) -> bool {
        match self.digest {
            None => {
                self.digest = Some(digest);
                self.last_sent_ms = now_ms;
                false
            }
            Some(prev) if prev != digest => {
                self.digest = Some(digest);
                self.last_sent_ms = now_ms;
                true
            }
            Some(_) if now_ms - self.last_sent_ms >= SELF_DEVICE_HELLO_INTERVAL_MS => {
                self.last_sent_ms = now_ms;
                true
            }
            _ => false,
        }
    }
}

/// 即时 hello 的最短间隔（防抖窗口）：窗口内的再次删除触发记一次尾随
/// 补发，覆盖批量删除的尾巴。
const SELF_HELLO_IMMEDIATE_MIN_INTERVAL_MS: i64 = 1_000;

/// 即时 hello 防抖状态（仅 org-sync worker 的 `SelfHelloNow` 分支读写）。
#[derive(Default)]
pub(crate) struct ImmediateHelloState {
    /// 上次即时 hello 发送时间（ms）。
    last_sent_ms: i64,
    /// 防抖窗口内是否已登记尾随补发任务。
    trailing_pending: bool,
}

/// org-sync worker 的请求队列项。
#[derive(Clone, Debug)]
pub(crate) enum OrgSyncRequest {
    /// 组织写入事件（addMember 等）：向该组织已连接复制组成员发即时
    /// orgsync-hello（P3 起不再推 org-share 快照——快照推送出站已停发，
    /// 更新传播由 orgsync 反熵承接）。`actor_root_id` 保留签名兼容（P4
    /// 清理）。
    PushOrg {
        /// 组织 id。
        org_id: String,
    },
    /// keepalive tick 的组织层保活（只读可达性发布 + 已连接集上的反熵/hello；
    /// M6 后零主动外联——补副本推送随 P3 出站停发删除，反熵由 orgsync 承接）。
    KeepaliveTick,
    /// 本机个人域删除（tombstone 写入）后的即时 hello 触发：不等
    /// keepalive tick（最坏 ~60s），立即向已连接自设备补发 pdsync-hello，
    /// 对端回 need 即拉走墓碑（删除传播秒级）。
    SelfHelloNow,
}

/// org-pull 对账计数（org-pull-sync.ts:458-467；`synced === pulled` 如实保留）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgReconcileStats {
    /// 对账的组织数（本地 ∪ 对端可见）。
    pub checked: u32,
    /// 同步成功数（恒等于 `pulled`，TS 返回形状保留）。
    pub synced: u32,
    /// 对端标记 removed 后本地删除的组织数。
    pub removed: u32,
    /// 反推尝试数。
    pub push_attempted: u32,
    /// 反推成功数。
    pub pushed: u32,
    /// 拉取成功数。
    pub pulled: u32,
    /// 版本等价跳过数（含反推无目标可寻的跳过）。
    pub skipped: u32,
}

/// ipc `p2p-sync-peer-organizations` 的返回形状（desktop/src/main/ipc/p2p.ts:86-93）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOrgSyncResult {
    /// 反推尝试数（= 对账 pushAttempted）。
    pub attempted: u32,
    /// 反推成功数（= 对账 pushed）。
    pub synced: u32,
    /// 对账组织数。
    pub pull_checked: u32,
    /// 拉取成功数。
    pub pull_synced: u32,
    /// 本地删除数。
    pub removed: u32,
    /// 跳过数。
    pub skipped: u32,
}

impl From<OrgReconcileStats> for PeerOrgSyncResult {
    fn from(stats: OrgReconcileStats) -> Self {
        Self {
            attempted: stats.push_attempted,
            synced: stats.pushed,
            pull_checked: stats.checked,
            pull_synced: stats.pulled,
            removed: stats.removed,
            skipped: stats.skipped,
        }
    }
}

/// 组织同步编排上下文（worker 与门面方法共享的句柄包；全部 Clone 廉价）。
#[derive(Clone)]
pub(crate) struct OrgSyncContext {
    pub(crate) storage: crate::kernel::KernelStorage,
    pub(crate) node: Arc<P2pNode>,
    pub(crate) current_root_id: Arc<Mutex<Option<String>>>,
    pub(crate) signing_key: Arc<Mutex<Option<SigningKey>>>,
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    /// 组织地址记录发布状态：orgAddress → 最近一次发布时间（ms）。
    /// 跨 tick 持久（kernel 持有，worker 与门面注入共用一份）。
    pub(crate) org_address_publish: Arc<Mutex<HashMap<String, i64>>>,
    /// 应用数据目录（自设备重连后读身份文件装配 profile-sync 快照用）。
    pub(crate) data_dir: std::path::PathBuf,
    /// 自设备链路状态（单一槽位，维持稳态 hello 状态机的既有语义）：
    /// 上一 tick 观察到的已连接配对设备 peerId（None=未连接）。
    /// 断→连跳变时触发 device-sync + profile-sync 快照重发，补齐对端离线
    /// 期间错过的变更（一次性启动广播无重试，靠此状态机收敛）。
    pub(crate) self_device_link: Arc<Mutex<Option<String>>>,
    /// 自设备已连接 peerId 集合（多设备视图，全 kernel 共享——事件泵在
    /// Disconnected 时复位）。"即时 hello"等触发路径遍历此集合逐台发送；
    /// 单一槽位无法表达多设备，且不复位的槽位会在断连后静默发丢。
    pub(crate) self_device_links: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（host 验签通过后按连接层
    /// peerId 写入——按设备粒度；保活读取决定是否回退发旧快照，见 §7.1）。
    pub(crate) pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 自设备稳态 hello 触发状态（变更 digest 基线 + 周期兜底计时；仅
    /// keepalive tick 的 StayConnected/Resync 分支读写）。
    pub(crate) self_hello_state: Arc<Mutex<SelfHelloState>>,
    /// 即时 hello 防抖状态（仅 worker 的 SelfHelloNow 分支读写）。
    pub(crate) self_hello_immediate: Arc<Mutex<ImmediateHelloState>>,
    /// O3 filtered 集合权限钩子注册表（= kernel `plugin_host.filter_caps`）。
    /// orgsync-hello 出站读取判定「本机数据账号对 filtered 集合是否有插件
    /// 运行时支撑」——无支撑的 filtered 集合在 hello 中降标（不宣告可服务）。
    pub(crate) filter_caps: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// KeepaliveTick 在飞标记（org-sync-stall-fix §3.1，F6）：事件泵注入侧
    /// 置位（[`inject_keepalive_tick`]），worker 开始处理 tick 时清除——
    /// 处理期间到达的新 tick 事件可再注入一份，队列至多积压 1 份 tick。
    pub(crate) tick_in_flight: Arc<AtomicBool>,
    /// tick 四阶段超时预算（org-sync-stall-fix §3.2；测试可注入缩短值）。
    pub(crate) tick_budgets: TickStageBudgets,
}

/// org-sync worker 主循环：推送/保活串行消费（kernel `start_p2p` 装配，
/// 随 p2p 起停；`KeepaliveTick` 由事件泵拦截 node 事件、经
/// [`inject_keepalive_tick`] 合并注入——幂等周期任务至多积压 1 份）。
///
/// 完成心跳（org-sync-stall-fix §3.4）：每完成一个请求打一行 INFO——停滞
/// 复现时从「最后一条 done 的 kind + 下一阶段未出现」直接读出卡点；
/// `queue_depth` 读 UnboundedReceiver 当前积压，判队列是否积压。
pub(crate) fn spawn_worker(
    handle: &tokio::runtime::Handle,
    ctx: OrgSyncContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<OrgSyncRequest>,
) -> tokio::task::JoinHandle<()> {
    handle.spawn(async move {
        while let Some(request) = rx.recv().await {
            let kind = match &request {
                OrgSyncRequest::PushOrg { .. } => "PushOrg",
                OrgSyncRequest::KeepaliveTick => "KeepaliveTick",
                OrgSyncRequest::SelfHelloNow => "SelfHelloNow",
            };
            let started = std::time::Instant::now();
            match request {
                OrgSyncRequest::PushOrg { org_id } => {
                    // P3：org-share 快照推送出站停发——「组织写入 → 通知成员」
                    // 改为对已连接复制组成员的即时 orgsync-hello（对端回 need
                    // 拉走 diff；未连接成员等其上线后的 tick/对方 hello 收敛）
                    ctx.orgsync_hello_now(&org_id).await;
                }
                OrgSyncRequest::KeepaliveTick => {
                    // 在飞标记在开始处理时清除：处理期间到达的 tick 事件可再
                    // 注入一份（合并保证至多 1 份在飞 + 1 份积压）
                    ctx.tick_in_flight.store(false, Ordering::SeqCst);
                    ctx.maintain_org_tick().await;
                }
                OrgSyncRequest::SelfHelloNow => ctx.self_hello_now(),
            }
            log::info!(
                "[ORG_SYNC] request done | kind={} elapsed={}ms queue_depth≈{}",
                kind,
                started.elapsed().as_millis(),
                rx.len(),
            );
        }
    })
}

impl OrgSyncContext {
    fn now(&self) -> i64 {
        system_now_ms()
    }

    fn root_id(&self) -> Option<String> {
        self.current_root_id.lock().unwrap().clone()
    }

    fn warn(&self, msg: impl Into<String>) {
        let _ = self.event_tx.send(P2pEvent::Warning(msg.into()));
    }

}
