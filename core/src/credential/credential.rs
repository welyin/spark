//! 资格凭证：canonical 载荷、credId、结构校验与验签（credential §2、§6 第 1–3 步）。
//!
//! 凭证不设有效期：资格变更由验证人注销承载（revocation.rs），故本文件不含任何
//! 「过期」判定；签发时刻新鲜度（[`validate_credential_issuance`]）只在签发入口
//! 校验，呈现验证（[`verify_credential_static`]）不查 issuedAt——旧凭证长期有效。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::error::{CredentialError, Result};
use super::revocation::verify_not_revoked;
use super::trust::issuer_trusted_at;
use super::types::{Credential, RevocationEntry, RevocationHead, TrustDecl};
use crate::evidence::normalize_object;
use crate::identity::verify_ed25519_signature;

/// 声明时间新鲜度窗口：±10 min（community 总约；nodeInfoClaim 先例）。
pub const FRESHNESS_WINDOW_MS: i64 = 10 * 60 * 1000;

/// 持有凭证本地存储键前缀（credential §2：`cred:held:{credId}`，本地键）。
pub const CRED_HELD_PREFIX: &str = "cred:held:";

/// 持有凭证本地存储键。
pub fn held_credential_key(cred_id: &str) -> String {
    format!("{CRED_HELD_PREFIX}{cred_id}")
}

/// `canonical(剔除 drop_key 的全部字段)`（community 总约签名载荷统一口径）。
pub(crate) fn canonical_sans<T: Serialize>(record: &T, drop_key: &str) -> Result<String> {
    let mut value = serde_json::to_value(record)?;
    if let Value::Object(map) = &mut value {
        map.shift_remove(drop_key);
    }
    Ok(normalize_object(&value))
}

/// sha256hex（UTF-8 字节摘要，64 字符小写 hex）。
pub(crate) fn sha256_hex_str(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// 由 base64 公钥派生 identity（`sha256hex(公钥原始字节)`，总约）；公钥形状
/// 非法（非 base64 / 非 32 字节）返回 None。
pub(crate) fn identity_from_public_key(public_key_b64: &str) -> Option<String> {
    let raw = B64.decode(public_key_b64).ok()?;
    if raw.len() != 32 {
        return None;
    }
    Some(hex::encode(Sha256::digest(&raw)))
}

/// `identity == sha256hex(base64decode(publicKey))` 自包含绑定校验（总约）。
pub(crate) fn identity_matches_public_key(identity: &str, public_key_b64: &str) -> bool {
    identity_from_public_key(public_key_b64).is_some_and(|derived| derived == identity)
}

/// 身份 id 形状：`^[0-9a-f]{64}$`（总约）。
pub(crate) fn is_valid_identity_id(identity: &str) -> bool {
    identity.len() == 64
        && identity
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// orgId 双形态（org-genesis §2）：legacy `org_<16hex>` / 创世哈希型 `org_<64hex>`。
/// 统一实现归 affair 模块（全仓单一口径，防双形态判定漂移），此处转调。
pub(crate) fn is_valid_org_id(org_id: &str) -> bool {
    crate::affair::is_valid_org_id(org_id)
}

/// credType 形状（credential §2）：`^[A-Za-z0-9_-]+(:[A-Za-z0-9_-]+)*$，≤64`。
pub(crate) fn is_valid_cred_type(cred_type: &str) -> bool {
    fn is_seg(seg: &str) -> bool {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
    cred_type.len() <= 64 && cred_type.split(':').all(is_seg)
}

/// 哈希形状：`^[0-9a-f]{64}$`（credId/entryHash/linkId/subject 等共用）。
pub(crate) fn is_valid_hash(hex_str: &str) -> bool {
    is_valid_identity_id(hex_str)
}

/// 新鲜度：`|declared - now_ms| <= FRESHNESS_WINDOW_MS`（`Math.abs` 口径，含未来）。
pub(crate) fn is_fresh(declared_at: i64, now_ms: i64) -> bool {
    (declared_at - now_ms).abs() <= FRESHNESS_WINDOW_MS
}

/// 签名载荷：`canonical(凭证剔除 sig)`。
pub fn credential_sign_payload(cred: &Credential) -> Result<String> {
    canonical_sans(cred, "sig")
}

/// `credId = sha256hex(normalizeObject(凭证剔除 sig))`（credential §2）。
pub fn credential_id(cred: &Credential) -> Result<String> {
    Ok(sha256_hex_str(&credential_sign_payload(cred)?))
}

/// 结构校验（§6 第 1 步）：字段常量/形状 + issuer/holder 的 identity 绑定。
///
/// serde 反序列化已保证字段类型；此处补齐取值约束。claims 只含结论字段的
/// 最小披露红线（禁止姓名/证件号）由验证插件与产品层执行，协议层不做词表判定。
pub fn validate_credential_structure(cred: &Credential) -> Result<()> {
    if cred.cred_v != 1 {
        return Err(CredentialError::InvalidStructure("credV must be 1"));
    }
    if !is_valid_cred_type(&cred.cred_type) {
        return Err(CredentialError::InvalidStructure("credType shape"));
    }
    if !is_valid_identity_id(&cred.issuer.identity)
        || !identity_matches_public_key(&cred.issuer.identity, &cred.issuer.public_key)
    {
        return Err(CredentialError::IdentityMismatch);
    }
    if !is_valid_identity_id(&cred.holder.identity)
        || !identity_matches_public_key(&cred.holder.identity, &cred.holder.public_key)
    {
        return Err(CredentialError::IdentityMismatch);
    }
    if !is_valid_org_id(&cred.subject_domain) {
        return Err(CredentialError::InvalidStructure(
            "subjectDomain orgId dual-form",
        ));
    }
    if cred.method.is_empty() {
        return Err(CredentialError::InvalidStructure("method required"));
    }
    if let Some(link_ref) = &cred.link_ref {
        if !is_valid_hash(link_ref) {
            return Err(CredentialError::InvalidStructure("linkRef hash shape"));
        }
    }
    Ok(())
}

/// 验签（§6 第 3 步）：issuer 私钥签名有效。
pub fn verify_credential_signature(cred: &Credential) -> Result<()> {
    let payload = credential_sign_payload(cred)?;
    if !verify_ed25519_signature(&payload, &cred.sig, &cred.issuer.public_key) {
        return Err(CredentialError::InvalidSignature);
    }
    Ok(())
}

/// 静态验证（§6 第 1–3 步）：结构 → credId 复算 → 验签。
///
/// 与信任/注销无关、永不随时间变化——「资格失效不抹除历史」的数据结构体现：
/// 历史操作的凭证核验走本函数；新操作资格才需要完整的
/// [`crate::credential::verify_credential_chain`]（含信任时间线与注销检查）。
///
/// `expected_cred_id`：外部引用值（注销条目/关联声明/holderProof 的 credId 槽位），
/// 给出时必须与复算值一致。
pub fn verify_credential_static(cred: &Credential, expected_cred_id: Option<&str>) -> Result<()> {
    validate_credential_structure(cred)?;
    let cred_id = credential_id(cred)?;
    if let Some(expected) = expected_cred_id {
        if expected != cred_id {
            return Err(CredentialError::CredIdMismatch);
        }
    }
    verify_credential_signature(cred)?;
    Ok(())
}

/// 签发入口校验：结构 + issuedAt 新鲜度（credential §2 ±10 min 门槛）。
///
/// 仅签发/受理新凭证时使用；呈现验证不得调用本函数（旧凭证必须长期可验）。
pub fn validate_credential_issuance(cred: &Credential, now_ms: i64) -> Result<()> {
    validate_credential_structure(cred)?;
    if !is_fresh(cred.issued_at, now_ms) {
        return Err(CredentialError::StaleTimestamp);
    }
    Ok(())
}

/// 注销证明视图：issuer 的头承诺 + `seq = 1..=headSeq` 全量条目（§3.2）。
pub struct RevocationView<'a> {
    /// 全量注销条目（链序）。
    pub entries: &'a [RevocationEntry],
    /// 头承诺。
    pub head: &'a RevocationHead,
}

/// 凭证呈现验证链（credential §6 第 1–5 步，fail-closed）：
/// 结构 → credId 复算 → 验签 → 信任匹配（按 issuedAt 时刻信任集，既往不咎）
/// → 注销检查（头承诺 + 全量链复算）。第 6 步持有者绑定由 read-gate 闭合
/// （[`crate::credential::verify_holder_proof`]）。
///
/// - `trust_decls`：subjectDomain 的全部已知信任声明版本（调用方按 orgId 预筛）；
/// - `revocation`：issuer 的注销证明；缺数据即失败（无头承诺不可证「未注销」）。
///   issuer 验签公钥取凭证 `issuer.publicKey`（已被凭证签名与 identity 绑定钉死）。
pub fn verify_credential_chain(
    cred: &Credential,
    trust_decls: &[&TrustDecl],
    revocation: Option<&RevocationView>,
    now_ms: i64,
) -> Result<String> {
    verify_credential_static(cred, None)?;
    let cred_id = credential_id(cred)?;
    if trust_decls.iter().any(|d| d.org_id != cred.subject_domain) {
        return Err(CredentialError::InvalidStructure(
            "trustDecl orgId pre-filter",
        ));
    }
    if !issuer_trusted_at(
        trust_decls,
        &cred.issuer.identity,
        &cred.cred_type,
        &cred.method,
        cred.issued_at,
    ) {
        return Err(CredentialError::IssuerNotTrusted);
    }
    let view = revocation.ok_or(CredentialError::RevocationUnavailable)?;
    verify_not_revoked(
        &cred_id,
        view.entries,
        view.head,
        &cred.issuer.public_key,
        now_ms,
    )?;
    Ok(cred_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cred_type_shape() {
        assert!(is_valid_cred_type("household-owner"));
        assert!(is_valid_cred_type("plugin:hoa-verify"));
        assert!(is_valid_cred_type("a:b:c"));
        assert!(!is_valid_cred_type(""));
        assert!(!is_valid_cred_type(":a"));
        assert!(!is_valid_cred_type("a:"));
        assert!(!is_valid_cred_type("a::b"));
        assert!(!is_valid_cred_type("a b"));
        assert!(!is_valid_cred_type(&"a".repeat(65)));
    }

    #[test]
    fn org_id_dual_form() {
        assert!(is_valid_org_id(&format!("org_{}", "a".repeat(16))));
        assert!(is_valid_org_id(&format!("org_{}", "a".repeat(64))));
        assert!(!is_valid_org_id(&format!("org_{}", "a".repeat(15))));
        assert!(!is_valid_org_id(&format!("org_{}", "A".repeat(16))));
        assert!(!is_valid_org_id(&"a".repeat(16)));
    }

    #[test]
    fn freshness_window_abs() {
        assert!(is_fresh(1000, 1000 + FRESHNESS_WINDOW_MS));
        assert!(is_fresh(1000 + FRESHNESS_WINDOW_MS, 1000));
        assert!(!is_fresh(1000, 1000 + FRESHNESS_WINDOW_MS + 1));
    }

    #[test]
    fn held_key_shape() {
        assert_eq!(held_credential_key("ab"), "cred:held:ab");
    }
}
