//! 组织副本概览（`getOrgSyncOverview`）：K 副本统计 + 网络状态扩展（连接数 /
//! 恢复状态 / 防抖判定，development_plan「组织网络状态 UI」）。

use std::collections::HashSet;

use super::{Kernel, Result};
use crate::org::sync_state::{OrgSyncState, org_sync_state_key};
use crate::org::{
    OrgNetworkStatusInput, OrgSyncOverview, OrganizationService,
    build_organization_sync_versions_default, compute_org_sync_overview, decide_org_network_status,
};
use crate::p2p::keepalive::RecoveryState;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_activity::PeerActivityStore;
use crate::storage::StorageBackend;

/// `Option<i64>` 取较大者（任一 `Some` 即保留）。
fn max_opt(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) => Some(x),
        (None, other) => other,
    }
}

impl Kernel {
    /// 组织 K 副本统计（`getOrgSyncOverview` 纯逻辑版）+ 网络状态扩展
    /// （连接数 / 恢复状态 / 防抖判定，development_plan「组织网络状态 UI」）。
    pub fn org_overview(&self, org_id: &str) -> Result<OrgSyncOverview> {
        let storage = self.require_storage()?;
        let record = OrganizationService::get_record(storage, org_id)?
            .ok_or(crate::org::OrgError::OrganizationNotFound)?;
        let current_root_id = self.current_root_id()?;
        let versions = record
            .sync
            .as_ref()
            .map(|sync| sync.versions)
            .or_else(|| Some(build_organization_sync_versions_default(&record)));
        let now = system_now_ms();
        let mut overview = compute_org_sync_overview(
            org_id,
            &record.members,
            current_root_id.as_deref(),
            versions.as_ref(),
            |peer_id| {
                storage
                    .get(&org_sync_state_key(peer_id, org_id))
                    .ok()
                    .flatten()
                    .and_then(|raw| OrgSyncState::from_json(&raw))
            },
            now,
        );
        self.fill_org_network_status(&mut overview, now);
        Ok(overview)
    }

    /// 填充副本概览的网络状态字段（org_overview 的扩展段）。
    ///
    /// 数据源：P2P 连接表（已连接成员数）、`PeerActivityStore`（最近连接/断开
    /// 时间）、`RecoveryTrigger`（恢复状态）、持久化 DHT 配置。无连接时长的
    /// 基准取 max(成员最近断开时间, p2p 启动时间)——节点刚启动时拨号尚未完成，
    /// 以启动时间兜底避免开机即误报「丢失」。
    fn fill_org_network_status(&self, overview: &mut OrgSyncOverview, now: i64) {
        let dht_mode = self.p2p_dht_mode().unwrap_or_default();
        let (p2p_running, total_connected_peers, connected_set) = match &self.p2p {
            Some(node) => match self.runtime.handle().block_on(node.local_node_info()) {
                Ok(info) => {
                    let total = info.connected_peers.len();
                    let set: HashSet<String> = info.connected_peers.into_iter().collect();
                    (true, total, set)
                }
                Err(_) => (true, 0, HashSet::new()),
            },
            None => (false, 0, HashSet::new()),
        };
        let connected_members = overview
            .members
            .iter()
            .filter(|member| !member.is_self)
            .filter(|member| {
                member
                    .peer_id
                    .as_deref()
                    .is_some_and(|peer_id| connected_set.contains(peer_id))
            })
            .count() as u32;

        // 成员活跃度记录：最近连接时间（展示用）与最近断开时间（防抖基准）
        let mut last_connected_at: Option<i64> = None;
        let mut last_gone_at: Option<i64> = None;
        {
            let mut storage = self.storage.clone();
            if let Some(storage) = storage.as_mut() {
                let mut activity = PeerActivityStore::new(storage);
                for member in &overview.members {
                    if member.is_self {
                        continue;
                    }
                    let Some(peer_id) = member.peer_id.as_deref() else {
                        continue;
                    };
                    let Ok(Some(record)) = activity.get(peer_id) else {
                        continue;
                    };
                    last_connected_at = max_opt(last_connected_at, record.last_connected_at);
                    if !connected_set.contains(peer_id) {
                        last_gone_at = max_opt(last_gone_at, record.last_connected_at);
                        last_gone_at = max_opt(last_gone_at, record.last_disconnected_at);
                    }
                }
            }
        }

        let unreachable_ms = if !p2p_running || connected_members > 0 {
            0
        } else {
            match max_opt(last_gone_at, self.p2p_started_at) {
                Some(basis) => (now - basis).max(0),
                None => 0,
            }
        };
        // 有成员连接时恢复状态无意义（trigger 会在下个 tick 自行清零）
        let recovery_state = if connected_members > 0 {
            RecoveryState::Idle
        } else {
            self.recovery_trigger.lock().unwrap().state(now)
        };

        overview.connected_peers = connected_members;
        overview.recovery_state = recovery_state;
        overview.last_connected_at = last_connected_at;
        overview.dht_mode = dht_mode;
        overview.status = decide_org_network_status(&OrgNetworkStatusInput {
            p2p_running,
            total_connected_peers,
            connected_peers: connected_members,
            replica_target: overview.replica_target,
            total_members: overview.total_members,
            recovery_state,
            unreachable_ms,
        });
    }
}
