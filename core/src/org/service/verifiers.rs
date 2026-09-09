//! 组织策略/信任声明的存储侧接线（C1/C3 生产调用方）：
//!
//! - 创世策略记录（`org:genesis:`）入站校验：三重校验（orgId 自认证复算 +
//!   根签名验签 + orgAddress 互绑复算）+ 写一次不可变守卫；
//! - 验证人信任声明（`org:verifiers:`，credential §4）入站合入：
//!   [`merge_trust_decl`] 的存储背衬 [`OrgSigSetVerifyContext`]——策略链读
//!   `org:genesis:`/`org:policy:`，名册读 `org:meta`，锚根复算读
//!   `org:evi:anchor:`。
//!
//! 纯逻辑（五步验证链、LWW 裁决）在 [`crate::org::sigset`] 与
//! [`crate::credential`]；本文件只做存储读写与闭包供给。

use serde_json::Value;

use crate::credential::{MergeVerdict, RosterMember};
use crate::credential::{OrgSigSetVerifier, TrustDecl, merge_trust_decl, trust_decl_key};
use crate::storage::{ScanOptions, StorageBackend};

use super::super::Result;
use super::super::genesis::{
    GenesisPolicyRecord, PolicyVersion, genesis_org_id, org_genesis_key, verify_genesis_signature,
    verify_org_address_binding,
};
use super::super::sigset::OrgSigSetVerifyContext;
use super::OrganizationService;

/// 载入组织策略链（org-genesis §5）：创世记录（seq 0）+ 全部修订记录
/// （`org:policy:{orgId}:{seq}`，任意顺序——验证方按 prevPolicyHash 链接回溯）。
pub fn load_policy_chain<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Result<Vec<PolicyVersion>> {
    let mut chain = Vec::new();
    // 损坏记录跳过（验证方按链回溯时自然缺口拒绝，fail-closed）
    if let Some(raw) = storage.get(&org_genesis_key(org_id))?
        && let Ok(genesis) = serde_json::from_str::<GenesisPolicyRecord>(&raw)
    {
        chain.push(PolicyVersion::Genesis(genesis));
    }
    let prefix = format!("org:policy:{org_id}:");
    for (_, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
        if let Ok(revision) = serde_json::from_str(&raw) {
            chain.push(PolicyVersion::Revision(revision));
        }
    }
    Ok(chain)
}

/// 存储背衬的 OrgSigSet 验证闭包对（org-signature §5 线上来源注入）：
///
/// - `anchor_matches`：扫本地 `org:evi:anchor:{orgId}:` 节点锚复算锚根
///   （sync-evidence §7）与声明比对；无本地锚 → false（拒绝，fail-closed）；
/// - `roster_lookup`：`org:meta` 当前名册投影为 RosterMember 列表
///   （锚时刻近似——本地未按锚时刻留存名册历史，如实标注；memberSetHash
///   复算仍对快照内容强约束）。
///
/// 闭包借 storage 捕获，返回给调用方持有，再以其引用构造
/// [`OrgSigSetVerifyContext`]（其字段为 `&dyn Fn`，生命周期要求闭包由
/// 调用方所有权持有）。
pub fn sigset_storage_closures<'a, S: StorageBackend>(
    storage: &'a S,
    org_id: &'a str,
) -> (
    impl Fn(&crate::credential::RosterAnchor) -> bool + use<'a, S>,
    impl Fn(&crate::credential::RosterAnchor) -> Option<Vec<RosterMember>> + use<'a, S>,
) {
    let anchor_matches = move |anchor: &crate::credential::RosterAnchor| {
        let prefix = format!("{}{}:", crate::evidence::EVIDENCE_ANCHOR_PREFIX, org_id);
        let Ok(items) = storage.scan(&ScanOptions::prefix(&prefix)) else {
            return false;
        };
        let records: Vec<crate::evidence::AnchorRecord> = items
            .into_iter()
            .filter_map(|(_, raw)| serde_json::from_str(&raw).ok())
            .collect();
        if records.is_empty() {
            return false;
        }
        crate::evidence::anchor_root(&records).as_deref() == Some(anchor.anchor_root.as_str())
    };
    let roster_lookup = move |anchor: &crate::credential::RosterAnchor| {
        let record = OrganizationService::get_record(storage, &anchor.org_id).ok()??;
        Some(
            record
                .members
                .iter()
                .map(|m| RosterMember {
                    identity: m.root_id.clone(),
                    role: m.role.as_str().to_string(),
                    // A16 双写：名册投影携带 org_user_id（未发布的成员为 None，
                    // 名册回查双键兼容不受影响）。
                    org_user_id: m.org_user_id(),
                })
                .collect(),
        )
    };
    (anchor_matches, roster_lookup)
}

/// 信任声明入站合入裁决。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustDeclMerge {
    /// 校验全过且版本更新——调用方落库。
    Accept,
    /// 校验全过但本地版本不旧——保留本地现状（调用方不写）。
    KeepCurrent,
    /// 结构/绑定/OrgSigSet 五步链任一失败——拒收（fail-closed）。
    Rejected,
}

/// 入站 trustDecl 合入校验（credential §4 生产调用点：orgsync-data 入站
/// `org:verifiers:` 键分支）。只裁决不落库——pmeta 记账归调用方（与邻键
/// 同口径）。
pub fn adjudicate_incoming_trust_decl<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    value: &Value,
) -> Result<TrustDeclMerge> {
    let Ok(incoming) = serde_json::from_value::<TrustDecl>(value.clone()) else {
        return Ok(TrustDeclMerge::Rejected);
    };
    if incoming.org_id != org_id {
        return Ok(TrustDeclMerge::Rejected);
    }
    let current: Option<TrustDecl> = storage
        .get(&trust_decl_key(org_id))?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let policies = load_policy_chain(storage, org_id)?;
    let (anchor_matches, roster_lookup) = sigset_storage_closures(storage, org_id);
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &anchor_matches,
        roster_lookup: &roster_lookup,
    };
    if !ctx.verify_org_sig_set(&incoming.sig_set) {
        return Ok(TrustDeclMerge::Rejected);
    }
    match merge_trust_decl(current.as_ref(), &incoming, &ctx) {
        Ok(MergeVerdict::Accept) => Ok(TrustDeclMerge::Accept),
        Ok(MergeVerdict::KeepCurrent) => Ok(TrustDeclMerge::KeepCurrent),
        Err(_) => Ok(TrustDeclMerge::Rejected),
    }
}

/// 入站创世策略记录校验（org-genesis §2.1 生产调用点：orgsync-data 入站
/// `org:genesis:` 键分支）。写一次不可变：本地已有且与 incoming 不一致 →
/// 保留本地（创世记录是 orgId 的自认证锚，LWW 不适用于它）。
///
/// 返回 `true` = 可落库（本地缺席，或本地已有且逐字节同义——幂等重放）。
pub fn admit_incoming_genesis<S: StorageBackend>(
    storage: &S,
    key: &str,
    value: &Value,
) -> Result<bool> {
    let Some(org_id) = key.strip_prefix(crate::org::genesis::ORG_GENESIS_PREFIX) else {
        return Ok(false);
    };
    let Ok(genesis) = serde_json::from_value::<GenesisPolicyRecord>(value.clone()) else {
        return Ok(false);
    };
    // 三重校验：orgId 自认证复算 + 根签名验签 + orgAddress 互绑复算
    if genesis_org_id(&genesis).ok().as_deref() != Some(org_id)
        || !verify_genesis_signature(&genesis)
        || !verify_org_address_binding(&genesis)
    {
        return Ok(false);
    }
    match storage.get(key)? {
        None => Ok(true),
        Some(raw) => {
            let local = serde_json::from_str::<GenesisPolicyRecord>(&raw).ok();
            Ok(local.as_ref() == Some(&genesis))
        }
    }
}
