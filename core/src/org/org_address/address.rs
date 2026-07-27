//! orgAddress 生成与解析（org.md §15）：sha256 指纹 + 2 字节校验段，base32 无 padding。

use sha2::{Digest, Sha256};

/// orgAddress 字符数（34 字节 base32 无 padding，§15）。
pub const ORG_ADDRESS_LEN: usize = 55;

/// checksum 域分隔符：`sha256("spark:org-address:" ‖ digest)` 前 2 字节（§15）。
pub const ORG_ADDRESS_CHECKSUM_DOMAIN: &str = "spark:org-address:";

/// digest 的 2 字节校验段：`sha256("spark:org-address:" ‖ digest)` 前 2 字节。
fn org_address_checksum(digest: &[u8; 32]) -> [u8; 2] {
    let mut hasher = Sha256::new();
    hasher.update(ORG_ADDRESS_CHECKSUM_DOMAIN.as_bytes());
    hasher.update(digest);
    let hash = hasher.finalize();
    [hash[0], hash[1]]
}

/// 组织根公钥 → orgAddress（55 字符小写 base32 无 padding，§15）。
pub fn org_address_from_public_key(public_key: &[u8; 32]) -> String {
    let digest: [u8; 32] = Sha256::digest(public_key).into();
    let checksum = org_address_checksum(&digest);
    let mut buf = [0u8; 34];
    buf[..32].copy_from_slice(&digest);
    buf[32..].copy_from_slice(&checksum);
    data_encoding::BASE32_NOPAD.encode(&buf).to_lowercase()
}

/// 解析并校验 orgAddress：base32 可解码为 34 字节且 checksum 段匹配，
/// 返回内嵌 digest（= `sha256(orgPublicKey)`，§16.3 第 4 步闭环比对的基准）。
pub fn decode_org_address(address: &str) -> Option<[u8; 32]> {
    let trimmed = address.trim();
    if trimmed.len() != ORG_ADDRESS_LEN
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    {
        return None;
    }
    let raw = data_encoding::BASE32_NOPAD
        .decode(trimmed.to_uppercase().as_bytes())
        .ok()?;
    if raw.len() != 34 {
        return None;
    }
    let digest: [u8; 32] = raw[..32].try_into().ok()?;
    if raw[32..] != org_address_checksum(&digest) {
        return None;
    }
    Some(digest)
}

/// orgAddress 合法性（§15 校验口径）。
pub fn is_valid_org_address(address: &str) -> bool {
    decode_org_address(address).is_some()
}

/// 组织地址记录的 DHT key = `sha256(orgPublicKey 原始 32 字节)` = orgAddress 内嵌
/// digest（p2p-messages.md §16：orgAddress 与该 key 一一对应）。
pub fn org_address_dht_key(org_address: &str) -> Option<Vec<u8>> {
    decode_org_address(org_address).map(|digest| digest.to_vec())
}
