//! org-pull 反熵对账链路（org-pull-sync.ts `reconcileFromPeer`）：org-pull-list
//! （捎带自签 nodeInfoClaim）→ 逐组织双向 stale 比较 → 拉取合并 / 反推 / 删除。

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use super::{
    OrgReconcileStats, OrgSyncContext, PullBranch, list_local_related_orgs,
    resolve_push_target_root_id,
};
use crate::org::sync_state::sync_state_after_pull_synced;
use crate::org::types::organization_key;
use crate::org::{
    NodeInfoClaim, OrganizationNodeInfo, OrganizationRecord, OrganizationService,
    OrganizationSyncVersions, PluginDocSyncItem, PullOrgOutcome, apply_plugin_doc_sync_items,
    classify_pull_org_response, is_organization_sync_stale, parse_pull_list_organizations,
    resolve_local_versions, sign_node_info_claim,
};
use crate::p2p::direct::{build_pull_list_request, build_pull_org_request};
use crate::p2p::peer_targets::{PeerNodeInfo, extract_peer_id};
impl OrgSyncContext {
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
        self.reconcile_from_peer_with_dial_timeout(
            node_info,
            with_claim,
            Duration::from_secs(crate::p2p::constants::CONNECT_TIMEOUT_SECS),
        )
        .await
    }

    /// 同 `reconcile_from_peer`，但拨号超时由调用方指定：
    /// 手动 sync-now 路径用更短超时让不可达成员快速失败（对账语义不变）。
    pub(crate) async fn reconcile_from_peer_with_dial_timeout(
        &self,
        node_info: &PeerNodeInfo,
        with_claim: bool,
        dial_timeout: Duration,
    ) -> Result<OrgReconcileStats, String> {
        let mut stats = OrgReconcileStats::default();
        let Some(root_id) = self.root_id() else {
            return Ok(stats);
        };
        self.node
            .connect_peer_with_timeout(node_info, dial_timeout)
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
        let claim_value = claim.as_ref().and_then(|c| serde_json::to_value(c).ok());

        // 自设备目标判定（对端 peerId == 自 FriendRecord.peer.peerId）：
        // 影响 removed 分支语义——自设备空存储不触发本地删除，转反推补齐
        let is_self_target = extract_peer_id(node_info)
            .map(|pid| {
                let mut storage = self.storage.clone();
                crate::contact::ContactService::get_friend(&mut storage, &root_id)
                    .ok()
                    .flatten()
                    .and_then(|f| f.peer)
                    .map(|p| p.peer_id == pid)
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        let list_request =
            build_pull_list_request(&root_id, local_peer_id.as_deref(), claim_value.clone());
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

        let local_orgs =
            list_local_related_orgs(&self.storage, &root_id).map_err(|e| e.to_string())?;
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
                    match self
                        .pull_org_apply(
                            node_info,
                            &root_id,
                            local_peer_id.as_deref(),
                            &org_id,
                            claim_value.as_ref(),
                            is_self_target,
                            &mut stats,
                        )
                        .await
                    {
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
                    self.pull_org_apply(
                        node_info,
                        &root_id,
                        local_peer_id.as_deref(),
                        &org_id,
                        claim_value.as_ref(),
                        is_self_target,
                        &mut stats,
                    )
                    .await;
                }
                (None, Some(_)) => {
                    self.pull_org_apply(
                        node_info,
                        &root_id,
                        local_peer_id.as_deref(),
                        &org_id,
                        claim_value.as_ref(),
                        is_self_target,
                        &mut stats,
                    )
                    .await;
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
    ///
    /// `is_self_target`：对端是同一身份的自设备时置 true——此时对端"没有该
    /// 组织"只说明它还没同步到（如 QR 恢复的新设备），绝不能按 §9.4 删除
    /// 本地记录；按 Unavailable 返回让调用方反推，主动把组织推给自设备。
    async fn pull_org_apply(
        &self,
        node_info: &PeerNodeInfo,
        root_id: &str,
        local_peer_id: Option<&str>,
        org_id: &str,
        claim: Option<&Value>,
        is_self_target: bool,
        stats: &mut OrgReconcileStats,
    ) -> PullBranch {
        let request = build_pull_org_request(root_id, local_peer_id, org_id, claim.cloned());
        let response = self
            .node
            .org_pull_request(node_info, &request)
            .await
            .ok()
            .flatten();
        match classify_pull_org_response(response.as_ref()) {
            PullOrgOutcome::Removed => {
                if is_self_target {
                    // 自设备空存储不代表成员资格变化：不删除，转反推补齐
                    return PullBranch::Unavailable;
                }
                // org.md §9.4：removed 与"非成员"不可区分，据此删除本地记录
                let mut storage = self.storage.clone();
                match crate::storage::StorageBackend::delete(
                    &mut storage,
                    &organization_key(org_id),
                ) {
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
}
