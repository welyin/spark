//! 懒连接链的「DHT 刷新」环节（connection-policy M6）：
//!
//! 原 `maybeRunOrgRecovery`（p2p-node.ts）是 tick 触发（连续 3 tick 全员不可达 +
//! 冷却后 DHT 查询 + 拨号）。M6 彻底懒连接后 **tick 不再触发**——组织成员连接只
//! 由懒拨号与被动入站建立，本地记录端点全不通即沉默。恢复的 **DHT 解析能力**保留，
//! 挪为懒连接链的「DHT 刷新」环节：本地记录端点全不通时，向组织私有 DHT 成员提示
//! （org.md §15）与恢复 token 查询（peer-rediscovery）刷新端点，命中候选只拨号，
//! 失败即沉默（等下一次懒拨号事件）。
//!
//! 触发点（事件驱动，非周期）：
//! - 登录一次性刷新（M2 已有节点存在记录查询路径）；
//! - 网络恢复事件（`redial_priority_peers` 既有路径）；
//! - org 写入推送 / orgsync-hello 懒拨号时，本地端点全不通走本环节。

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
    pub(super) async fn query_org_member_hints(&self, root_id: &str) -> Vec<PeerNodeInfo> {
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

    /// 懒连接链的「DHT 刷新 + 拨号」环节（M6，原 `maybe_run_org_recovery` 改造）：
    /// 本地记录的组织成员/网关端点全不通时调用——向组织私有 DHT 成员提示 + 恢复
    /// token 查询刷新端点，命中候选拨号（受预算约束），失败即沉默。
    ///
    /// 不再有「连续 3 tick + 冷却」门控：本环节由事件点（登录/网络恢复/org 写入
    /// 推送/orgsync-hello 懒拨号）触发，每次按需刷新，重复调用无碍（事件驱动非
    /// 周期，不算骚扰）。仅在实际发起查询时记录 `recovery_trigger.last_query_at`
    /// （供 UI 展示恢复状态）。
    pub(super) async fn refresh_org_endpoints_and_dial(&self, root_id: &str) {
        let now = self.now();
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
        // 组织私有 DHT 成员提示查询（§15）：命中提示只拨号，入池经节点侧宿主回调
        let mut dialed = self.query_org_member_hints(root_id).await;
        if view.is_empty() || neighbors.is_empty() {
            if dialed.is_empty() {
                // 无恢复视图/无邻居且 DHT 提示未命中：未发起有效查询，不记查询时间
                return;
            }
            // 无恢复视图/邻居但 DHT 提示命中：仅拨号
            self.recovery_trigger
                .lock()
                .unwrap()
                .note_query(now);
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
        self.recovery_trigger.lock().unwrap().note_query(now);
        for candidate in plan_recovery_dials(&dialed, RECOVERY_DIAL_BUDGET) {
            // 提示类候选，拨不通静默跳过
            let _ = self.node.connect_peer(&candidate).await;
        }
    }
}
