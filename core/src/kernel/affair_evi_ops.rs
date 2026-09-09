//! evi:resolution 存证条目与效力相关组织反查（affair.md §6.3；
//! docs/architecture/affairs/affair-model.md §4.1，A21）。
//!
//! - **写入拓扑**：生效决议（§6.2 生效判定通过，非「待确认」态）由每个求值
//!   到该生效判定的节点向**本机**存证链确定性自写自锚——写入点 = 组织效力
//!   钩子消费编排（`affair_apply_org_effects`，与 effect.rs 治理钩子同点），
//!   先写条目 → 再触发锚（`evidence_anchor` 幂等，链头承诺自然覆盖新条目）；
//!   条目不流动、锚经 orgsync 全员流动（存证链本地性，sync-evidence §6）；
//! - **幂等**：条目输入全确定性（决议记录 + 声明面 + 锚时刻），本机链已存在
//!   同 (domain=orgId, collection="resolution", id=决议 opHash) 条目 → 不重写；
//! - **效力相关组织反查**（声明面）：扫 `org:effectgrant:` 键域取现行非撤销
//!   声明的组织集合——无声明组织效力的事务（纯讨论）不写此条目。

use serde_json::{Value, json};

use super::{Kernel, KernelError, Result};
use crate::affair::{
    EFFECT_GRANT_PREFIX, RESOLUTION_ENTRY_COLLECTION, is_valid_identity_id, parse_effect_grant,
    resolution_entry_payload,
};
use crate::evidence::{
    EvidenceOp, NewEvidenceEntry, append_evidence, get_evidence_entry, get_evidence_height,
};
use crate::storage::{ScanOptions, StorageBackend};

/// 一条待封存的生效决议（`affair_apply_org_effects` 从 Apply 行集装配）。
pub(crate) struct ResolutionSeal {
    /// 决议 id（resolution 操作的 opHash）。
    pub(crate) resolution_op_hash: String,
    /// 决议 §6.1 payload 原文（conclusionHash 输入）。
    pub(crate) payload: Value,
    /// 决议操作 actor.orgSig 原样（kind=person → None → 载荷落 null）。
    pub(crate) sig_set: Option<Value>,
    /// 生效判定所依据的存证锚时刻（决议在本副本链上的锚定时刻）。
    pub(crate) effective_ts: i64,
}

impl Kernel {
    /// 先写条目后锚（affair.md §6.3 锚定接线）：对本组织每条生效决议向本机
    /// 链确定性自写 evi:resolution 条目（已在链 → 幂等跳过），随后触发锚
    /// （`evidence_anchor` 幂等）。返回逐条动作（written / already-on-chain）。
    pub(crate) fn seal_resolution_entries(
        &mut self,
        org_id: &str,
        affair_id: &str,
        now_ms: i64,
        items: &[ResolutionSeal],
    ) -> Result<Vec<Value>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let node_id = self.sync_node_id();
        let mut out = Vec::new();
        for item in items {
            let payload = resolution_entry_payload(
                affair_id,
                &item.resolution_op_hash,
                &item.payload,
                item.sig_set.as_ref(),
                item.effective_ts,
            );
            let action =
                if self.resolution_entry_on_chain(org_id, &item.resolution_op_hash)? {
                    "already-on-chain"
                } else {
                    append_evidence(
                        self.require_storage_raw_mut()?,
                        NewEvidenceEntry::from_parts(
                            org_id,
                            RESOLUTION_ENTRY_COLLECTION,
                            &item.resolution_op_hash,
                            EvidenceOp::Put,
                            Some(&payload),
                            None,
                            now_ms,
                            &node_id,
                        ),
                    )?;
                    "written"
                };
            out.push(json!({
                "resolutionOpHash": item.resolution_op_hash,
                "action": action,
                "conclusionHash": payload["conclusionHash"],
            }));
        }
        // 后锚（幂等：链头未变时 evidence_anchor 内部跳过）
        self.evidence_anchor(org_id)?;
        Ok(out)
    }

    /// 条目是否已在本机链上（domain=orgId、collection=resolution、id=决议
    /// opHash）。O(链高) 顺序扫描，与 affair_anchor_map 同口径。
    fn resolution_entry_on_chain(&self, org_id: &str, resolution_op_hash: &str) -> Result<bool> {
        let storage = self.require_storage()?;
        let height = get_evidence_height(storage)?;
        for seq in 1..=height {
            let Some(entry) = get_evidence_entry(storage, seq)? else {
                continue;
            };
            if entry.domain == org_id
                && entry.collection == RESOLUTION_ENTRY_COLLECTION
                && entry.id == resolution_op_hash
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 效力相关组织反查（affair.md §6.3 声明面）：扫 `org:effectgrant:` 键域，
    /// 返回声明「本域成员/某项规则由该事务决议决定」的组织集合——现行非撤销、
    /// 键-文一致（`grant.key == 存储键`）的声明记录，字典序去重。纯讨论事务
    /// （无任何声明）→ 空集。
    pub fn affair_effect_orgs(&self, affair_id: &str) -> Result<Vec<String>> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let storage = self.require_storage()?;
        let mut orgs = std::collections::BTreeSet::new();
        for (key, raw) in storage.scan(&ScanOptions::prefix(EFFECT_GRANT_PREFIX))? {
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue; // 损坏记录跳过
            };
            let Ok(grant) = parse_effect_grant(&value) else {
                continue;
            };
            if grant.key != key || grant.revoked || grant.affair_id != affair_id {
                continue;
            }
            orgs.insert(grant.org_id);
        }
        Ok(orgs.into_iter().collect())
    }
}

#[cfg(test)]
#[path = "affair_evi_ops_tests.rs"]
mod tests;
