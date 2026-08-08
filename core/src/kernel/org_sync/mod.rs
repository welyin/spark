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
//! 在 `pull`，keepalive 周期任务在 `tick`，失联恢复在 `recovery`。

mod pull;
mod push;
mod recovery;
mod tick;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tokio::sync::broadcast;

use crate::collection::DocumentCollection;
use crate::org::sync_state::{OrgSyncState, org_sync_state_key};
use crate::org::{OrganizationRecord, OrganizationService};
use crate::p2p::keepalive::RecoveryTrigger;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::{PeerNodeInfo, extract_peer_id};
use crate::p2p::{P2pEvent, P2pNode};
use crate::storage::{SledStorage, StorageBackend};

use super::host::{CollectionConfigs, SharedOrgShareAckTracker};

/// pubsub 兜底重试节奏（org-share-sync.ts:444）。
const RETRY_INTERVALS_MS: [u64; 5] = [0, 400, 1000, 2000, 3500];
/// 每次 pubsub 发布后的 ack 等待窗口（org-share-sync.ts:461）。
const ACK_WAIT_MS: u64 = 1500;
/// 等待对端订阅 spark-sync 的总窗口（org-share-session.ts waitForTopicSubscriber 5000ms）。
const SUBSCRIBER_WAIT_MS: u64 = 5000;
/// 订阅者轮询间隔（org-share-session.ts 200ms）。
const SUBSCRIBER_POLL_MS: u64 = 200;
/// keepalive 每 tick 候选拨号上限（p2p-node.ts:404 `dialed >= 3`）。
const DIAL_BUDGET_PER_TICK: usize = 3;
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
    /// keepalive tick 的组织层保活（候选拨号/反熵/补副本/recovery）。
    KeepaliveTick,
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
    pub(crate) storage: SledStorage,
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
    /// 自设备链路状态：上一 tick 观察到的已连接配对设备 peerId（None=未连接）。
    /// 断→连跳变时触发 device-sync + profile-sync 快照重发，补齐对端离线
    /// 期间错过的变更（一次性启动广播无重试，靠此状态机收敛）。
    pub(crate) self_device_link: Arc<Mutex<Option<String>>>,
}

/// org-sync worker 主循环：推送/保活串行消费（kernel `start_p2p` 装配，
/// 随 p2p 起停；`KeepaliveTick` 由事件泵拦截 node 事件注入）。
pub(crate) fn spawn_worker(
    handle: &tokio::runtime::Handle,
    ctx: OrgSyncContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<OrgSyncRequest>,
) -> tokio::task::JoinHandle<()> {
    handle.spawn(async move {
        while let Some(request) = rx.recv().await {
            match request {
                OrgSyncRequest::PushOrg {
                    org_id,
                    actor_root_id,
                } => ctx.push_org_to_known_members(&org_id, &actor_root_id).await,
                OrgSyncRequest::KeepaliveTick => ctx.maintain_org_tick().await,
            }
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

    /// 读取 org-sync-state（缺失/损坏 → None）。
    fn read_sync_state(&self, peer_id: &str, org_id: &str) -> Option<OrgSyncState> {
        self.storage
            .get(&org_sync_state_key(peer_id, org_id))
            .ok()
            .flatten()
            .and_then(|raw| OrgSyncState::from_json(&raw))
    }

    fn save_sync_state(&self, peer_id: &str, org_id: &str, state: OrgSyncState) {
        let mut storage = self.storage.clone();
        if let Err(e) = storage.put(&org_sync_state_key(peer_id, org_id), &state.to_json()) {
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
    storage: &SledStorage,
    current_root_id: &str,
    local_peer_id: Option<&str>,
) -> Vec<PeerNodeInfo> {
    let records = OrganizationService::read_all_organizations(storage).unwrap_or_default();
    let mut by_peer: HashMap<String, PeerNodeInfo> = HashMap::new();
    let mut by_address: HashMap<String, PeerNodeInfo> = HashMap::new();
    // 自设备候选：双来源合并（去重）——
    // 1) FriendRecord.peer（配对握手回填，组织候选主通道）
    // 2) DeviceRecord（设备管理记录，QR 恢复后即有，不依赖 friend-request
    //    投递成功——org-pull 等自设备同步在 friend-request 丢失时仍可工作）
    let friend_self_peers = crate::contact::ContactService::overview(storage, "personal")
        .map(|view| view.friends)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.root_id == current_root_id)
        .filter_map(|f| f.peer)
        .map(|p| PeerNodeInfo {
            peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
            addresses: p.addresses,
        });
    // DeviceRecord 只提供 peerId（无监听地址）；已连接时 dm_direct 短路
    // 直发，未连接时靠 keepalive tick 的补拨建立连接后再投递。
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
            let Some(info) = &member.node_info else {
                continue;
            };
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
    by_peer
        .into_values()
        .chain(by_address.into_values())
        .collect()
}

/// 本地相关组织（org-pull-sync.ts:133-147）：当前用户为成员的组织。
fn list_local_related_orgs(
    storage: &SledStorage,
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
            m.node_info
                .as_ref()
                .and_then(|n| n.peer_id.as_deref())
                .map(str::trim)
                == Some(peer_id.as_str())
        })
        .map(|m| m.root_id.clone())
}
