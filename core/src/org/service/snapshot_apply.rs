//! 入站数据落库：对端快照合并（`applyIncomingSnapshot`）与 nodeInfoClaim
//! 地址回填（`applyNodeInfoClaim`）。
//!
//! 两条路径的共同口径：校验不通过/无变化一律静默跳过或幂等返回，不 bump
//! 版本；定向投递校验（targetRootId 匹配、本机在成员列表）在 p2p 层完成。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::super::Result;
use super::super::claim::{NodeInfoClaim, verify_node_info_claim};
use super::super::snapshot::{merge_organization_sync_snapshot, normalize_incoming_snapshot};
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{OrganizationRecord, normalize_optional_node_info};
use super::OrganizationService;

impl OrganizationService {
    /// `applyNodeInfoClaim`（service.ts:381-458，网络推送部分除外）。
    ///
    /// 落库三条件（org.md §14.5）：claim 校验通过 + 本机当前用户是该组织
    /// admin + 声明者是该组织成员；不满足的组织静默跳过。
    /// 与现有 nodeInfo 完全一致时不 bump 版本。返回落库的组织 id 列表。
    ///
    /// 前置闸说明：TS 入口侧 org-pull-list 只在 requesterRootId 是本地某组织
    /// 已知成员时才处理其 claim（org-pull-sync.ts:165-184）——该判定在 p2p 层。
    pub fn apply_node_info_claim<S: StorageBackend>(
        storage: &mut S,
        claim: &NodeInfoClaim,
        current_root_id: &str,
        remote_peer_id: Option<&str>,
        now_ms: i64,
    ) -> Result<Vec<String>> {
        if !verify_node_info_claim(claim, now_ms).is_ok() {
            return Ok(Vec::new());
        }
        // 防代填他人地址：连接层 peerId 与声明 peerId 必须一致
        if let (Some(remote), Some(claimed)) = (remote_peer_id, claim.node_info.peer_id.as_deref())
            && claimed != remote
        {
            return Ok(Vec::new());
        }
        let claim_root_id = claim.root_id.trim().to_lowercase();
        // 对齐 TS：归一化失败（如 peerId < 8 字符）按异常上抛
        let Some(claimed_node_info) = normalize_optional_node_info(Some(&claim.node_info))? else {
            return Ok(Vec::new());
        };

        let mut applied = Vec::new();
        let records = Self::read_all_organizations(storage)?;
        for record in records {
            if !record.is_admin(current_root_id) {
                continue;
            }
            let Some(member) = record.find_member(&claim_root_id) else {
                continue;
            };
            let unchanged = member.node_info.as_ref().and_then(|n| n.peer_id.as_deref())
                == claimed_node_info.peer_id.as_deref()
                && member
                    .node_info
                    .as_ref()
                    .map(|n| n.addresses.clone())
                    .unwrap_or_default()
                    == claimed_node_info.addresses;
            if unchanged {
                continue;
            }

            let mut updated = record.clone();
            for m in &mut updated.members {
                if m.root_id == claim_root_id {
                    m.node_info = Some(claimed_node_info.clone());
                }
            }
            updated.updated_at = now_ms;
            let previous_last_synced_at =
                updated.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);
            let transaction = append_organization_transaction(
                storage,
                OrganizationTransactionRecord {
                    tx_id: String::new(),
                    org_id: updated.org_id.clone(),
                    type_: OrganizationTransactionType::MemberUpdate,
                    created_at: now_ms,
                    actor_root_id: claim_root_id.clone(),
                    target_root_id: Some(claim_root_id.clone()),
                    summary: format!(
                        "成员节点地址自动回填 {}",
                        &claim_root_id[..8.min(claim_root_id.len())]
                    ),
                    payload: Some(
                        [
                            (
                                "nodeInfo".to_string(),
                                serde_json::to_value(&claimed_node_info)?,
                            ),
                            ("source".to_string(), Value::from("node-info-claim")),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                },
            )?;
            Self::rebuild_sync_after_mutation(
                &mut updated,
                previous_last_synced_at,
                transaction.created_at,
            );
            Self::save_record(storage, &updated)?;
            applied.push(updated.org_id.clone());
        }
        Ok(applied)
    }

    /// 接收侧快照落库（org.md §7.4：`normalizeIncomingSnapshot` → `merge` → 写
    /// `org:meta:<orgId>`）。接受两种线形（原始记录 / 重建快照）。
    ///
    /// 定向投递校验（targetRootId 匹配、本机在成员列表）在 p2p 层完成。
    pub fn apply_incoming_snapshot<S: StorageBackend>(
        storage: &mut S,
        organization: &Value,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let snapshot = normalize_incoming_snapshot(organization)?;
        let existing = Self::get_record(storage, &snapshot.org_id)?;
        let merged = merge_organization_sync_snapshot(existing.as_ref(), &snapshot, now_ms);
        Self::save_record(storage, &merged)?;
        Ok(merged)
    }
}
