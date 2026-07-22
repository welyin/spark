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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::collection::DocumentCollection;
use crate::org::sync_state::{
    OrgSyncState, org_sync_state_key, should_skip_share_push, sync_state_after_pull_synced,
    sync_state_after_share_acked, sync_state_after_share_delivered,
};
use crate::org::{
    NodeInfoClaim, OrganizationNodeInfo, OrganizationRecord, OrganizationService,
    OrganizationSyncVersions, PluginDocSyncItem, PullOrgOutcome, active_recovery_tokens,
    apply_plugin_doc_sync_items, build_organization_sync_snapshot, classify_pull_org_response,
    collect_syncable_plugin_docs, compute_org_sync_overview, is_organization_sync_stale,
    parse_pull_list_organizations, resolve_local_versions, sign_node_info_claim,
};
use crate::org::types::organization_key;
use crate::p2p::constants::{RECOVERY_QUERY_WANT, SYNC_TOPIC};
use crate::p2p::direct::{build_pull_list_request, build_pull_org_request};
use crate::p2p::envelope::build_org_body;
use crate::p2p::keepalive::{RecoveryTrigger, plan_organization_dials, plan_recovery_dials};
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_activity::PeerActivityStore;
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

    // ------------------------------------------------------------------
    // org-share 推送（org-share-sync.ts:384-484 `syncOrganizationToMember`）
    // ------------------------------------------------------------------

    /// 向单个成员推送组织快照：stale 跳过 → connectPeer → 等订阅者 →
    /// 直连优先 → pubsub 五次重试等 ack → 记账。
    pub(crate) async fn sync_org_to_member(
        &self,
        node_info: &PeerNodeInfo,
        target_root_id: &str,
        org_id: &str,
    ) -> Result<(), String> {
        let record = OrganizationService::get_record(&self.storage, org_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Organization not found".to_string())?;
        // 推送线形：TS `organization.sync ? organization : buildOrganizationSyncSnapshot`
        // （原始记录优先；spec §13.3）
        let organization = if record.sync.is_some() {
            serde_json::to_value(&record).map_err(|e| e.to_string())?
        } else {
            serde_json::to_value(build_organization_sync_snapshot(&record, &[]))
                .map_err(|e| e.to_string())?
        };
        let versions = resolve_local_versions(&record);
        let target_peer_id = extract_peer_id(node_info);

        // 推送前跳过判定（正确语义版，sync_state.rs 的"有意修复"）
        if let Some(peer_id) = &target_peer_id {
            let state = self.read_sync_state(peer_id, org_id);
            if should_skip_share_push(state.as_ref(), &versions) {
                return Ok(());
            }
        }

        let sync_id = generate_sync_id();
        self.node
            .connect_peer(node_info)
            .await
            .map_err(|e| e.to_string())?;
        self.wait_topic_subscriber(target_peer_id.as_deref(), SUBSCRIBER_WAIT_MS)
            .await;

        let plugin_docs = collect_syncable_plugin_docs(&self.storage, org_id)
            .map_err(|e| e.to_string())?;
        let payload = serde_json::json!({
            "targetRootId": target_root_id,
            "syncId": sync_id,
            "organization": organization,
            "pluginDocs": serde_json::to_value(&plugin_docs).map_err(|e| e.to_string())?,
            "nodeInfo": {
                "peerId": node_info.peer_id,
                "addresses": node_info.addresses,
            },
        });

        // 直连优先：ok && syncId 匹配即送达（等价收到 ack）
        if self
            .node
            .org_share_direct(node_info, payload.clone())
            .await
            .unwrap_or(false)
        {
            if let Some(peer_id) = &target_peer_id {
                self.save_sync_state(
                    peer_id,
                    org_id,
                    sync_state_after_share_delivered(versions, self.now()),
                );
            }
            return Ok(());
        }

        // pubsub 兜底：[0, 400, 1000, 2000, 3500]ms × 5 次，每次等 ack 1500ms
        let body = build_org_body("org-share", payload);
        for (attempt, wait_ms) in RETRY_INTERVALS_MS.iter().enumerate() {
            if *wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(*wait_ms)).await;
            }
            self.node
                .broadcast(SYNC_TOPIC, body.clone())
                .await
                .map_err(|e| e.to_string())?;
            if self.wait_ack(&sync_id, ACK_WAIT_MS).await {
                if let Some(peer_id) = &target_peer_id {
                    self.save_sync_state(
                        peer_id,
                        org_id,
                        sync_state_after_share_acked(versions, self.now()),
                    );
                }
                return Ok(());
            }
            let _ = attempt;
        }
        Err(format!(
            "Organization sync ack timeout: orgId={org_id}, targetRootId={target_root_id}, syncId={sync_id}"
        ))
    }

    /// 等待 ack：先查竞态缓存（ack 先于等待到达），再注册 oneshot 等待器。
    async fn wait_ack(&self, sync_id: &str, timeout_ms: u64) -> bool {
        let rx = {
            let mut tracker = self.org_acks.lock().unwrap();
            if tracker.take_early_ack(sync_id) {
                return true;
            }
            tracker.register(sync_id)
        };
        let acked = tokio::time::timeout(Duration::from_millis(timeout_ms), rx)
            .await
            .is_ok();
        if !acked {
            self.org_acks.lock().unwrap().remove_waiter(sync_id);
        }
        acked
    }

    /// 等待对端出现在 spark-sync 订阅者列表（200ms 轮询，总窗口 5000ms；
    /// 无目标 peerId 直接返回——org-share-session.ts 同等场景不阻塞）。
    async fn wait_topic_subscriber(&self, target_peer_id: Option<&str>, budget_ms: u64) {
        let Some(target) = target_peer_id else {
            return;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(budget_ms);
        loop {
            if let Ok(info) = self.node.local_node_info().await
                && info
                    .spark_sync_subscribers
                    .iter()
                    .any(|p| p == target)
            {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(SUBSCRIBER_POLL_MS)).await;
        }
    }

    /// `syncOrganizationToKnownMembers`（service.ts:537-571）：向组织的已知
    /// 成员（排除操作者，要求 nodeInfo 可达）逐个尽力推送；失败仅告警。
    pub(crate) async fn push_org_to_known_members(&self, org_id: &str, actor_root_id: &str) {
        let record = match OrganizationService::get_record(&self.storage, org_id) {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(e) => {
                self.warn(format!("org push: read record failed: {e}"));
                return;
            }
        };
        let recipients = OrganizationService::sync_recipients(&record, actor_root_id);
        for member in recipients {
            let Some(info) = member.node_info.clone() else {
                continue;
            };
            let peer = PeerNodeInfo {
                peer_id: info.peer_id,
                addresses: info.addresses,
            };
            if let Err(e) = self
                .sync_org_to_member(&peer, &member.root_id, org_id)
                .await
            {
                // 预录模型：成员离线不视为失败（service.ts:563-569 console.warn）
                self.warn(format!(
                    "[org] member sync deferred (peer unreachable): orgId={org_id}, targetRootId={}, error={e}",
                    member.root_id
                ));
            }
        }
    }

    // ------------------------------------------------------------------
    // org-pull 反熵对账（org-pull-sync.ts:298-467 `reconcileFromPeer`）
    // ------------------------------------------------------------------

    /// 自签 nodeInfoClaim（bootstrap.ts `buildSelfNodeInfoClaim`；未解锁返回 None）。
    async fn self_node_info_claim(&self) -> Option<NodeInfoClaim> {
        let key = self.signing_key.lock().unwrap().clone()?;
        let info = self.node.local_node_info().await.ok()?;
        Some(sign_node_info_claim(
            &key,
            OrganizationNodeInfo {
                peer_id: info.peer_id,
                addresses: info.addresses,
            },
            self.now(),
        ))
    }

    /// 从某 peer 对账全部共同组织：org-pull-list（捎带 claim）→ 逐组织
    /// 双向 stale 比较 → 拉取合并 / 反推 / 删除。
    pub(crate) async fn reconcile_from_peer(
        &self,
        node_info: &PeerNodeInfo,
        with_claim: bool,
    ) -> Result<OrgReconcileStats, String> {
        let mut stats = OrgReconcileStats::default();
        let Some(root_id) = self.root_id() else {
            return Ok(stats);
        };
        self.node
            .connect_peer(node_info)
            .await
            .map_err(|e| e.to_string())?;
        let local_peer_id = self
            .node
            .local_node_info()
            .await
            .ok()
            .and_then(|i| i.peer_id);
        let claim = if with_claim {
            self.self_node_info_claim().await
        } else {
            None
        };
        let claim_value = claim
            .as_ref()
            .and_then(|c| serde_json::to_value(c).ok());

        let list_request = build_pull_list_request(
            &root_id,
            local_peer_id.as_deref(),
            claim_value,
        );
        let list_response = self
            .node
            .org_pull_request(node_info, &list_request)
            .await
            .ok()
            .flatten();
        // versions 缺失的条目按"对端没有该组织"处理（TS 的 `remote` falsy 语义）
        let remote_versions: HashMap<String, OrganizationSyncVersions> = list_response
            .as_ref()
            .map(parse_pull_list_organizations)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(org_id, v)| v.map(|v| (org_id, v)))
            .collect();

        let local_orgs = list_local_related_orgs(&self.storage, &root_id)
            .map_err(|e| e.to_string())?;
        let mut target_ids: Vec<String> = local_orgs.keys().cloned().collect();
        for org_id in remote_versions.keys() {
            if !local_orgs.contains_key(org_id) {
                target_ids.push(org_id.clone());
            }
        }
        target_ids.sort();
        stats.checked = target_ids.len() as u32;

        for org_id in target_ids {
            let local = local_orgs.get(&org_id);
            let remote = remote_versions.get(&org_id);
            match (local, remote) {
                (Some(local), None) => {
                    // 本地有、对端列表没有：先 pull-org 确认，无有效响应才反推
                    match self.pull_org_apply(node_info, &root_id, local_peer_id.as_deref(), &org_id, &mut stats).await {
                        PullBranch::Applied => {}
                        PullBranch::Unavailable => {
                            self.push_back(node_info, local, &root_id, &mut stats).await;
                        }
                    }
                }
                (Some(local), Some(remote_v)) => {
                    let local_v = resolve_local_versions(local);
                    let remote_newer = is_organization_sync_stale(Some(&local_v), remote_v);
                    let local_newer = is_organization_sync_stale(Some(remote_v), &local_v);
                    if local_newer && !remote_newer {
                        self.push_back(node_info, local, &root_id, &mut stats).await;
                        continue;
                    }
                    if !local_newer && !remote_newer {
                        stats.skipped += 1;
                        continue;
                    }
                    // 对端更新或双方分叉：拉取合并
                    self.pull_org_apply(node_info, &root_id, local_peer_id.as_deref(), &org_id, &mut stats).await;
                }
                (None, Some(_)) => {
                    self.pull_org_apply(node_info, &root_id, local_peer_id.as_deref(), &org_id, &mut stats).await;
                }
                (None, None) => {}
            }
        }
        stats.synced = stats.pulled;
        Ok(stats)
    }

    /// 反推 org-share（本地更新方向）。targetRootId 解析见模块文档"有意差异 1"。
    async fn push_back(
        &self,
        node_info: &PeerNodeInfo,
        local: &OrganizationRecord,
        fallback_root_id: &str,
        stats: &mut OrgReconcileStats,
    ) {
        stats.push_attempted += 1;
        let target_root_id = resolve_push_target_root_id(local, node_info)
            .unwrap_or_else(|| fallback_root_id.to_string());
        match self
            .sync_org_to_member(node_info, &target_root_id, &local.org_id)
            .await
        {
            Ok(()) => stats.pushed += 1,
            Err(e) => self.warn(format!(
                "[p2p][org-pull] version-plan push failed: orgId={}, error={e}",
                local.org_id
            )),
        }
    }

    /// org-pull-org 拉取并应用（merged 落库 + pluginDocs + sync-state 记账）。
    /// 返回分支供调用方决定后续动作（Unavailable 时反推）。
    async fn pull_org_apply(
        &self,
        node_info: &PeerNodeInfo,
        root_id: &str,
        local_peer_id: Option<&str>,
        org_id: &str,
        stats: &mut OrgReconcileStats,
    ) -> PullBranch {
        let request = build_pull_org_request(root_id, local_peer_id, org_id);
        let response = self
            .node
            .org_pull_request(node_info, &request)
            .await
            .ok()
            .flatten();
        match classify_pull_org_response(response.as_ref()) {
            PullOrgOutcome::Removed => {
                // org.md §9.4：removed 与"非成员"不可区分，据此删除本地记录
                let mut storage = self.storage.clone();
                match crate::storage::StorageBackend::delete(&mut storage, &organization_key(org_id)) {
                    Ok(()) => stats.removed += 1,
                    Err(e) => self.warn(format!("org-pull remove local failed: {e}")),
                }
                PullBranch::Applied
            }
            PullOrgOutcome::Member {
                organization,
                plugin_docs,
            } => {
                let now = self.now();
                let merged = match OrganizationService::apply_incoming_snapshot(
                    &mut self.storage.clone(),
                    &organization,
                    now,
                ) {
                    Ok(merged) => merged,
                    Err(e) => {
                        self.warn(format!("org-pull merge failed: {e}"));
                        return PullBranch::Applied;
                    }
                };
                self.apply_plugin_docs(&plugin_docs, now);
                stats.pulled += 1;
                // 副本记账（org-pull-sync.ts:279-296 onSyncState）
                if let Some(peer_id) = extract_peer_id(node_info) {
                    let versions = resolve_local_versions(&merged);
                    self.save_sync_state(
                        &peer_id,
                        org_id,
                        sync_state_after_pull_synced(versions, now),
                    );
                }
                PullBranch::Applied
            }
            PullOrgOutcome::Unavailable => PullBranch::Unavailable,
        }
    }

    /// pluginDocs 应用（失败仅告警，与 TS 的 catch warn 对齐）。
    fn apply_plugin_docs(&self, plugin_docs: &[Value], now_ms: i64) {
        if plugin_docs.is_empty() {
            return;
        }
        let items: Vec<PluginDocSyncItem> = plugin_docs
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        if items.is_empty() {
            return;
        }
        let mut storage = self.storage.clone();
        if let Err(e) = apply_plugin_doc_sync_items(
            &mut storage,
            &items,
            |domain, collection| self.make_collection(domain, collection),
            now_ms,
        ) {
            self.warn(format!("apply plugin docs failed: {e}"));
        }
    }

    // ------------------------------------------------------------------
    // keepalive 组织保活（p2p-node.ts:379-445 `maintainOrganizationNetwork`）
    // ------------------------------------------------------------------

    /// 单个 keepalive tick 的组织层保活：候选拨号 → 反熵拉取 → 补副本 →
    /// recovery 触发（覆盖网维护已在 p2p 事件循环内完成）。
    pub(crate) async fn maintain_org_tick(&self) {
        let Some(root_id) = self.root_id() else {
            return;
        };
        let now = self.now();
        let candidates = collect_org_peer_candidates(&self.storage, &root_id);
        if candidates.is_empty() {
            // 无任何已知成员地址：全员不可达更重形态，仍尝试定向恢复
            self.maybe_run_org_recovery(true, &root_id).await;
            return;
        }

        let connected: HashSet<String> = self
            .node
            .local_node_info()
            .await
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();
        let sorted = {
            let mut storage = self.storage.clone();
            let mut store = PeerActivityStore::new(&mut storage);
            store
                .sort_candidates_by_priority(&candidates, now)
                .unwrap_or_else(|_| candidates.clone())
        };

        // 1) 候选拨号：每 tick 最多新拨 3 个（node.connect_peer 内部已记账
        //    活跃度 success/failure）
        let (to_dial, mut connected_candidates) =
            plan_organization_dials(&sorted, &connected, DIAL_BUDGET_PER_TICK);
        for candidate in to_dial {
            if self.node.connect_peer(&candidate).await.is_ok() {
                connected_candidates.push(candidate);
            }
        }

        // 2) 反熵拉取：最多 2 个已连接候选（捎带自签 claim）
        for candidate in connected_candidates.iter().take(PULL_CANDIDATES_PER_TICK) {
            if let Err(e) = self.reconcile_from_peer(candidate, true).await {
                self.warn(format!(
                    "[p2p][keepalive] pull from candidate failed: peerId={:?}, error={e}",
                    candidate.peer_id
                ));
            }
        }

        // 3) 管理员补副本
        self.replenish_replicas(&root_id).await;

        // 4) 失联 recovery
        self.maybe_run_org_recovery(connected_candidates.is_empty(), &root_id)
            .await;
    }

    /// 管理员补副本（p2p-node.ts:520-573 `replenishOrganizationReplicas`）：
    /// 副本不足 K 时向未同步成员推送快照（每组织最多 2 个）。
    async fn replenish_replicas(&self, root_id: &str) {
        let now = self.now();
        let records = match OrganizationService::read_all_organizations(&self.storage) {
            Ok(records) => records,
            Err(e) => {
                self.warn(format!("replenish replicas: read orgs failed: {e}"));
                return;
            }
        };
        for record in records {
            if !record.is_admin(root_id) {
                continue;
            }
            let versions = record
                .sync
                .as_ref()
                .map(|s| s.versions)
                .or_else(|| Some(resolve_local_versions(&record)));
            let storage = self.storage.clone();
            let overview = compute_org_sync_overview(
                &record.org_id,
                &record.members,
                Some(root_id),
                versions.as_ref(),
                |peer_id| {
                    storage
                        .get(&org_sync_state_key(peer_id, &record.org_id))
                        .ok()
                        .flatten()
                        .and_then(|raw| OrgSyncState::from_json(&raw))
                },
                now,
            );
            if overview.is_replica_sufficient() {
                continue;
            }
            let mut pushed_for_org = 0;
            for member in &overview.members {
                if pushed_for_org >= REPLICA_PUSH_PER_ORG {
                    break;
                }
                if member.is_self || member.ever_synced {
                    continue;
                }
                let node_info = record
                    .find_member(&member.root_id)
                    .and_then(|m| m.node_info.clone())
                    .filter(|info| {
                        info.peer_id.as_deref().is_some_and(|p| !p.trim().is_empty())
                            || !info.addresses.is_empty()
                    });
                let Some(info) = node_info else {
                    continue;
                };
                let peer = PeerNodeInfo {
                    peer_id: info.peer_id,
                    addresses: info.addresses,
                };
                if self
                    .sync_org_to_member(&peer, &member.root_id, &record.org_id)
                    .await
                    .is_ok()
                {
                    pushed_for_org += 1;
                }
            }
        }
    }

    /// 失联恢复（p2p-node.ts:453-504 `maybeRunOrgRecovery`）：全员不可达连续
    /// 3 tick 且冷却过后，按恢复 token 向覆盖网邻居查询，命中候选只拨号。
    async fn maybe_run_org_recovery(&self, org_unreachable: bool, root_id: &str) {
        let now = self.now();
        let should_query = self
            .recovery_trigger
            .lock()
            .unwrap()
            .on_tick(org_unreachable, now);
        if !should_query {
            return;
        }
        let view = {
            let mut storage = self.storage.clone();
            OrganizationService::get_recovery_view(&mut storage, root_id, now).unwrap_or_default()
        };
        let neighbors: Vec<String> = self
            .node
            .local_node_info()
            .await
            .map(|info| {
                let self_id = info.peer_id.unwrap_or_default();
                info.connected_peers
                    .into_iter()
                    .filter(|p| *p != self_id)
                    .take(RECOVERY_ORGS_PER_ROUND)
                    .collect()
            })
            .unwrap_or_default();
        if view.is_empty() || neighbors.is_empty() {
            // TS 此时未进入查询，冷却不计（lastRecoveryQueryAt 保持上一轮的值）
            self.recovery_trigger.lock().unwrap().reset_cooldown();
            return;
        }

        let mut dialed = Vec::new();
        for entry in view.iter().take(RECOVERY_ORGS_PER_ROUND) {
            let token = active_recovery_tokens(&entry.org_id, &entry.recovery_secret, now)
                .into_iter()
                .next()
                .unwrap_or_default();
            if token.is_empty() {
                continue;
            }
            let found = self
                .node
                .query_recovery(&token, neighbors.clone(), RECOVERY_QUERY_WANT)
                .await
                .unwrap_or_default();
            dialed.extend(found);
        }
        for candidate in plan_recovery_dials(&dialed, RECOVERY_DIAL_BUDGET) {
            // 提示类候选，拨不通静默跳过（p2p-node.ts:493-495）
            let _ = self.node.connect_peer(&candidate).await;
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
fn collect_org_peer_candidates(storage: &SledStorage, current_root_id: &str) -> Vec<PeerNodeInfo> {
    let records = OrganizationService::read_all_organizations(storage).unwrap_or_default();
    let mut by_peer: HashMap<String, PeerNodeInfo> = HashMap::new();
    let mut by_address: HashMap<String, PeerNodeInfo> = HashMap::new();
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
    }
    by_peer.into_values().chain(by_address.into_values()).collect()
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
fn resolve_push_target_root_id(record: &OrganizationRecord, node_info: &PeerNodeInfo) -> Option<String> {
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
