//! dm_e2e AES-256-GCM 加解密原语（显式密钥路径）。
//!
//! 对齐 p2p-dm §19.1.1。本文件纯逻辑（AES-256-GCM + AAD 构造），**不碰密钥
//! 表**——密钥由调用方显式传入（出站用临时派生密钥、入站按信封 `ephPub`
//! 派生的密钥）。AAD 绑定 `kind:from:to:ts`（防跨 kind/跨对端/跨时间窗搬迁）。
//!
//! 按密钥表 current/history 选取密钥的封装在 [`super::service`]（`encrypt_body` /
//! `decrypt_body`），本文件只交付显式密钥的底层原语。

use aes_gcm::aead::{Aead, Nonce};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::Rng;
use serde_json::{Value, json};

use super::service::{DmE2eError, Result};

/// 构造加密后的 body 线形 `{encrypted:true, ciphertext, nonce}`（§19.1.1）。
/// AAD 绑 `kind:from:to:ts`。**显式密钥**：本函数不读密钥表，密钥由调用方
/// 传入（出站用临时派生会话密钥）。
pub fn encrypt_body_with_key(
    key: &[u8; 32],
    from: &str,
    to: &str,
    kind: &str,
    ts: i64,
    plaintext_body: &Value,
) -> Result<Value> {
    let aad = format!("{kind}:{from}:{to}:{ts}");
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| DmE2eError::Aead(e.to_string()))?;
    let plaintext = plaintext_body.to_string();
    let payload = aes_gcm::aead::Payload {
        msg: plaintext.as_bytes(),
        aad: aad.as_bytes(),
    };
    let ct = cipher
        .encrypt(&Nonce::<Aes256Gcm>::from(nonce_bytes), payload)
        .map_err(|e| DmE2eError::Aead(e.to_string()))?;
    Ok(json!({
        "encrypted": true,
        "ciphertext": B64.encode(ct),
        "nonce": B64.encode(nonce_bytes),
    }))
}

/// 解密 body（§19.1.1 逆，**显式密钥**）：S6 接线用——入站信封携带 `ephPub`
/// 时由接线层用 [`super::derive::derive_session_key_from_eph_pub`] 派生本次临时
/// 会话密钥后传入。本函数不碰密钥表，纯 AES-256-GCM 解密（AAD 绑
/// `kind:from:to:ts`）。
pub fn decrypt_body_with_key(
    key: &[u8; 32],
    from: &str,
    to: &str,
    kind: &str,
    ts: i64,
    encrypted_body: &Value,
) -> Result<Value> {
    let ct_b64 = encrypted_body
        .get("ciphertext")
        .and_then(Value::as_str)
        .ok_or_else(|| DmE2eError::InvalidCiphertext("missing ciphertext".into()))?;
    let nonce_b64 = encrypted_body
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or_else(|| DmE2eError::InvalidCiphertext("missing nonce".into()))?;
    let ct = B64.decode(ct_b64)
        .map_err(|_| DmE2eError::InvalidCiphertext("ciphertext not base64".into()))?;
    let nonce_raw = B64.decode(nonce_b64)
        .map_err(|_| DmE2eError::InvalidCiphertext("nonce not base64".into()))?;
    let nonce_arr: [u8; 12] = nonce_raw
        .try_into()
        .map_err(|_| DmE2eError::InvalidCiphertext("nonce not 12B".into()))?;

    let aad = format!("{kind}:{from}:{to}:{ts}");
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| DmE2eError::Aead(e.to_string()))?;
    let payload = aes_gcm::aead::Payload {
        msg: ct.as_ref(),
        aad: aad.as_bytes(),
    };
    let plain = cipher
        .decrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), payload)
        .map_err(|_| DmE2eError::Aead("AAD tampered or wrong key".into()))?;
    let s = String::from_utf8(plain).map_err(|e| DmE2eError::InvalidPlaintext(e.to_string()))?;
    serde_json::from_str(&s).map_err(|e| DmE2eError::InvalidPlaintext(e.to_string()))
}
