//! 同人关联声明（credential §5，opt-in）：linkId、结构/签名校验与双向索引核对。
//!
//! 计票合并语义在事务规则/插件侧；本模块只保证声明可验证、留痕、opt-in。

use super::credential::identity_matches_public_key;
use super::credential::is_valid_org_id;
use super::credential::{canonical_sans, is_valid_hash, is_valid_identity_id, sha256_hex_str};
use super::error::{CredentialError, Result};
use super::types::{Credential, SamePersonLink};
use crate::identity::verify_ed25519_signature;

/// 声明类型标签（恒 `same-person`）。
pub const LINK_STATEMENT: &str = "same-person";

/// 关联声明签名载荷：`canonical(剔除 sig)`。
pub fn link_sign_payload(link: &SamePersonLink) -> Result<String> {
    canonical_sans(link, "sig")
}

/// `linkId = sha256hex(canonical 剔除 sig)`（credential §5；凭证侧 `linkRef` 回指它）。
pub fn link_id(link: &SamePersonLink) -> Result<String> {
    Ok(sha256_hex_str(&link_sign_payload(link)?))
}

/// 结构 + 签名校验：linkV/statement 常量、members ≥ 2、成员槽位形状、
/// issuer identity 绑定、签名有效。
///
/// 与凭证同口径（credential §2「不设有效期」）：验证路径不查 issuedAt——
/// 关联声明的失效路径 = 成员凭证注销，旧声明长期可验；签发时刻新鲜度只在
/// 签发入口 [`validate_link_issuance`] 校验。`_now_ms` 仅为保持调用方签名
/// 稳定而保留（新鲜度判定已迁出本函数）。
///
/// 「每个 credId 必须是该 issuer 已签发、未注销的凭证」（§5 验证链回查）不在
/// 本函数内——凭证持有与注销状态需外部数据，由调用方逐成员走
/// [`crate::credential::verify_credential_chain`] 后再用
/// [`verify_link_membership`] 核对归属与回指。
pub fn verify_same_person_link(link: &SamePersonLink, _now_ms: i64) -> Result<()> {
    if link.link_v != 1 {
        return Err(CredentialError::InvalidLink("linkV must be 1"));
    }
    if link.statement != LINK_STATEMENT {
        return Err(CredentialError::InvalidLink(
            "statement must be same-person",
        ));
    }
    if link.members.len() < 2 {
        return Err(CredentialError::InvalidLink("members must be >= 2"));
    }
    for member in &link.members {
        if !is_valid_identity_id(&member.holder_identity) || !is_valid_hash(&member.cred_id) {
            return Err(CredentialError::InvalidLink("member slot shape"));
        }
    }
    if !is_valid_identity_id(&link.issuer.identity)
        || !identity_matches_public_key(&link.issuer.identity, &link.issuer.public_key)
    {
        return Err(CredentialError::IdentityMismatch);
    }
    if !is_valid_org_id(&link.subject_domain) {
        return Err(CredentialError::InvalidLink(
            "subjectDomain orgId dual-form",
        ));
    }
    if !verify_ed25519_signature(
        &link_sign_payload(link)?,
        &link.sig,
        &link.issuer.public_key,
    ) {
        return Err(CredentialError::InvalidSignature);
    }
    Ok(())
}

/// 签发入口校验：结构 + 签名 + issuedAt 新鲜度（community 总约 ±10 min 门槛）。
///
/// 仅签发/受理新关联声明时使用；计票/消费验证不得调用本函数（旧声明必须
/// 长期可验——与 [`crate::credential::validate_credential_issuance`] 同分工）。
pub fn validate_link_issuance(link: &SamePersonLink, now_ms: i64) -> Result<()> {
    verify_same_person_link(link, now_ms)?;
    if !super::credential::is_fresh(link.issued_at, now_ms) {
        return Err(CredentialError::StaleTimestamp);
    }
    Ok(())
}

/// 双向索引核对（§5「声明与凭证双向可索引」）：`creds` 为成员槽位对应的凭证
/// 全文（调用方已逐条完成验证链含注销检查），逐成员核对——
/// credId 复算匹配、holderIdentity 匹配、凭证 issuer 即声明 issuer、
/// 凭证 `linkRef` 回指本声明 linkId。
pub fn verify_link_membership(link: &SamePersonLink, creds: &[&Credential]) -> Result<()> {
    let link_id = link_id(link)?;
    for member in &link.members {
        let cred = creds
            .iter()
            .find(|c| super::credential::credential_id(c).is_ok_and(|id| id == member.cred_id))
            .ok_or(CredentialError::InvalidLink("member credential missing"))?;
        if cred.holder.identity != member.holder_identity {
            return Err(CredentialError::InvalidLink("holderIdentity mismatch"));
        }
        if cred.issuer.identity != link.issuer.identity {
            return Err(CredentialError::InvalidLink(
                "member not issued by link issuer",
            ));
        }
        if cred.link_ref.as_deref() != Some(link_id.as_str()) {
            return Err(CredentialError::InvalidLink(
                "linkRef back-reference mismatch",
            ));
        }
    }
    Ok(())
}
