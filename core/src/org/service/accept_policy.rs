//! 准入策略声明的入站合入（membership §4.5 / A17；与 disclosure 同一模式）：
//!
//! - 发布键域 `org:accept:{orgId}`（org:structure@v1 内建集合，version 单调
//!   LWW）——发布即公示（随 orgsync 全员流动），生效由记录自带
//!   `effectiveAt` 门控（扩大方向 = updatedAt + 24h）；
//! - 入站合入 [`adjudicate_incoming_accept_policy`]：结构校验 → sigSet
//!   必须存在且 subject 绑定本记录 acceptPolicyHash（防搬签）→ OrgSigSet
//!   五步链（与 trustDecl/policyDoc/disclosure 同一存储闭包背衬）→
//!   version LWW 裁决。
//!
//! 免预录验证（org-join §8）只采信已生效记录——合入点的五步链是发布件
//! 进入同步键域的唯一闸门。

use serde_json::Value;

use crate::credential::OrgSigSetVerifier;
use crate::policy::{AcceptPolicyRecord, accept_policy_hash, accept_policy_key, validate_accept_policy};
use crate::storage::StorageBackend;

use super::super::Result;
use super::super::sigset::OrgSigSetVerifyContext;
use super::verifiers::{load_policy_chain, sigset_storage_closures};

/// 准入策略声明入站合入裁决（与 disclosure 同三态口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptPolicyMerge {
    /// 校验全过且版本更新——调用方落库。
    Accept,
    /// 校验全过但本地版本不旧（version LWW）——保留本地现状。
    KeepCurrent,
    /// 结构/sigSet 绑定/OrgSigSet 五步链任一失败——拒收（fail-closed）。
    Rejected,
}

/// 入站准入策略声明合入校验（orgsync-data 入站 `org:accept:` 键分支）。
/// 只裁决不落库——pmeta 记账归调用方（与邻键同口径）。
pub fn adjudicate_incoming_accept_policy<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    value: &Value,
) -> Result<AcceptPolicyMerge> {
    let Ok(incoming) = serde_json::from_value::<AcceptPolicyRecord>(value.clone()) else {
        return Ok(AcceptPolicyMerge::Rejected);
    };
    if incoming.org_id != org_id || validate_accept_policy(&incoming).is_err() {
        return Ok(AcceptPolicyMerge::Rejected);
    }
    // 发布件必须携带组织签名包，且 subject 绑定本记录哈希（防搬签）。
    let Some(sig_set) = &incoming.sig_set else {
        return Ok(AcceptPolicyMerge::Rejected);
    };
    let Ok(hash) = accept_policy_hash(&incoming) else {
        return Ok(AcceptPolicyMerge::Rejected);
    };
    if sig_set.subject != hash {
        return Ok(AcceptPolicyMerge::Rejected);
    }
    let policies = load_policy_chain(storage, org_id)?;
    let (anchor_matches, roster_lookup) = sigset_storage_closures(storage, org_id);
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &anchor_matches,
        roster_lookup: &roster_lookup,
    };
    if !ctx.verify_org_sig_set(sig_set) {
        return Ok(AcceptPolicyMerge::Rejected);
    }
    // version LWW：高版本胜；同版本保留本地现状（确定性收敛，与 disclosure 同口径）。
    let local: Option<AcceptPolicyRecord> = storage
        .get(&accept_policy_key(org_id))?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match local {
        Some(local) if local.version >= incoming.version => Ok(AcceptPolicyMerge::KeepCurrent),
        _ => Ok(AcceptPolicyMerge::Accept),
    }
}
