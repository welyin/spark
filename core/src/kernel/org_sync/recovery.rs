//! 失联恢复（p2p-node.ts `maybeRunOrgRecovery`）：全员不可达连续 3 tick 且
//! 冷却过后，按恢复 token 向覆盖网邻居查询 + 组织私有 DHT 成员提示查询
//! （org.md §15），命中候选只拨号。

use super::{OrgSyncContext, RECOVERY_DIAL_BUDGET, RECOVERY_ORGS_PER_ROUND};
use crate::org::gateway::{OrgMemberHint, org_members_dht_key};
use crate::org::{OrganizationService, active_recovery_tokens};
use crate::p2p::constants::RECOVERY_QUERY_WANT;
use crate::p2p::keepalive::plan_recovery_dials;
use crate::p2p::peer_targets::PeerNodeInfo;

impl OrgSyncContext {
    /// 组织私有 DHT 成员提示查询（§15）：向本机为成员且持有 orgSecret 的组织
    /// 派生 key 查记录，命中的 {peerId, addresses} 提示返回供拨号；入池由
    /// 节点命中时经 `on_org_member_hints` 宿主回调完成（未验证口径）。
    async fn query_org_member_hints(&self, root_id: &str) -> Vec<PeerNodeInfo> {
        let records =
            OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        let mut hints = Vec::new();
        for record in records
            .iter()
            .filter(|r| r.find_member(root_id).is_some())
            .take(RECOVERY_ORGS_PER_ROUND)
        {
            let Some(secret) = record.org_secret() else {
                continue;
            };
            let key = org_members_dht_key(secret);
            let Ok(Some(value)) = self.node.dht_get_record(key.as_bytes()).await else {
                continue;
            };
            if let Some(hint) = OrgMemberHint::from_record_value(&value) {
                hints.push(PeerNodeInfo {
                    peer_id: Some(hint.peer_id),
                    addresses: hint.addresses,
                });
            }
        }
        hints
    }

    /// 失联恢复（p2p-node.ts:453-504 `maybeRunOrgRecovery`）：全员不可达连续
    /// 3 tick 且冷却过后，按恢复 token 向覆盖网邻居查询，命中候选只拨号。
    pub(super) async fn maybe_run_org_recovery(&self, org_unreachable: bool, root_id: &str) {
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
            let node_id = self.node.peer_id().to_string();
            OrganizationService::get_recovery_view(&mut storage, root_id, now, &node_id)
                .unwrap_or_default()
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
        // 组织私有 DHT 成员提示查询（§15）：与 token 查询同一触发轮进行，
        // 命中提示只拨号，入池经节点侧的宿主回调完成
        let mut dialed = self.query_org_member_hints(root_id).await;
        if view.is_empty() || neighbors.is_empty() {
            if dialed.is_empty() {
                // TS 此时未进入查询，冷却不计（lastRecoveryQueryAt 保持上一轮的值）
                self.recovery_trigger.lock().unwrap().reset_cooldown();
                return;
            }
            // 无恢复视图/邻居但 DHT 提示命中：仅拨号（p2p-node.ts:493-495 口径）
            for candidate in plan_recovery_dials(&dialed, RECOVERY_DIAL_BUDGET) {
                let _ = self.node.connect_peer(&candidate).await;
            }
            return;
        }

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
