//! node-announce：覆盖网控制面通告（对齐 node-announce.ts 与 core/spec/p2p-messages.md §5）。
//!
//! - 用本机 **libp2p Ed25519 私钥**签名（证明"该 peerId 持有者发布了这些地址"），
//!   不携带 rootId/根公钥；
//! - 待签名载荷 = 固定键序紧凑 JSON：
//!   `{"type":...,"version":...,"peerId":...,"addresses":...,"timestamp":...}`
//! - 验签公钥从 `peerId` 字符串内嵌提取（identity multihash → protobuf 公钥）；
//! - 接收校验链按序：结构 → ±10min 新鲜度 → 地址数/长度 → 非本机 → 限流（新地址放宽）→ 验签。

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use libp2p::identity::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::constants::{
    MAX_ANNOUNCE_ADDRESSES, MAX_ANNOUNCE_ADDRESS_LENGTH, NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_MS,
    NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_ON_CHANGE_MS, NODE_ANNOUNCE_MAX_AGE_MS,
};

/// node-announce 消息（TS `NodeAnnounce`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAnnounce {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub version: u32,
    #[serde(rename = "peerId")]
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub timestamp: i64,
    pub signature: String,
}

/// 待签名载荷（固定键序紧凑 JSON，node-announce.ts:35-43）。
pub fn build_node_announce_payload(
    peer_id: &str,
    addresses: &[String],
    timestamp_ms: i64,
) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("spark-node-announce".to_string()));
    map.insert("version".to_string(), Value::Number(1.into()));
    map.insert("peerId".to_string(), Value::String(peer_id.to_string()));
    map.insert(
        "addresses".to_string(),
        Value::Array(addresses.iter().map(|a| Value::String(a.clone())).collect()),
    );
    map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
    serde_json::to_string(&Value::Object(map)).expect("announce payload is always serializable")
}

/// 签名并构造完整 announce 消息。
pub fn sign_node_announce(
    keypair: &Keypair,
    peer_id: &str,
    addresses: &[String],
    timestamp_ms: i64,
) -> Result<NodeAnnounce, libp2p::identity::SigningError> {
    let payload = build_node_announce_payload(peer_id, addresses, timestamp_ms);
    let signature = keypair.sign(payload.as_bytes())?;
    Ok(NodeAnnounce {
        msg_type: "spark-node-announce".to_string(),
        version: 1,
        peer_id: peer_id.to_string(),
        addresses: addresses.to_vec(),
        timestamp: timestamp_ms,
        signature: B64.encode(signature),
    })
}

/// 发布侧地址过滤：滤空、滤超长、截 20 条；无地址则不发布（返回 None）。
pub fn prepare_publish_addresses(addresses: &[String]) -> Option<Vec<String>> {
    let filtered: Vec<String> = addresses
        .iter()
        .filter(|a| !a.is_empty() && a.len() <= MAX_ANNOUNCE_ADDRESS_LENGTH)
        .take(MAX_ANNOUNCE_ADDRESSES)
        .cloned()
        .collect();
    if filtered.is_empty() { None } else { Some(filtered) }
}

/// 完整 announce 的紧凑 JSON（发布字节）。
pub fn announce_to_json(announce: &NodeAnnounce) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(announce.msg_type.clone()));
    map.insert("version".to_string(), Value::Number(announce.version.into()));
    map.insert("peerId".to_string(), Value::String(announce.peer_id.clone()));
    map.insert(
        "addresses".to_string(),
        Value::Array(announce.addresses.iter().map(|a| Value::String(a.clone())).collect()),
    );
    map.insert("timestamp".to_string(), Value::Number(announce.timestamp.into()));
    map.insert("signature".to_string(), Value::String(announce.signature.clone()));
    serde_json::to_string(&Value::Object(map)).expect("announce is always serializable")
}

/// 接收侧拒绝原因（校验链按序，任一失败静默丢弃）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnounceReject {
    /// JSON 解析或结构不符。
    Structure,
    /// 时间戳超出 ±10 min。
    Stale,
    /// 地址数/单地址长度越界。
    AddressLimits,
    /// 本机 peerId。
    SelfPeer,
    /// 限流。
    RateLimited,
    /// 验签失败（含 peerId 无法提取公钥）。
    BadSignature,
}

/// 从 peerId 字符串提取内嵌的 Ed25519 公钥。
///
/// rust-libp2p 对 Ed25519 一律用 identity multihash（公钥短）：
/// bytes = `0x00 <len> <protobuf 公钥>`。返回原始 32 字节公钥。
pub fn public_key_from_peer_id_str(peer_id: &str) -> Option<[u8; 32]> {
    let peer_id: libp2p::PeerId = peer_id.parse().ok()?;
    let bytes = peer_id.to_bytes();
    // 解析 identity multihash：varint(code=0) + varint(len) + digest
    let (code, rest) = read_unsigned_varint(&bytes)?;
    if code != 0 {
        return None;
    }
    let (len, rest) = read_unsigned_varint(rest)?;
    if rest.len() != len as usize {
        return None;
    }
    let public = PublicKey::try_decode_protobuf(rest).ok()?;
    let raw = public.try_into_ed25519().ok()?.to_bytes();
    Some(raw)
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

/// 结构校验（对齐 isNodeAnnounce，node-announce.ts:45-56）。
fn parse_structure(text: &str) -> Option<NodeAnnounce> {
    let value: Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    let msg_type = obj.get("type")?.as_str()?;
    let version = obj.get("version")?.as_u64()?;
    let peer_id = obj.get("peerId")?.as_str()?;
    let addresses: Vec<String> = obj
        .get("addresses")?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(ToString::to_string))
        .collect::<Option<_>>()?;
    let timestamp = obj.get("timestamp")?.as_i64()?;
    let signature = obj.get("signature")?.as_str()?;
    if msg_type != "spark-node-announce" || version != 1 {
        return None;
    }
    Some(NodeAnnounce {
        msg_type: msg_type.to_string(),
        version: version as u32,
        peer_id: peer_id.to_string(),
        addresses,
        timestamp,
        signature: signature.to_string(),
    })
}

/// 接收侧校验链 + 限流状态（lastAcceptedAtByPeerId）。
#[derive(Default)]
pub struct NodeAnnounceValidator {
    last_accepted_at: HashMap<String, i64>,
}

impl NodeAnnounceValidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 校验入站 announce；通过即返回消息（调用方随后 `overlayPeers.remember(..., 'announce', true)`）。
    ///
    /// - `known_addresses`：邻居池中该 peerId 已存地址（判定是否携带新地址）
    /// - `now_ms`：当前时间注入
    pub fn validate(
        &mut self,
        text: &str,
        self_peer_id: &str,
        known_addresses: &[String],
        now_ms: i64,
    ) -> std::result::Result<NodeAnnounce, AnnounceReject> {
        let announce = parse_structure(text).ok_or(AnnounceReject::Structure)?;

        if (now_ms - announce.timestamp).abs() > NODE_ANNOUNCE_MAX_AGE_MS {
            return Err(AnnounceReject::Stale);
        }
        if announce.addresses.is_empty() || announce.addresses.len() > MAX_ANNOUNCE_ADDRESSES {
            return Err(AnnounceReject::AddressLimits);
        }
        if announce
            .addresses
            .iter()
            .any(|a| a.is_empty() || a.len() > MAX_ANNOUNCE_ADDRESS_LENGTH)
        {
            return Err(AnnounceReject::AddressLimits);
        }
        if announce.peer_id == self_peer_id {
            return Err(AnnounceReject::SelfPeer);
        }

        // 限流：携带未知新地址时放宽到 5s，否则 60s
        let carries_new = announce
            .addresses
            .iter()
            .any(|a| !known_addresses.contains(a));
        let min_interval = if carries_new {
            NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_ON_CHANGE_MS
        } else {
            NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_MS
        };
        let last = self.last_accepted_at.get(&announce.peer_id).copied().unwrap_or(0);
        if now_ms - last < min_interval {
            return Err(AnnounceReject::RateLimited);
        }
        // TS 口径：限流检查通过即消耗配额（node-announce.ts:180），先于验签
        self.last_accepted_at.insert(announce.peer_id.clone(), now_ms);

        // 验签：peerId 内嵌公钥 + 固定键序载荷
        let raw_public = public_key_from_peer_id_str(&announce.peer_id)
            .ok_or(AnnounceReject::BadSignature)?;
        let sig_bytes = B64.decode(&announce.signature).map_err(|_| AnnounceReject::BadSignature)?;
        if sig_bytes.len() != 64 {
            return Err(AnnounceReject::BadSignature);
        }
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let payload = build_node_announce_payload(
            &announce.peer_id,
            &announce.addresses,
            announce.timestamp,
        );
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&raw_public)
            .map_err(|_| AnnounceReject::BadSignature)?;
        use ed25519_dalek::Verifier;
        verifying_key
            .verify(payload.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig_arr))
            .map_err(|_| AnnounceReject::BadSignature)?;

        Ok(announce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_keypair() -> Keypair {
        Keypair::generate_ed25519()
    }

    #[test]
    fn payload_fixed_key_order() {
        let payload = build_node_announce_payload(
            "12D3KooWTest",
            &["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
            1_720_000_000_000,
        );
        assert_eq!(
            payload,
            "{\"type\":\"spark-node-announce\",\"version\":1,\"peerId\":\"12D3KooWTest\",\"addresses\":[\"/ip4/1.2.3.4/tcp/15002/ws\"],\"timestamp\":1720000000000}"
        );
    }

    #[test]
    fn sign_and_accept_roundtrip() {
        let keypair = make_keypair();
        let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
        let addrs = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];
        let announce = sign_node_announce(&keypair, &peer_id, &addrs, 1_000_000).unwrap();
        let text = announce_to_json(&announce);

        let mut validator = NodeAnnounceValidator::new();
        let accepted = validator
            .validate(&text, "12D3KooWSelfSelfSelf", &[], 1_000_000)
            .expect("must accept");
        assert_eq!(accepted.peer_id, peer_id);
        assert_eq!(accepted.addresses, addrs);
    }

    #[test]
    fn peer_id_pubkey_extraction_roundtrip() {
        let keypair = make_keypair();
        let peer_id = libp2p::PeerId::from_public_key(&keypair.public());
        let raw = public_key_from_peer_id_str(&peer_id.to_base58()).expect("extractable");
        let expect = keypair.public().try_into_ed25519().unwrap().to_bytes();
        assert_eq!(raw, expect);
    }

    #[test]
    fn reject_chain() {
        let keypair = make_keypair();
        let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
        let addrs = vec!["/ip4/127.0.0.1/tcp/15002/ws".to_string()];
        let now = 1_000_000i64;

        // 过期
        let stale = sign_node_announce(&keypair, &peer_id, &addrs, now - NODE_ANNOUNCE_MAX_AGE_MS - 1).unwrap();
        let mut v = NodeAnnounceValidator::new();
        assert_eq!(
            v.validate(&announce_to_json(&stale), "self", &[], now),
            Err(AnnounceReject::Stale)
        );

        // 未来 10 min 内可接受（Math.abs 口径）
        let future = sign_node_announce(&keypair, &peer_id, &addrs, now + NODE_ANNOUNCE_MAX_AGE_MS).unwrap();
        assert!(v.validate(&announce_to_json(&future), "self", &[], now).is_ok());

        // 本机
        let fresh = sign_node_announce(&keypair, &peer_id, &addrs, now).unwrap();
        let mut v = NodeAnnounceValidator::new();
        assert_eq!(
            v.validate(&announce_to_json(&fresh), &peer_id, &[], now),
            Err(AnnounceReject::SelfPeer)
        );

        // 篡改签名
        let mut tampered = fresh.clone();
        tampered.timestamp = now + 1;
        let mut v = NodeAnnounceValidator::new();
        assert_eq!(
            v.validate(&announce_to_json(&tampered), "self", &[], now),
            Err(AnnounceReject::BadSignature)
        );

        // 限流：同一 peerId 60s 内第二次（无新地址）被拒
        let mut v = NodeAnnounceValidator::new();
        assert!(v.validate(&announce_to_json(&fresh), "self", &[], now).is_ok());
        assert_eq!(
            v.validate(&announce_to_json(&fresh), "self", &addrs, now + 10_000),
            Err(AnnounceReject::RateLimited)
        );
        // 携带新地址放宽到 5s
        let new_addrs = vec!["/ip4/10.0.0.9/tcp/15002/ws".to_string()];
        let changed = sign_node_announce(&keypair, &peer_id, &new_addrs, now + 6_000).unwrap();
        assert!(
            v.validate(&announce_to_json(&changed), "self", &addrs, now + 6_000)
                .is_ok()
        );

        // 空地址
        let empty = NodeAnnounce {
            addresses: vec![],
            ..fresh.clone()
        };
        let mut v = NodeAnnounceValidator::new();
        assert_eq!(
            v.validate(&announce_to_json(&empty), "self", &[], now),
            Err(AnnounceReject::AddressLimits)
        );
    }
}
