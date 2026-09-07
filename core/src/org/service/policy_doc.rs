//! B1 策略文档的发布承载（policy §2 sigSet 合入校验归 C3 的落地）：
//!
//! - 发布键域 `org:policydoc:{orgId}`（org:structure@v1 内建集合，单记录
//!   LWW by updatedAt）——与本地草稿键 `policy:draft:`（不进同步流量）
//!   分立：草稿是编辑工作副本，发布件是经组织签名包背书的生效面；
//! - 入站合入 [`adjudicate_incoming_policy_doc`]：结构校验 → sigSet 必须
//!   存在且 subject 绑定本文档 policyDocHash（防搬签）→ OrgSigSet 五步链
//!   （与 trustDecl 同一存储闭包背衬）→ updatedAt LWW 裁决。
//!
//! 求值入口（read-gate §4 第 5 步）不验签——文档按 policyDocHash 自认证；
//! 合入点的五步链是发布件进入同步键域的唯一闸门。

use serde_json::Value;

use crate::credential::OrgSigSetVerifier;
use crate::policy::{PolicyDoc, policy_doc_hash, validate_policy_doc};
use crate::storage::StorageBackend;

use super::super::Result;
use super::super::sigset::OrgSigSetVerifyContext;
use super::verifiers::{load_policy_chain, sigset_storage_closures};

/// 发布策略文档存储键前缀（org:structure@v1 键域，`org:policydoc:{orgId}`）。
pub const POLICY_DOC_PREFIX: &str = "org:policydoc:";

/// 发布策略文档存储键。
pub fn policy_doc_key(org_id: &str) -> String {
    format!("{POLICY_DOC_PREFIX}{org_id}")
}

/// 策略文档入站合入裁决（与 trustDecl 同三态口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyDocMerge {
    /// 校验全过且更新——调用方落库。
    Accept,
    /// 校验全过但本地版本不旧（updatedAt LWW）——保留本地现状。
    KeepCurrent,
    /// 结构/sigSet 绑定/OrgSigSet 五步链任一失败——拒收（fail-closed）。
    Rejected,
}

/// 入站发布策略文档合入校验（orgsync-data 入站 `org:policydoc:` 键分支）。
/// 只裁决不落库——pmeta 记账归调用方（与邻键同口径）。
pub fn adjudicate_incoming_policy_doc<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    value: &Value,
) -> Result<PolicyDocMerge> {
    let Ok(incoming) = serde_json::from_value::<PolicyDoc>(value.clone()) else {
        return Ok(PolicyDocMerge::Rejected);
    };
    if incoming.org_id != org_id || validate_policy_doc(&incoming).is_err() {
        return Ok(PolicyDocMerge::Rejected);
    }
    // 发布件必须携带组织签名包，且 subject 绑定本文档哈希（防搬签）。
    let Some(sig_set) = &incoming.sig_set else {
        return Ok(PolicyDocMerge::Rejected);
    };
    let Ok(doc_hash) = policy_doc_hash(&incoming) else {
        return Ok(PolicyDocMerge::Rejected);
    };
    if sig_set.subject != doc_hash {
        return Ok(PolicyDocMerge::Rejected);
    }
    let policies = load_policy_chain(storage, org_id)?;
    let (anchor_matches, roster_lookup) = sigset_storage_closures(storage, org_id);
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &anchor_matches,
        roster_lookup: &roster_lookup,
    };
    if !ctx.verify_org_sig_set(sig_set) {
        return Ok(PolicyDocMerge::Rejected);
    }
    // updatedAt LWW：新者胜；同刻保留本地现状（确定性收敛，与 trustDecl 同口径）。
    let local: Option<PolicyDoc> = storage
        .get(&policy_doc_key(org_id))?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match local {
        Some(local) if local.updated_at >= incoming.updated_at => Ok(PolicyDocMerge::KeepCurrent),
        _ => Ok(PolicyDocMerge::Accept),
    }
}
