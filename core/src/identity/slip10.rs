//! SLIP-0010 ed25519 强化派生（手写实现，逻辑小、无合适对齐的现成 crate）。
//!
//! - master：I = HMAC-SHA512(key="ed25519 seed", data=seed)
//! - 子节点（仅强化）：data = 0x00 ‖ parent.key ‖ ser32(index + 0x80000000) BE，
//!   I = HMAC-SHA512(key=parent.chainCode, data=data)

use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha512;

use super::error::{IdentityError, Result};

/// 强化派生偏移量。
pub const HARDENED_OFFSET: u32 = 0x8000_0000;

type HmacSha512 = Hmac<Sha512>;

fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut mac =
        <HmacSha512 as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// SLIP-0010 派生节点（32 字节私钥 + 32 字节链码）。
#[derive(Clone, Debug)]
pub struct Slip10Node {
    /// 节点私钥（32 字节）。
    pub key: [u8; 32],
    /// 节点链码（32 字节）。
    pub chain_code: [u8; 32],
}

impl Slip10Node {
    /// 由种子生成 master 节点。
    pub fn master(seed: &[u8]) -> Self {
        let i = hmac_sha512(b"ed25519 seed", seed);
        Self::from_i(&i)
    }

    /// 强化派生一个子节点。`index` 为未加偏移的原始索引。
    pub fn derive_child(&self, index: u32) -> Self {
        debug_assert!(index < HARDENED_OFFSET);
        let mut data = [0u8; 1 + 32 + 4];
        data[1..33].copy_from_slice(&self.key);
        data[33..37].copy_from_slice(&(index + HARDENED_OFFSET).to_be_bytes());
        let i = hmac_sha512(&self.chain_code, &data);
        Self::from_i(&i)
    }

    /// 沿索引序列逐层强化派生。
    pub fn derive_path(&self, indices: &[u32]) -> Self {
        indices.iter().fold(self.clone(), |node, &idx| node.derive_child(idx))
    }

    fn from_i(i: &[u8; 64]) -> Self {
        let mut key = [0u8; 32];
        let mut chain_code = [0u8; 32];
        key.copy_from_slice(&i[..32]);
        chain_code.copy_from_slice(&i[32..]);
        Self { key, chain_code }
    }
}

/// 解析形如 `m/44'/607'/0'/0'/0'` 的派生路径为索引序列（仅接受全强化路径）。
pub fn parse_derivation_path(path: &str) -> Result<Vec<u32>> {
    let stripped = path.strip_prefix("m/").unwrap_or(path);
    if stripped.is_empty() {
        return Err(IdentityError::InvalidPath(path.to_string()));
    }
    stripped
        .split('/')
        .map(|seg| {
            let idx_str = seg
                .strip_suffix('\'')
                .ok_or_else(|| IdentityError::InvalidPath(path.to_string()))?;
            idx_str
                .parse::<u32>()
                .ok()
                .filter(|&i| i < HARDENED_OFFSET)
                .ok_or_else(|| IdentityError::InvalidPath(path.to_string()))
        })
        .collect()
}

/// 将索引序列格式化为 `m/44'/607'/...` 形式。
pub fn format_derivation_path(indices: &[u32]) -> String {
    let mut s = String::from("m");
    for idx in indices {
        s.push_str(&format!("/{idx}'"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_roundtrip() {
        let path = "m/44'/607'/0'/0'/0'";
        let indices = parse_derivation_path(path).unwrap();
        assert_eq!(indices, vec![44, 607, 0, 0, 0]);
        assert_eq!(format_derivation_path(&indices), path);
    }

    #[test]
    fn rejects_non_hardened() {
        assert!(parse_derivation_path("m/44'/607'/0").is_err());
        assert!(parse_derivation_path("m/44'/x'/0'").is_err());
        assert!(parse_derivation_path("m/").is_err());
    }
}
