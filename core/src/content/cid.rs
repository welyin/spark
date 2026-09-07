//! CID：内容寻址标识 = SHA-256 的 64 位小写 hex（与 pdsync blob、
//! social-feed「hash 即能力」同口径）。
//!
//! Kad provider key 线形：`spark:blob:{cid}` 明文字节——沿用 `spark:relay`
//! 共享池的明文命名空间 key 风格（provider key 本身即内容寻址，无需二次哈希；
//! 节点存在记录走 `sha256("spark:node:"+peerId)` 是因为 peerId 需要定长散布，
//! CID 天然满足）。

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::{ContentError, Result};

/// blob provider 记录的 Kad key 前缀（`spark:blob:{cid}`）。
pub const BLOB_KAD_KEY_PREFIX: &str = "spark:blob:";

/// 内容寻址标识（SHA-256 hex，64 位小写）。
///
/// 不变量：构造即校验——只允许经 [`Cid::from_data`] / [`Cid::parse`] 产生，
/// 持有 `Cid` 即持有合法形状。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cid(String);

impl Cid {
    /// 从内容字节计算 CID（SHA-256 hex 小写）。
    pub fn from_data(data: &[u8]) -> Self {
        Self(hex::encode(sha2::Sha256::digest(data)))
    }

    /// 解析并校验 CID 形状（64 位小写 hex）。
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(text.to_string()))
        } else {
            Err(ContentError::InvalidCid(text.to_string()))
        }
    }

    /// CID 文本（64 位小写 hex）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 该 CID 的 Kad provider key（`spark:blob:{cid}` 明文字节）。
    pub fn to_kad_key(&self) -> Vec<u8> {
        blob_kad_key(&self.0)
    }
}

impl std::fmt::Display for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Cid {
    type Error = ContentError;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<Cid> for String {
    fn from(cid: Cid) -> Self {
        cid.0
    }
}

/// CID 文本 → Kad provider key 字节（不做形状校验的纯拼接；p2p 层的入参
/// 已在 API 边界经 [`Cid::parse`] 校验）。
pub fn blob_kad_key(cid: &str) -> Vec<u8> {
    format!("{BLOB_KAD_KEY_PREFIX}{cid}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_data_is_sha256_hex() {
        // sha256("hello") 的公开已知值
        let cid = Cid::from_data(b"hello");
        assert_eq!(
            cid.as_str(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn parse_accepts_only_64_lower_hex() {
        assert!(Cid::parse(&"a".repeat(64)).is_ok());
        assert!(Cid::parse(&"0123456789abcdef".repeat(4)).is_ok());
        // 长度不对
        assert!(Cid::parse(&"a".repeat(63)).is_err());
        assert!(Cid::parse(&"a".repeat(65)).is_err());
        // 大写拒绝（统一小写口径，避免同内容两种 key）
        assert!(Cid::parse(&"A".repeat(64)).is_err());
        // 非 hex 字符拒绝
        assert!(Cid::parse(&"g".repeat(64)).is_err());
        assert!(Cid::parse("").is_err());
    }

    #[test]
    fn kad_key_lineage() {
        let cid = Cid::from_data(b"x");
        let key = cid.to_kad_key();
        assert_eq!(key, format!("spark:blob:{}", cid.as_str()).into_bytes());
        // 与 spark:relay 同命名空间风格
        assert!(key.starts_with(b"spark:blob:"));
    }

    #[test]
    fn serde_roundtrip_validates() {
        let cid = Cid::from_data(b"serde");
        let json = serde_json::to_string(&cid).unwrap();
        let back: Cid = serde_json::from_str(&json).unwrap();
        assert_eq!(cid, back);
        assert!(serde_json::from_str::<Cid>("\"not-a-cid\"").is_err());
    }
}
