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
    AnchorRecord, EvidenceExportPackage, ExportScope, anchor_key, build_export_package,
    get_evidence_head, sign_anchor,
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

    /// 存证导出包（§9）：全量连续链 + 锚集 + 默克尔根 + proofs + 本机
    /// root 身份签名。返回 canonical 核验可复算的 serde Value（壳层序列化
    /// 落盘；canonical 口径见 export.rs）。
    pub fn evidence_export(&mut self, scope: ExportScope) -> Result<EvidenceExportPackage> {
        let seed = self
            .unlocked
            .as_ref()
            .map(|u| u.seed)
            .ok_or(KernelError::Locked)?;
        let identity = derive_root_identity(&seed);
        let package = build_export_package(
            self.require_storage()?,
            scope,
            &identity.signing_key,
            system_now_ms(),
        )
        .map_err(KernelError::Internal)?;
        Ok(package)
    }

    /// 组织锚记录列表（当前已知全部成员锚，按 nodeId 排序）——治理面采纳
    /// /呈现层查询入口（锚不验签入库，验签在消费点，§6 既定口径）。
    pub fn evidence_anchors(&self, org_id: &str) -> Result<Vec<AnchorRecord>> {
        crate::evidence::collect_anchors(self.require_storage()?, Some(org_id))
            .map_err(KernelError::Internal)
    }
}
