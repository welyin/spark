//! 验证人信任声明 trustDecl：合入校验（LWW + OrgSigSet）与「既往不咎」
//! 时间线判定（credential §4、§6 第 4 步）。
//!
//! OrgSigSet 的五步验证链（org-signature §5）依赖组织策略/名册/锚状态，
//! 其实现归 C3（core/org）；本模块只定义线形（types.rs）与注入接口
//! [`OrgSigSetVerifier`]，并自持两项不依赖组织状态的绑定校验：
//! `sigSet.orgId == trustDecl.orgId`、`sigSet.subject == trustDecl 剔除 sigSet
//! 的哈希`（防跨包搬签）。

use super::credential::{
    canonical_sans, identity_matches_public_key, is_valid_cred_type, is_valid_hash,
    is_valid_identity_id, is_valid_org_id, sha256_hex_str,
};
use super::error::{CredentialError, Result};
use super::types::{OrgSigSet, TrustDecl};

/// 信任声明存储键前缀（credential §4：`org:verifiers:{orgId}`，org:structure@v1 键域）。
pub const TRUST_DECL_PREFIX: &str = "org:verifiers:";

/// 信任声明存储键。
pub fn trust_decl_key(org_id: &str) -> String {
    format!("{TRUST_DECL_PREFIX}{org_id}")
}

/// 组织签名验证接口（org-signature §5 五步验证链；C3 提供实现并注入）。
pub trait OrgSigSetVerifier {
    /// 五步全过返回 true；任何一步失败返回 false（含 legacy 降级是否接受，
    /// 由实现方按消费场景裁决——credential §4 合入校验要求完整通过）。
    fn verify_org_sig_set(&self, sig_set: &OrgSigSet) -> bool;
}

/// trustDecl 内容哈希（sigSet.subject 必须等于它）：`sha256hex(canonical(剔除 sigSet))`。
pub fn trust_decl_hash(decl: &TrustDecl) -> Result<String> {
    // 注意：剔除键是 serde 线形键名（camelCase），不是 Rust 字段名
    Ok(sha256_hex_str(&canonical_sans(decl, "sigSet")?))
}

/// 结构校验：字段常量/形状 + 各验证人 identity 绑定 + credType/method 形状。
///
/// `verifiers` 允许为空数组——「清空信任集」是合法的撤销全部信任的新版本。
pub fn validate_trust_decl_structure(decl: &TrustDecl) -> Result<()> {
    if decl.trust_v != 1 {
        return Err(CredentialError::InvalidStructure("trustV must be 1"));
    }
    if !is_valid_org_id(&decl.org_id) {
        return Err(CredentialError::InvalidStructure("orgId dual-form"));
    }
    for grant in &decl.verifiers {
        if !is_valid_identity_id(&grant.identity)
            || !identity_matches_public_key(&grant.identity, &grant.public_key)
        {
            return Err(CredentialError::IdentityMismatch);
        }
        if grant.cred_types.iter().any(|t| !is_valid_cred_type(t)) {
            return Err(CredentialError::InvalidStructure("credTypes entry shape"));
        }
        if grant.methods.iter().any(|m| m.is_empty()) {
            return Err(CredentialError::InvalidStructure("methods entry empty"));
        }
    }
    Ok(())
}

/// 合入裁决。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeVerdict {
    /// 接受 incoming，覆盖本地现状。
    Accept,
    /// 保留本地现状（incoming 版本更旧或同版不新）。
    KeepCurrent,
}

/// 合入校验（credential §4）：结构 → sigSet 绑定 → OrgSigSet 五步验证（注入）
/// → 逐版 LWW（`seq` 大者胜；同 `seq` 比 `updatedAt`，大者胜；仍并列则保留
/// 本地现状）。任何一步失败返回 Err，调用方保留本地现状。
pub fn merge_trust_decl(
    current: Option<&TrustDecl>,
    incoming: &TrustDecl,
    verifier: &dyn OrgSigSetVerifier,
) -> Result<MergeVerdict> {
    validate_trust_decl_structure(incoming)?;
    if incoming.sig_set.org_id != incoming.org_id {
        return Err(CredentialError::InvalidStructure("sigSet orgId mismatch"));
    }
    if !is_valid_hash(&incoming.sig_set.subject)
        || incoming.sig_set.subject != trust_decl_hash(incoming)?
    {
        return Err(CredentialError::SigSetSubjectMismatch);
    }
    if !verifier.verify_org_sig_set(&incoming.sig_set) {
        return Err(CredentialError::SigSetRejected);
    }
    if let Some(current) = current {
        let current_newer = current.seq > incoming.seq
            || (current.seq == incoming.seq && current.updated_at >= incoming.updated_at);
        if current_newer {
            return Ok(MergeVerdict::KeepCurrent);
        }
    }
    Ok(MergeVerdict::Accept)
}

/// 时间线取版（既往不咎，credential §3.2/§4）：`at_ms` 时刻生效的版本 =
/// `effectiveFrom <= at_ms` 中 effectiveFrom 最大者（并列取 seq 大者）。
///
/// `decls` 为同一 orgId 的全部已知版本；调用方负责按 orgId 预筛。
pub fn trust_decl_at<'a>(decls: &[&'a TrustDecl], at_ms: i64) -> Option<&'a TrustDecl> {
    decls
        .iter()
        .filter(|d| d.effective_from <= at_ms)
        .max_by_key(|d| (d.effective_from, d.seq))
        .copied()
}

/// 方法模式匹配：尾部 `*` 为前缀通配（`plugin:hoa-verify:*` 匹配
/// `plugin:hoa-verify:manual-property-cert`），其余精确相等。
fn method_matches(pattern: &str, method: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => method.starts_with(prefix),
        None => pattern == method,
    }
}

/// 单版判定：issuer 是否在本版信任集内且 credType/method 在其授权范围。
pub fn verifier_granted(decl: &TrustDecl, issuer: &str, cred_type: &str, method: &str) -> bool {
    decl.verifiers.iter().any(|grant| {
        grant.identity == issuer
            && grant.cred_types.iter().any(|t| t == cred_type)
            && grant.methods.iter().any(|m| method_matches(m, method))
    })
}

/// 签发时刻信任判定（§6 第 4 步，既往不咎）：按 `issued_at` 取生效版本后判定；
/// 无任何已生效版本 = 不信任（fail-closed）。
pub fn issuer_trusted_at(
    decls: &[&TrustDecl],
    issuer: &str,
    cred_type: &str,
    method: &str,
    issued_at: i64,
) -> bool {
    match trust_decl_at(decls, issued_at) {
        Some(decl) => verifier_granted(decl, issuer, cred_type, method),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_pattern_match() {
        assert!(method_matches(
            "plugin:hoa-verify:*",
            "plugin:hoa-verify:manual-property-cert"
        ));
        assert!(method_matches("plugin:hoa-verify:*", "plugin:hoa-verify:"));
        assert!(!method_matches("plugin:hoa-verify:*", "plugin:other:x"));
        assert!(method_matches(
            "plugin:hoa-verify:manual",
            "plugin:hoa-verify:manual"
        ));
        assert!(!method_matches(
            "plugin:hoa-verify:manual",
            "plugin:hoa-verify:manual2"
        ));
    }

    #[test]
    fn trust_decl_key_shape() {
        assert_eq!(trust_decl_key("org_ab"), "org:verifiers:org_ab");
    }
}
