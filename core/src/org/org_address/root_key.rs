//! 组织根密钥对（§15）：创建时生成；私钥加密存 extra，不进快照、不同步出本机。

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use rand::Rng;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::types::OrganizationRecord;

/// 根私钥密文的加密键派生域分隔符（实现期定案，§15 口径）。
const ORG_ROOT_SECRET_ENC_DOMAIN: &str = "spark:org-root-enc:";

/// 生成组织根 Ed25519 密钥对（与 root 身份、libp2p 节点密钥、pubsub 信封
/// 临时密钥均独立的第四条签名链，§19.1）。
pub fn generate_org_root_signing_key() -> SigningKey {
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

/// 根私钥密文的加密键：`sha256("spark:org-root-enc:" ‖ orgSecret)`（实现期定案
/// 口径）。密文字段为保留键从不离机（快照构建剔除 + 推送前剥除），orgSecret
/// 仅在成员同步链路内流动，二者不会同时到达非持钥节点。
fn org_root_secret_cipher_key(org_secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ORG_ROOT_SECRET_ENC_DOMAIN.as_bytes());
    hasher.update(org_secret.as_bytes());
    hasher.finalize().into()
}

/// 加密组织根私钥种子：`base64(nonce(12) ‖ aes-256-gcm(seed)(32+16))`。
pub fn seal_org_root_secret(signing_key: &SigningKey, org_secret: &str) -> String {
    let key = org_root_secret_cipher_key(org_secret);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("32-byte key is valid");
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = aes_gcm::aead::Nonce::<Aes256Gcm>::from(nonce_bytes);
    let sealed = cipher
        .encrypt(&nonce, signing_key.to_bytes().as_slice())
        .expect("aes-gcm encrypt of 32 bytes is infallible");
    let mut buf = nonce_bytes.to_vec();
    buf.extend_from_slice(&sealed);
    B64.encode(buf)
}

/// 解密组织根私钥（密文/orgSecret 不匹配返回 `None`）。
pub fn open_org_root_secret(sealed: &str, org_secret: &str) -> Option<SigningKey> {
    let raw = B64.decode(sealed.trim()).ok()?;
    if raw.len() != 12 + 32 + 16 {
        return None;
    }
    let key = org_root_secret_cipher_key(org_secret);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let nonce_arr: [u8; 12] = raw[..12].try_into().ok()?;
    let nonce = aes_gcm::aead::Nonce::<Aes256Gcm>::from(nonce_arr);
    let plain = cipher.decrypt(&nonce, raw[12..].as_ref()).ok()?;
    let seed: [u8; 32] = plain.try_into().ok()?;
    Some(SigningKey::from_bytes(&seed))
}

/// 从组织记录读取组织根私钥（extra `orgRootSecret` 密文 + `orgSecret` 派生
/// 加密键；缺一或解密失败返回 `None`——非持钥节点得到 `None`）。
pub fn org_root_signing_key(record: &OrganizationRecord) -> Option<SigningKey> {
    let sealed = record
        .extra
        .get(OrganizationRecord::ORG_ROOT_SECRET_KEY)?
        .as_str()?;
    open_org_root_secret(sealed, record.org_secret()?)
}

/// 上线前剥除根私钥密文键（§15：不同步出本机）。
///
/// org-share 推送发**原始 OrganizationRecord** 线形（extra 动态键平铺在顶层），
/// 必须在推送前剔除；快照重建路径（org-pull 响应 / 接收侧 normalize）经
/// `extract_metadata` 已剔除，本函数是推送侧的对称兜底。
pub fn strip_org_root_secret(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove(OrganizationRecord::ORG_ROOT_SECRET_KEY);
    }
}
