//! 存证锚定与导出编排（阶段四F，evidence-anchoring-export §1–§2）：
//! - 锚定触发：链头变化时刷新（doc 写入挂钩 + p2p 启动兜底）+ 治理事件
//!   驱动的显式 API；锚 = 本机链头承诺（`org:evi:anchor:{orgId}:{nodeId}`
//!   LWW 自覆盖，orgsync org:structure@v1 键域全员流动）；
//! - 导出：自包含存证包构建（§9），签名材料 = 本机 root 身份。
//!
//! 锚签名用 root 身份（锚是节点级声明；双层身份红线不适用于组织内
//! orgsync 面——成员表本身就是 rootId 维度）。

use super::{Kernel, KernelError, Result};
use crate::evidence::{
    AnchorRecord, EvidenceExportPackage, EvidenceOp, ExportScope, NewEvidenceEntry, RosterAnchorRef,
    RosterMember, RosterSection, anchor_key, anchor_root, append_evidence,
    build_evidence_payload_hash, build_export_package, collect_anchors, get_evidence_entry,
    get_evidence_head, inclusion_proof, roster_commitment_payload, sign_anchor,
    ROSTER_ENTRY_COLLECTION,
};
use crate::identity::derive_root_identity;
use crate::org::OrganizationService;
use crate::p2p::node::system_now_ms;
use crate::storage::StorageBackend;

impl Kernel {
    /// 显式锚定（治理事件驱动入口，设计 §1.2 触发①；治理插件经本 API 在
    /// 事务容器关闭/法定人数快照时锚一次）。返回是否写入新锚——链头与
    /// 在册锚一致（含空链）→ `Ok(false)` 幂等跳过。
    pub fn evidence_anchor(&mut self, org_id: &str) -> Result<bool> {
        let seed = self
            .unlocked
            .as_ref()
            .map(|u| u.seed)
            .ok_or(KernelError::Locked)?;
        let node_id = self.sync_node_id();
        let Some(head) = get_evidence_head(self.require_storage()?)? else {
            return Ok(false); // 空链不锚
        };
        let key = anchor_key(org_id, &node_id);
        let storage = self.require_storage_mut()?;
        let stale = storage
            .get(&key)?
            .and_then(|raw| serde_json::from_str::<AnchorRecord>(&raw).ok())
            .is_none_or(|a| a.head_seq != head.seq || a.head_hash != head.hash);
        if !stale {
            return Ok(false);
        }
        let identity = derive_root_identity(&seed);
        let anchor = sign_anchor(
            &identity.signing_key,
            org_id,
            &node_id,
            head.seq,
            &head.hash,
            system_now_ms(),
        );
        // 经版本化句柄写：自动 pmeta 记账 → 随 orgsync org:structure@v1 全员
        // 流动（键域纳管见 sync/orgsync/builtin.rs + sync/versioned.rs）
        storage.put(&key, &serde_json::to_string(&anchor)?)?;
        Ok(true)
    }

    /// 链头变化刷新：本机为成员的全部组织逐个幂等锚定。返回新锚个数；
    /// 身份锁定/无存储 → 0（静默跳过，尽力而为）。
    pub(crate) fn refresh_evidence_anchors(&mut self) -> usize {
        let Ok(Some(root_id)) = self.current_root_id() else {
            return 0;
        };
        let org_ids: Vec<String> = self
            .storage
            .as_ref()
            .and_then(|s| OrganizationService::read_all_organizations(s.raw()).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.find_member(&root_id).is_some())
            .map(|r| r.org_id)
            .collect();
        let mut written = 0;
        for org_id in org_ids {
            match self.evidence_anchor(&org_id) {
                Ok(true) => written += 1,
                Ok(false) => {}
                Err(e) => {
                    log::warn!("[EVIDENCE] anchor refresh failed | org={org_id} err={e}");
                }
            }
        }
        written
    }

    /// 存证导出包（§9 + evidence §4.1）：全量连续链 + 锚集 + 默克尔根 +
    /// proofs + roster 段（scope 声明 orgId 且本机持有该组织时）+ 本机
    /// root 身份签名。返回 canonical 核验可复算的 serde Value（壳层序列化
    /// 落盘；canonical 口径见 export.rs）。
    pub fn evidence_export(&mut self, scope: ExportScope) -> Result<EvidenceExportPackage> {
        let seed = self
            .unlocked
            .as_ref()
            .map(|u| u.seed)
            .ok_or(KernelError::Locked)?;
        let identity = derive_root_identity(&seed);
        let roster = self.build_roster_section(&scope)?;
        let package = build_export_package(
            self.require_storage()?,
            scope,
            &identity.signing_key,
            system_now_ms(),
            roster,
        )
        .map_err(KernelError::Internal)?;
        Ok(package)
    }

    /// roster 段编排（evidence §4.1 构建路径）：本地 `OrganizationRecord.
    /// members`（identity + role）→ `member_set_hash` 承诺 → **名册承诺须先
    /// 在链上**（payloadHash 幂等扫描，未在链先追加 roster 存证条目——承诺
    /// 载荷无 ts，同名册重复导出不重写）→ 触发锚定（「先写条目后锚」，与
    /// affairs evi:resolution 同路径）→ 覆盖锚（本机锚记录）的组织锚根
    /// inclusion proof。scope 未声明 orgId / 本机不持有该组织 → None（v1 包）。
    fn build_roster_section(&mut self, scope: &ExportScope) -> Result<Option<RosterSection>> {
        let Some(org_id) = scope.org_id.clone() else {
            return Ok(None);
        };
        let Some(record) = OrganizationService::get_record(self.require_storage()?, &org_id)?
        else {
            return Ok(None);
        };
        let snapshot: Vec<RosterMember> = record
            .members
            .iter()
            .map(|m| RosterMember {
                // identity 保持 rootId（② 层签名回查的零依赖锚点，见
                // export.rs RosterMember 注释）；A16 双写：orgUserId additive
                // 携带（未发布成员为 None）。
                identity: m.root_id.clone(),
                role: m.role.as_str().to_string(),
                org_user_id: m.org_user_id(),
            })
            .collect();
        let values: Vec<serde_json::Value> = snapshot
            .iter()
            .map(|m| serde_json::json!({ "identity": m.identity, "role": m.role }))
            .collect();
        let member_set_hash = crate::affair::snapshot::member_set_hash(&values)
            .map_err(|e| KernelError::Internal(format!("名册承诺构建失败: {e}")))?;

        // 名册承诺须在链上：按 payloadHash 幂等扫描（承诺载荷无 ts）
        let payload = roster_commitment_payload(&org_id, &member_set_hash);
        let payload_hash = build_evidence_payload_hash(Some(&payload));
        let mut on_chain = false;
        if let Some(head) = get_evidence_head(self.require_storage()?)? {
            for seq in 1..=head.seq {
                if let Some(entry) = get_evidence_entry(self.require_storage()?, seq)?
                    && entry.domain == org_id
                    && entry.collection == ROSTER_ENTRY_COLLECTION
                    && entry.payload_hash == payload_hash
                {
                    on_chain = true;
                    break;
                }
            }
        }
        let node_id = self.sync_node_id();
        if !on_chain {
            append_evidence(
                self.require_storage_raw_mut()?,
                NewEvidenceEntry::from_parts(
                    &org_id,
                    ROSTER_ENTRY_COLLECTION,
                    &org_id,
                    EvidenceOp::Put,
                    Some(&payload),
                    None,
                    system_now_ms(),
                    &node_id,
                ),
            )?;
        }
        // 后锚（幂等：链头未变时 evidence_anchor 内部跳过）
        self.evidence_anchor(&org_id)?;

        let anchors = collect_anchors(self.require_storage()?, Some(&org_id))
            .map_err(KernelError::Internal)?;
        let own = anchors
            .iter()
            .find(|a| a.node_id == node_id)
            .ok_or_else(|| KernelError::Internal("锚定后找不到本机锚记录".to_string()))?;
        let root = anchor_root(&anchors)
            .ok_or_else(|| KernelError::Internal("锚集为空无法生成 anchorRoot".to_string()))?;
        let proof = inclusion_proof(&anchors, &node_id)
            .ok_or_else(|| KernelError::Internal("本机锚 inclusion proof 生成失败".to_string()))?;
        Ok(Some(RosterSection {
            member_set_hash,
            anchor: RosterAnchorRef {
                org_id,
                anchor_root: root,
                ts: own.ts,
            },
            snapshot,
            anchor_proof: proof,
        }))
    }

    /// 组织锚记录列表（当前已知全部成员锚，按 nodeId 排序）——治理面采纳
    /// /呈现层查询入口（锚不验签入库，验签在消费点，§6 既定口径）。
    pub fn evidence_anchors(&self, org_id: &str) -> Result<Vec<AnchorRecord>> {
        crate::evidence::collect_anchors(self.require_storage()?, Some(org_id))
            .map_err(KernelError::Internal)
    }
}
