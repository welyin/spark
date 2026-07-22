//! 身份文件加解密原语。
//!
//! - v2：scrypt(N=32768, r=8, p=1, keyLen=32, maxmem=64MB) + aes-256-gcm（iv 12B，
//!   authTag 单独存储）
//! - v1（只读兼容）：pbkdf2(210000, sha512, keyLen=32) + aes-256-cbc（iv 16B，PKCS7）

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use sha2::Sha512;

use super::error::{IdentityError, Result};

/// v2 scrypt 参数 log2(N)。
pub const SCRYPT_LOG_N: u8 = 15; // N = 32768
/// v2 scrypt 参数 r。
pub const SCRYPT_R: u32 = 8;
/// v2 scrypt 参数 p。
pub const SCRYPT_P: u32 = 1;
/// KDF 输出密钥长度（字节）。
pub const KEY_LEN: usize = 32;
/// v2 GCM IV 长度（字节）。
pub const GCM_IV_LEN: usize = 12;
/// v2 GCM authTag 长度（字节）。
pub const GCM_TAG_LEN: usize = 16;
/// v1 PBKDF2 迭代次数。
pub const PBKDF2_ITERATIONS: u32 = 210_000;
/// v1 CBC IV 长度（字节）。
pub const CBC_IV_LEN: usize = 16;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// 由已校验长度的 IV 构造 GCM nonce（12 字节）。
fn nonce_from_iv(iv: &[u8]) -> Result<aes_gcm::aead::Nonce<Aes256Gcm>> {
    let arr: [u8; GCM_IV_LEN] = iv
        .try_into()
        .map_err(|_| IdentityError::Crypto(format!("v2 iv must be {GCM_IV_LEN} bytes")))?;
    Ok(arr.into())
}

/// v2 KDF：scrypt(password, salt, N=32768, r=8, p=1) → 32 字节密钥。
///
/// 说明：Rust `scrypt` crate 不暴露 maxmem 参数；N=32768,r=8 实际内存
/// 128·N·r = 32MB，低于规格的 64MB 上限，行为一致。
pub fn scrypt_v2_key(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P)
        .map_err(|e| IdentityError::Crypto(format!("scrypt params: {e}")))?;
    let mut key = [0u8; KEY_LEN];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut key)
        .map_err(|e| IdentityError::Crypto(format!("scrypt: {e}")))?;
    Ok(key)
}

/// v1 KDF：pbkdf2(password, salt, 210000, sha512) → 32 字节密钥。
pub fn pbkdf2_v1_key(password: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha512>(password.as_bytes(), salt, PBKDF2_ITERATIONS, &mut key);
    key
}

/// v2 加密：aes-256-gcm。返回 (密文, authTag)。
pub fn encrypt_v2(plaintext: &[u8], password: &str, salt: &[u8], iv: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if iv.len() != GCM_IV_LEN {
        return Err(IdentityError::Crypto(format!(
            "v2 iv must be {GCM_IV_LEN} bytes, got {}",
            iv.len()
        )));
    }
    let key = scrypt_v2_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| IdentityError::Crypto(e.to_string()))?;
    let nonce = nonce_from_iv(iv)?;
    let sealed = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| IdentityError::Crypto(format!("aes-gcm encrypt: {e}")))?;
    let (data, tag) = sealed.split_at(sealed.len() - GCM_TAG_LEN);
    Ok((data.to_vec(), tag.to_vec()))
}

/// v2 解密：aes-256-gcm，authTag 单独传入。
pub fn decrypt_v2(data: &[u8], auth_tag: &[u8], password: &str, salt: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if iv.len() != GCM_IV_LEN {
        return Err(IdentityError::Crypto(format!(
            "v2 iv must be {GCM_IV_LEN} bytes, got {}",
            iv.len()
        )));
    }
    if auth_tag.len() != GCM_TAG_LEN {
        return Err(IdentityError::Crypto(format!(
            "v2 authTag must be {GCM_TAG_LEN} bytes, got {}",
            auth_tag.len()
        )));
    }
    let key = scrypt_v2_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| IdentityError::Crypto(e.to_string()))?;
    let nonce = nonce_from_iv(iv)?;
    let mut sealed = Vec::with_capacity(data.len() + GCM_TAG_LEN);
    sealed.extend_from_slice(data);
    sealed.extend_from_slice(auth_tag);
    cipher
        .decrypt(&nonce, sealed.as_ref())
        .map_err(|_| IdentityError::DecryptionFailed)
}

/// v1 加密：aes-256-cbc + PKCS7（用于生成测试与迁移验证）。
pub fn encrypt_v1(plaintext: &[u8], password: &str, salt: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if iv.len() != CBC_IV_LEN {
        return Err(IdentityError::Crypto(format!(
            "v1 iv must be {CBC_IV_LEN} bytes, got {}",
            iv.len()
        )));
    }
    let key = pbkdf2_v1_key(password, salt);
    Ok(Aes256CbcEnc::new_from_slices(&key, iv)
        .map_err(|e| IdentityError::Crypto(e.to_string()))?
        .encrypt_padded_vec::<Pkcs7>(plaintext))
}

/// v1 解密：aes-256-cbc + PKCS7。
pub fn decrypt_v1(data: &[u8], password: &str, salt: &[u8], iv: &[u8]) -> Result<Vec<u8>> {
    if iv.len() != CBC_IV_LEN {
        return Err(IdentityError::Crypto(format!(
            "v1 iv must be {CBC_IV_LEN} bytes, got {}",
            iv.len()
        )));
    }
    let key = pbkdf2_v1_key(password, salt);
    Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| IdentityError::Crypto(e.to_string()))?
        .decrypt_padded_vec::<Pkcs7>(data)
        .map_err(|_| IdentityError::DecryptionFailed)
}
