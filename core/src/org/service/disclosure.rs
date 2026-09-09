//! 名册开放声明的入站合入（membership §4.3 / A15；与 policy_doc 同一模式）：
//!
//! - 发布键域 `org:disclosure:{orgId}:{targetDomain}`（org:structure@v1 内建
//!   集合，version 单调 LWW）——发布即公示（随 orgsync 全员流动），生效
//!   由记录自带 `effectiveAt` 门控（扩大方向 = updatedAt + 24h）；
//! - 入站合入 [`adjudicate_incoming_disclosure`]：结构校验 → sigSet 必须
//!   存在且 subject 绑定本记录 disclosureHash（防搬签）→ OrgSigSet 五步链
//!   （与 trustDecl/policyDoc 同一存储闭包背衬）→ version LWW 裁决。
//!
//! 求值入口（`eval_disclosure`）不验签——记录按 disclosureHash 自认证；
//! 合入点的五步链是发布件进入同步键域的唯一闸门。

use serde_json::Value;

use crate::credential::OrgSigSetVerifier;
use crate::policy::{DisclosureRecord, disclosure_hash, validate_disclosure};
use crate::storage::StorageBackend;

use super::super::Result;
use super::super::sigset::OrgSigSetVerifyContext;
use super::verifiers::{load_policy_chain, sigset_storage_closures};

/// 开放声明入站合入裁决（与 policyDoc 同三态口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisclosureMerge {
    /// 校验全过且版本更新——调用方落库。
    Accept,
    /// 校验全过但本地版本不旧（version LWW）——保留本地现状。
    KeepCurrent,
    /// 结构/sigSet 绑定/OrgSigSet 五步链任一失败——拒收（fail-closed）。
    Rejected,
}

/// 入站开放声明合入校验（orgsync-data 入站 `org:disclosure:` 键分支）。
/// 只裁决不落库——pmeta 记账归调用方（与邻键同口径）。
pub fn adjudicate_incoming_disclosure<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    target_domain: &str,
    value: &Value,
) -> Result<DisclosureMerge> {
    let Ok(incoming) = serde_json::from_value::<DisclosureRecord>(value.clone()) else {
        return Ok(DisclosureMerge::Rejected);
    };
    if incoming.org_id != org_id
        || incoming.target_domain != target_domain
        || validate_disclosure(&incoming).is_err()
    {
        return Ok(DisclosureMerge::Rejected);
    }
    // 发布件必须携带组织签名包，且 subject 绑定本记录哈希（防搬签）。
    let Some(sig_set) = &incoming.sig_set else {
        return Ok(DisclosureMerge::Rejected);
    };
    let Ok(hash) = disclosure_hash(&incoming) else {
        return Ok(DisclosureMerge::Rejected);
    };
    if sig_set.subject != hash {
        return Ok(DisclosureMerge::Rejected);
    }
    let policies = load_policy_chain(storage, org_id)?;
    let (anchor_matches, roster_lookup) = sigset_storage_closures(storage, org_id);
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &anchor_matches,
        roster_lookup: &roster_lookup,
    };
    if !ctx.verify_org_sig_set(sig_set) {
        return Ok(DisclosureMerge::Rejected);
    }
    // version LWW：高版本胜；同版本保留本地现状（确定性收敛，与 trustDecl 同口径）。
    let local: Option<DisclosureRecord> = storage
        .get(&crate::policy::disclosure_key(org_id, target_domain))?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match local {
        Some(local) if local.version >= incoming.version => Ok(DisclosureMerge::KeepCurrent),
        _ => Ok(DisclosureMerge::Accept),
    }
}
