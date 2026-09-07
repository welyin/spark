//! Actor（操作者/签名主体）结构与校验（wiki/protocol/community/affair.md §2.2）。
//!
//! 防伪绑定同 nodeInfoClaim 口径：`identity == sha256hex(base64decode(publicKey))`
//! 自包含确认「签名者即该身份持有者」。`kind: org` 的组织表态必须携带 `orgSig`
//! （组织签名集合，org-signature.md §2）；本模块只做结构存在性检查，签名包
//! 五步验证链属组织签名实现（C3）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 身份 id 形状：`^[0-9a-f]{64}$`（community README 总约）。
pub fn is_valid_identity_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Actor 类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorKind {
    /// 个人参与者（上下文身份 / 公共身份，协议不区分线形）。
    Person,
    /// 组织表态（展示级立场，不爬阶梯、不表决、无票权）。
    Org,
}

/// 操作者/签名主体（§2.2）。`org_sig` 保持原始 JSON（组织签名包验证不在本层）。
#[derive(Clone, Debug, PartialEq)]
pub struct Actor {
    /// person / org。
    pub kind: ActorKind,
    /// 身份 id（64 hex 小写）。
    pub identity: String,
    /// 公钥 base64（原始 32 字节）。
    pub public_key: String,
    /// 组织签名集合原文（仅 kind == org 时必须存在）。
    pub org_sig: Option<Value>,
}

/// Actor 结构/绑定校验失败原因（reason 字符串稳定，golden vectors 可依赖）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// kind 非 person/org。
    UnknownKind,
    /// identity 不是 64 hex 小写。
    InvalidIdentity,
    /// publicKey base64 解码失败。
    InvalidPublicKey,
    /// `sha256hex(base64decode(publicKey)) != identity`。
    PublicKeyIdentityMismatch,
    /// kind == org 但未携带 orgSig。
    OrgSigRequired,
}

impl ActorReject {
    /// 稳定 reason 字符串。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed-actor",
            Self::UnknownKind => "unknown-actor-kind",
            Self::InvalidIdentity => "invalid-identity",
            Self::InvalidPublicKey => "invalid-public-key",
            Self::PublicKeyIdentityMismatch => "public-key-identity-mismatch",
            Self::OrgSigRequired => "org-sig-required",
        }
    }
}

/// 解析并校验 Actor：结构 → 身份形状 → 公钥-身份绑定 → org 必带 orgSig。
pub fn parse_actor(value: &Value) -> Result<Actor, ActorReject> {
    let obj = value.as_object().ok_or(ActorReject::Malformed)?;
    let kind = match obj.get("kind").and_then(Value::as_str) {
        Some("person") => ActorKind::Person,
        Some("org") => ActorKind::Org,
        Some(_) => return Err(ActorReject::UnknownKind),
        None => return Err(ActorReject::Malformed),
    };
    let identity = obj
        .get("identity")
        .and_then(Value::as_str)
        .ok_or(ActorReject::Malformed)?
        .to_string();
    if !is_valid_identity_id(&identity) {
        return Err(ActorReject::InvalidIdentity);
    }
    let public_key = obj
        .get("publicKey")
        .and_then(Value::as_str)
        .ok_or(ActorReject::Malformed)?
        .to_string();
    let public_key_bytes = B64
        .decode(public_key.as_bytes())
        .map_err(|_| ActorReject::InvalidPublicKey)?;
    if hex::encode(Sha256::digest(&public_key_bytes)) != identity {
        return Err(ActorReject::PublicKeyIdentityMismatch);
    }
    let org_sig = obj.get("orgSig").cloned();
    if kind == ActorKind::Org && org_sig.is_none() {
        return Err(ActorReject::OrgSigRequired);
    }
    Ok(Actor {
        kind,
        identity,
        public_key,
        org_sig,
    })
}

/// Ed25519 detached 验签（PureEd25519）：公钥/签名均 base64，载荷为 UTF-8 字符串。
/// 公钥须 32 字节、签名须 64 字节，任一不符即失败。
pub fn verify_actor_signature(actor: &Actor, payload: &str, sig_b64: &str) -> bool {
    let Ok(public_key_bytes) = B64.decode(actor.public_key.as_bytes()) else {
        return false;
    };
    let Ok(signature_bytes) = B64.decode(sig_b64.as_bytes()) else {
        return false;
    };
    let Ok(public_key_arr) = <[u8; 32]>::try_from(public_key_bytes.as_slice()) else {
        return false;
    };
    let Ok(signature_arr) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key_arr) else {
        return false;
    };
    verifying_key
        .verify(payload.as_bytes(), &Signature::from_bytes(&signature_arr))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_id_shape() {
        assert!(is_valid_identity_id(&"ab".repeat(32)));
        assert!(!is_valid_identity_id(&"AB".repeat(32)));
        assert!(!is_valid_identity_id(&"ab".repeat(31)));
        assert!(!is_valid_identity_id(&"gb".repeat(32)));
    }

    #[test]
    fn actor_binding() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let public_key = B64.encode(key.verifying_key().to_bytes());
        let identity = hex::encode(Sha256::digest(B64.decode(&public_key).unwrap()));
        let value = json!({"kind": "person", "identity": identity, "publicKey": public_key});
        let actor = parse_actor(&value).unwrap();
        assert_eq!(actor.kind, ActorKind::Person);

        let mut bad = value.clone();
        bad["identity"] = json!("00".repeat(32));
        assert_eq!(
            parse_actor(&bad),
            Err(ActorReject::PublicKeyIdentityMismatch)
        );

        let org_without_sig = json!({"kind": "org", "identity": identity, "publicKey": public_key});
        assert_eq!(
            parse_actor(&org_without_sig),
            Err(ActorReject::OrgSigRequired)
        );
    }
}
