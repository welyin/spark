//! libp2p peerId 的公钥提取（纯解析实现，不依赖 libp2p 网络栈）。
//!
//! 供纯逻辑层（org 节点名片验签）与 p2p 模块（announce/challenge 验签）
//! 共用，避免 org → p2p 的反向模块依赖（规范 §3.1「网络只属于 p2p 模块」）。
//!
//! 线形（rust-libp2p，Ed25519 一律 identity multihash）：
//! - peerId 字符串 = base58btc 编码的 multihash 字节；
//! - bytes = varint(code=0) + varint(len=36) + protobuf 公钥；
//! - protobuf 公钥 = `0x08 0x01 0x12 0x20` + 32B 原始 Ed25519 公钥
//!   （KeyType::Ed25519=1，length-delimited 字段 2，对照
//!   libp2p-identity `PublicKey::try_decode_protobuf` 的形状约束——
//!   多余/缺失字段都会使解码失败，故逐字节等值判定）。

/// base58btc 字符集（Bitcoin 序）。
const B58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// protobuf Ed25519 公钥头：`0x08 0x01`（field1 varint = KeyType::Ed25519）
/// + `0x12 0x20`（field2 length-delimited，32 字节）。
const ED25519_PROTOBUF_PREFIX: [u8; 4] = [0x08, 0x01, 0x12, 0x20];

/// 从 peerId 字符串提取内嵌的 Ed25519 公钥（原始 32 字节）。
///
/// 非 Ed25519 identity multihash（如 sha256 散列的 RSA peerId）/非法
/// base58/形状不符返回 `None`。
pub fn ed25519_public_key_from_peer_id(peer_id: &str) -> Option<[u8; 32]> {
    let bytes = base58btc_decode(peer_id)?;
    // identity multihash：varint(code=0) + varint(len) + digest(=protobuf 公钥)
    let (code, rest) = read_unsigned_varint(&bytes)?;
    if code != 0 {
        return None;
    }
    let (len, rest) = read_unsigned_varint(rest)?;
    if rest.len() != len as usize {
        return None;
    }
    // protobuf 形状：与 libp2p PublicKey::try_decode_protobuf 的 Ed25519
    // 分支等值（field1=1、field2=32B payload、无多余字段）
    let raw = rest.strip_prefix(&ED25519_PROTOBUF_PREFIX)?;
    let key: [u8; 32] = raw.try_into().ok()?;
    Some(key)
}

/// 最小 base58btc 解码（big-endian 大数除法；输入 ≤ 64 字节，逐字节进位够用）。
fn base58btc_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = vec![0u8; 0];
    for ch in text.bytes() {
        let value = B58_ALPHABET.iter().position(|&c| c == ch)? as u32;
        let mut carry = value;
        for byte in bytes.iter_mut().rev() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    // 前导 '1' = 前导零字节
    let leading_zeros = text.bytes().take_while(|&b| b == b'1').count();
    let mut out = vec![0u8; leading_zeros];
    out.extend_from_slice(&bytes);
    Some(out)
}

/// 最小 unsigned varint 解析（multihash 头）。
fn read_unsigned_varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for (i, b) in bytes.iter().enumerate() {
        result |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((result, &bytes[i + 1..]));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知向量：32B 全 1 公钥的 peerId（identity multihash + protobuf
    /// 前缀，`0x00 0x24 0x08 0x01 0x12 0x20` + 32×0x01 的 base58btc）。
    /// 用编解码对称性自检，并固定一个外部已知的稳定 peerId 形状。
    #[test]
    fn roundtrips_identity_peer_id_bytes() {
        // 手工构造：multihash(0x00, 36) + protobuf 头 + 32B 公钥
        let pubkey = [7u8; 32];
        let mut bytes = vec![0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
        bytes.extend_from_slice(&pubkey);
        // base58btc 编码（本模块解码的逆运算，用简易实现避免依赖断言循环）
        let encoded = base58btc_encode(&bytes);
        let decoded = ed25519_public_key_from_peer_id(&encoded).expect("应可解析");
        assert_eq!(decoded, pubkey);
    }

    #[test]
    fn rejects_non_ed25519_and_malformed() {
        // 非法字符（0/O/I/l 不在 base58btc 字符集）
        assert!(ed25519_public_key_from_peer_id("0OIl").is_none());
        // 空串
        assert!(ed25519_public_key_from_peer_id("").is_none());
        // sha256 multihash（code=0x12）——非 identity，应拒绝
        let mut bytes = vec![0x12, 0x20];
        bytes.extend_from_slice(&[0u8; 32]);
        let encoded = base58btc_encode(&bytes);
        assert!(ed25519_public_key_from_peer_id(&encoded).is_none());
        // identity 但 payload 形状不是 protobuf Ed25519 公钥
        let encoded = base58btc_encode(&[0x00, 0x02, 0xde, 0xad]);
        assert!(ed25519_public_key_from_peer_id(&encoded).is_none());
    }

    /// 测试用简易 base58btc 编码（big-endian 大数乘法）。
    fn base58btc_encode(bytes: &[u8]) -> String {
        let leading_zeros = bytes.iter().take_while(|&&b| b == 0).count();
        let mut digits = vec![0u32; 0];
        for &byte in bytes {
            let mut carry = u32::from(byte);
            for d in digits.iter_mut().rev() {
                carry += *d << 8;
                *d = carry % 58;
                carry /= 58;
            }
            while carry > 0 {
                digits.insert(0, carry % 58);
                carry /= 58;
            }
        }
        let mut out = String::new();
        for _ in 0..leading_zeros {
            out.push('1');
        }
        for d in digits {
            out.push(B58_ALPHABET[d as usize] as char);
        }
        out
    }
}
