//! 组织同步编排（kernel 层 async worker）：org-share 推送、org-pull 反熵对账、
//! keepalive 组织保活。对齐 TS p2p/org-share-sync.ts、org-pull-sync.ts 与
//! p2p-node.ts `maintainOrganizationNetwork`（org.md §6-§12、p2p-messages.md §9/§12）。
//!
//! 线程模型：全部方法为 async，跑在 kernel 内部 tokio runtime 上（事件泵/worker
//! 或门面方法的 `block_on`）。存储经 [`SledStorage`] 克隆句柄访问（线程安全）。
//!
//! ## 与 TS 的有意差异（均已记录）
//!
//! 1. **reconcile 反推的 targetRootId**（org-pull-sync.ts:372/396）：TS 传的是
//!    **本机** currentRootId，对端接收校验（targetRootId 必须等于对端当前
//!    rootId）恒拒——跨身份反推从未生效（仅同身份多设备成立）。Rust 先按对端
//!    peerId 在组织成员表里反查目标 rootId，查不到才回退 TS 原值（同身份
//!    多设备路径不受影响）。
//! 2. 推送触发点与 TS 一致（addMember / claim 落库后，尽力而为），但 TS 的
//!    "先推送后落库"顺序拉平为"落库后异步推送"（kernel 门面为同步 API，
//!    推送经 worker 队列异步执行；TS 推送失败本就只 warn 不阻断落库）。
//! 3. removeMember / applyIncomingOrgShare 不触发推送（与 TS 一致——移除经
//!    org-pull `removed` 状态传播）。
//!
//! 代码组织：本文件为 [`OrgSyncContext`]（worker 与门面共享的句柄包）、worker
//! 主循环与各链路共用的私有辅助；org-share 推送在 `push`，org-pull 反熵对账
//! 在 `pull`，keepalive 周期任务在 `tick`（M6 后零主动外联），事件驱动补副本纯决策
//! 在 `replica`（由 `push` 写入路径触发），失联恢复在 `recovery`。

mod dial;
mod orgsync_hello;
mod pull;
mod push;
mod recovery;
mod replica;
#[cfg(test)]
mod stall_tests;
mod tick;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tokio::sync::broadcast;

use crate::collection::DocumentCollection;
use crate::org::sync_state::OrgSyncState;
use crate::org::{OrganizationRecord, OrganizationService};
use crate::p2p::keepalive::RecoveryTrigger;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::{PeerNodeInfo, extract_peer_id};
use crate::p2p::{P2pEvent, P2pNode};
use crate::storage::StorageBackend;

use super::host::{CollectionConfigs, SharedOrgShareAckTracker};

/// pubsub 兜底重试节奏（org-share-sync.ts:444）。
const RETRY_INTERVALS_MS: [u64; 5] = [0, 400, 1000, 2000, 3500];
/// 每次 pubsub 发布后的 ack 等待窗口（org-share-sync.ts:461）。
const ACK_WAIT_MS: u64 = 1500;
/// 等待对端订阅 spark-sync 的总窗口（org-share-session.ts waitForTopicSubscriber 5000ms）。
const SUBSCRIBER_WAIT_MS: u64 = 5000;
/// 订阅者轮询间隔（org-share-session.ts 200ms）。
const SUBSCRIBER_POLL_MS: u64 = 200;
/// keepalive 每 tick 反熵拉取的候选数（p2p-node.ts:417 `pulled >= 2`）。
const PULL_CANDIDATES_PER_TICK: usize = 2;
/// 补副本每组织最多推送成员数（p2p-node.ts:553 `pushedForOrg >= 2`）。
const REPLICA_PUSH_PER_ORG: usize = 2;
/// recovery 每轮查询的组织数（p2p-node.ts:481 `view.slice(0, 3)`）。
const RECOVERY_ORGS_PER_ROUND: usize = 3;
/// recovery 命中候选拨号上限（p2p-node.ts:486 `dialedCount >= 4`）。
const RECOVERY_DIAL_BUDGET: usize = 4;

/// 组织地址记录的 DHT/gossip 重发间隔（p2p-messages.md §16：周期重发同 §13.2，
/// 即 DHT 记录 TTL 8h 之半）。
const ORG_ADDRESS_REPUBLISH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

/// 自设备稳态周期 hello 兜底间隔：StayConnected 期间即使无任何变更也按此
/// 周期发一次 pdsync-hello，作为投递静默失败（断→连跳变的 Resync hello 丢
/// 失等）的收敛兜底。变更触发的增量 hello 见 [`SelfHelloState::observe`]。
const SELF_DEVICE_HELLO_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// 补副本事件驱动后的最小检查间隔（每 org）：连续多次组织写入不应对同一
/// 组织反复全量扫描 + 推送（副本不足且目标离线时推送失败不写 sync-state，
/// 会持续判定不足）。写入触发时若距上次检查 < 该间隔则短路跳过。
const REPLICA_CHECK_MIN_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// 懒连接链「DHT 刷新」环节的最小触发间隔（每 rootId）：组织写入连败时每次
/// 写入都会走到本环节（N 个 DHT get + ≤[`RECOVERY_DIAL_BUDGET`] 个
/// connect_peer），远超出站预算。距上次刷新 < 该间隔直接跳过本次刷新
/// （事件驱动非周期，重复触发防风暴；参照 REPLICA_CHECK_MIN_INTERVAL_MS）。
const RECOVERY_REFRESH_MIN_INTERVAL_MS: i64 = 60_000;

/// keepalive tick 四阶段超时预算（org-sync-stall-fix §3.2，F6）：单 tick 的
/// 串行链在 join 编排期可吃满到分钟级，队列积压把末段 orgsync-hello 无限
/// 推迟；分阶段 `timeout` 后任一阶段超时即放弃本阶段进入下一阶段（阶段间
/// 无依赖），S3（orgsync-hello，tick 出口语义）恒执行不被前序超时跳过。
/// 总预算 ≤50s（另加 S1/S2 前一次 `local_node_info` 读取，API 层 5s 超时
/// 兜底，§3.3），约小于生产 keepalive 间隔（60s）；e2e 1s 注入下超时轮次
/// 由注入合并（[`inject_keepalive_tick`]）吸收，不积压。
#[derive(Clone, Copy, Debug)]
pub(crate) struct TickStageBudgets {
    /// S0 网关/地址发布（正常为本地读写 + 即时返回的 provide）。
    pub gateway_publish: std::time::Duration,
    /// S1 自设备链路（含 Resync 快照 4 连发；最坏 4×dm 超时的截断）。
    pub self_device_link: std::time::Duration,
    /// S2 反熵对账（reconcile × ≤2 候选；给一次 connect+list 超时的完整余量）。
    pub reconcile: std::time::Duration,
    /// S3 orgsync-hello（逐端点 dm 直发，正常 <100ms）。
    pub orgsync_hello: std::time::Duration,
}

impl Default for TickStageBudgets {
    fn default() -> Self {
        Self {
            gateway_publish: std::time::Duration::from_secs(10),
            self_device_link: std::time::Duration::from_secs(10),
            reconcile: std::time::Duration::from_secs(20),
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
    /// 向已知成员推送该组织快照（service.ts `syncOrganizationToKnownMembers`；
    /// `actor_root_id` 为操作者，从接收方集合排除）。
    PushOrg {
        /// 组织 id。
        org_id: String,
        /// 操作者 rootId（addMember 为当前管理员，claim 落库后为本机当前用户）。
        actor_root_id: String,
    },
    /// keepalive tick 的组织层保活（只读可达性发布 + 已连接集上的反熵/hello；
    /// M6 后零主动外联，补副本已挪到 `push_org_to_known_members` 事件路径）。
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
    pub(crate) collection_configs: CollectionConfigs,
    pub(crate) org_acks: SharedOrgShareAckTracker,
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    pub(crate) recovery_trigger: Arc<Mutex<RecoveryTrigger>>,
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
    /// 已证明支持 orgsync 的成员设备 peerId 集合（O2b §20.8；host 验签通过后
    /// 按连接层 peerId 写入——按设备粒度；org-share/org-pull 出站读取决定是否
    /// 回退旧快照链路）。
    pub(crate) orgsync_capable_member_peers:
        Arc<Mutex<std::collections::HashSet<String>>>,
    /// 自设备稳态 hello 触发状态（变更 digest 基线 + 周期兜底计时；仅
    /// keepalive tick 的 StayConnected/Resync 分支读写）。
    pub(crate) self_hello_state: Arc<Mutex<SelfHelloState>>,
    /// 即时 hello 防抖状态（仅 worker 的 SelfHelloNow 分支读写）。
    pub(crate) self_hello_immediate: Arc<Mutex<ImmediateHelloState>>,
    /// O3 filtered 集合权限钩子注册表（= kernel `plugin_host.filter_caps`）。
    /// orgsync-hello 出站读取判定「本机数据账号对 filtered 集合是否有插件
    /// 运行时支撑」——无支撑的 filtered 集合在 hello 中降标（不宣告可服务）。
    pub(crate) filter_caps: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// 补副本事件驱动节流状态：orgId → 最近一次检查时间（ms）。组织写入推送
    /// 路径（`ensure_replicas_after_write`）消费，跨写入短路径跳过重复扫描+推送。
    pub(crate) replica_check: Arc<Mutex<HashMap<String, i64>>>,
    /// 「DHT 刷新」环节节流状态：rootId → 最近一次刷新时间（ms）。组织写入
    /// 连败路径（`refresh_org_endpoints_and_dial`）消费，超限跳过重复
    /// DHT 查询 + 拨号（出站防风暴）。
    pub(crate) recovery_refresh: Arc<Mutex<HashMap<String, i64>>>,
    /// KeepaliveTick 在飞标记（org-sync-stall-fix §3.1，F6）：事件泵注入侧
    /// 置位（[`inject_keepalive_tick`]），worker 开始处理 tick 时清除——
    /// 处理期间到达的新 tick 事件可再注入一份，队列至多积压 1 份 tick。
    pub(crate) tick_in_flight: Arc<AtomicBool>,
    /// tick 四阶段超时预算（org-sync-stall-fix §3.2；测试可注入缩短值）。
    pub(crate) tick_budgets: TickStageBudgets,
    /// org:meta 写路径原子段互斥锁（org-meta-rmw-fix §2.2，F8）：kernel
    /// `io_lock` 同一把——worker 的 org:meta 写（pull 快照应用/claim 回填/
    /// recovery 补齐）经 `update_record_atomic` 持锁，与入站落库互斥。
    pub(crate) io_lock: Arc<Mutex<()>>,
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
                OrgSyncRequest::PushOrg {
                    org_id,
                    actor_root_id,
                } => ctx.push_org_to_known_members(&org_id, &actor_root_id).await,
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

    fn make_collection(&self, domain: &str, collection: &str) -> DocumentCollection {
        let config = self
            .collection_configs
            .lock()
            .unwrap()
            .get(&(domain.to_string(), collection.to_string()))
            .cloned()
            .unwrap_or_default();
        DocumentCollection::new(domain, collection, config)
    }

    /// 读取 org-sync-state（缺失/损坏 → None）。O1 账号口径：rootId 定键，
    /// 旧 peerId 键自动迁移读取回填。
    fn read_sync_state(&self, root_id: &str, org_id: &str, legacy_peer_id: Option<&str>) -> Option<OrgSyncState> {
        let mut storage = self.storage.clone();
        crate::org::sync_state::read_org_sync_state_account(
            &mut storage,
            root_id,
            org_id,
            legacy_peer_id,
        )
    }

    /// 写入 org-sync-state（O1 账号口径：rootId 定键——同账号多设备/peerId
    /// 漂移共享一份记账）。
    fn save_sync_state(&self, root_id: &str, org_id: &str, state: OrgSyncState) {
        let mut storage = self.storage.clone();
        if let Err(e) = storage.put(
            &crate::org::sync_state::org_sync_state_account_key(root_id, org_id),
            &state.to_json(),
        ) {
            self.warn(format!("org sync state save failed: {e}"));
        }
    }
}

/// `pull_org_apply` 的分支结果。
enum PullBranch {
    /// 已有终态（拉取/删除/合并完成，或合并失败已告警）。
    Applied,
    /// 无有效响应（调用方可决定反推）。
    Unavailable,
}

/// `crypto.randomBytes(12).toString('hex')`（24 hex，org-share-sync.ts:391）。
fn generate_sync_id() -> String {
    use rand::Rng as _;
    let mut bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// `collectOrganizationPeerCandidates`（peer-activity-store.ts:210-259）：
/// 当前用户为成员的组织中，其他成员的 nodeInfo 按 peerId 合并（地址去重
/// 并集），无 peerId 的按地址串键去重。损坏记录跳过（TS catch 静默）。
///
/// 多设备同步修复：候选额外纳入**同身份已配对自设备**（个人空间
/// rootId==自己 且带 peer 寻址的朋友记录）——自设备间经 org-pull 反熵
/// 对账（pull-list 捎带自签 claim → 逐组织快照合并），新设备由此从在线
/// 自设备拉回「我的组织」全量记录；`local_peer_id`（本机）排除在外。
fn collect_org_peer_candidates(
    storage: &crate::kernel::KernelStorage,
    current_root_id: &str,
    local_peer_id: Option<&str>,
) -> Vec<PeerNodeInfo> {
    let records = OrganizationService::read_all_organizations(storage).unwrap_or_default();
    let mut by_peer: HashMap<String, PeerNodeInfo> = HashMap::new();
    let mut by_address: HashMap<String, PeerNodeInfo> = HashMap::new();
    // 自设备候选：双来源合并（去重）——
    // 1) FriendRecord.peers（配对握手回填，组织候选主通道，多设备遍历）
    // 2) DeviceRecord（设备管理记录，QR 恢复后即有，不依赖 friend-request
    //    投递成功——org-pull 等自设备同步在 friend-request 丢失时仍可工作）
    let friend_self_peers = crate::contact::ContactService::overview(storage, "personal")
        .map(|view| view.friends)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.root_id == current_root_id)
        .flat_map(|f| f.peers)
        .map(|p| PeerNodeInfo {
            peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
            addresses: p.addresses,
        });
    // DeviceRecord 只提供 peerId（无监听地址）；已连接时 dm_direct 短路直发，
    // 未连接时靠懒拨号（写入触发 `self_hello_now` / M2 登录刷新）建立连接后投递
    // （M6 后 tick 不再补拨）。
    let device_self_peers = crate::device::DeviceService::list(storage)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.peer_id != local_peer_id.unwrap_or(""))
        .map(|r| PeerNodeInfo {
            peer_id: Some(r.peer_id),
            addresses: Vec::new(),
        });
    let self_peers = friend_self_peers.chain(device_self_peers);
    for candidate in self_peers {
        if candidate.peer_id.as_deref() == local_peer_id {
            continue;
        }
        if let Some(peer_id) = extract_peer_id(&candidate) {
            let entry = by_peer.entry(peer_id.clone()).or_insert_with(|| PeerNodeInfo {
                peer_id: Some(peer_id),
                addresses: Vec::new(),
            });
            for addr in &candidate.addresses {
                if !entry.addresses.contains(addr) {
                    entry.addresses.push(addr.clone());
                }
            }
            continue;
        }
        let key = candidate.addresses.join("|");
        if !key.is_empty() {
            by_address.entry(key).or_insert(candidate);
        }
    }
    for record in records {
        if !record.members.iter().any(|m| m.root_id == current_root_id) {
            continue;
        }
        for member in &record.members {
            if member.root_id == current_root_id {
                continue;
            }
            let Some(set) = &member.node_info else {
                continue;
            };
            // 端点化：遍历成员端点集，逐端点作为拨号候选（多设备聚合）。
            for info in set.iter() {
                let candidate = PeerNodeInfo {
                    peer_id: info.peer_id.clone(),
                    addresses: info.addresses.clone(),
                };
                if let Some(peer_id) = extract_peer_id(&candidate) {
                    let entry = by_peer
                        .entry(peer_id.clone())
                        .or_insert_with(|| PeerNodeInfo {
                            peer_id: Some(peer_id),
                            addresses: Vec::new(),
                        });
                    for addr in &candidate.addresses {
                        if !entry.addresses.contains(addr) {
                            entry.addresses.push(addr.clone());
                        }
                    }
                    continue;
                }
                let key = candidate.addresses.join("|");
                if !key.is_empty() {
                    by_address.entry(key).or_insert(candidate);
                }
            }
        }
    }
    by_peer
        .into_values()
        .chain(by_address.into_values())
        .collect()
}

/// 本地相关组织（org-pull-sync.ts:133-147）：当前用户为成员的组织。
fn list_local_related_orgs(
    storage: &crate::kernel::KernelStorage,
    current_root_id: &str,
) -> crate::org::Result<HashMap<String, OrganizationRecord>> {
    let records = OrganizationService::read_all_organizations(storage)?;
    Ok(records
        .into_iter()
        .filter(|r| r.members.iter().any(|m| m.root_id == current_root_id))
        .map(|r| (r.org_id.clone(), r))
        .collect())
}

/// 反推目标 rootId 解析（有意差异 1）：按对端 peerId 在本地组织成员表里
/// 反查；查不到返回 None（调用方回退 TS 原值=本机 rootId，同身份多设备仍通）。
fn resolve_push_target_root_id(
    record: &OrganizationRecord,
    node_info: &PeerNodeInfo,
) -> Option<String> {
    let peer_id = extract_peer_id(node_info)?;
    record
        .members
        .iter()
        .find(|m| {
            // 端点化：遍历成员端点集匹配 peerId。
            m.node_info.as_ref().is_some_and(|set| {
                set.iter()
                    .any(|e| e.peer_id.as_deref().map(str::trim) == Some(peer_id.as_str()))
            })
        })
        .map(|m| m.root_id.clone())
}
