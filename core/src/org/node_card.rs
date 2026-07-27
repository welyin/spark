//! 节点名片（手动恢复连接，wiki/protocol/org.md §17）。
//!
//! - **名片串** = `base64url(紧凑 JSON 的 UTF-8)`（`URL_SAFE_NO_PAD`，口径同
//!   邀请码 invite.rs）；线形 `{type, version, peerId, addresses, timestamp,
//!   recoveryToken?, signature}`，`recoveryToken` 缺省时线上对象丢键；
//! - **签名**：本机 **libp2p Ed25519 私钥**（同 node-announce 密钥链），待签名
//!   载荷 = 固定键序紧凑 JSON 且 `recoveryToken ?? null`（§17.2）；
//! - **校验链**按序（§17.3）：结构 → ±10min 新鲜度（`abs` 口径）→
//!   recoveryToken 形状（仅形状 `^[0-9a-f]{64}$`，不校验有效性）→ 从 peerId
//!   提取内嵌公钥验签；
//! - **导入口径**：一律按未验证提示 `remember(..., verified=false)` 入邻居池
//!   并发起连接（见 kernel `import_node_card`）；后续组织校验照旧走
//!   pull/claim 链路，信任边界不变。

use base64::Engine;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD};
use libp2p::identity::Keypair;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::p2p::announce::public_key_from_peer_id_str;
use crate::p2p::constants::NODE_ANNOUNCE_MAX_AGE_MS;

/// 名片 type 标签。
pub const NODE_CARD_TYPE: &str = "spark-node-card";

/// 名片新鲜窗口：±10 min（与 node-announce 同口径，org.md §17.3.2）。
pub const NODE_CARD_MAX_AGE_MS: i64 = NODE_ANNOUNCE_MAX_AGE_MS;

/// 节点名片（org.md §17.1 线形）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCard {
    /// 固定 `spark-node-card`。
    #[serde(rename = "type")]
    pub card_type: String,
    /// 固定 1。
    pub version: u32,
    /// 发布方 libp2p peerId 字符串。
    #[serde(rename = "peerId")]
    pub peer_id: String,
    /// 发布方 multiaddr 列表。
    pub addresses: Vec<String>,
    /// 签发时间（ms）。
    pub timestamp: i64,
    /// 可省：org-recovery token（`sha256hex(orgId:recoverySecret:timeBucket)`
    /// 当前桶，org.md §10）；缺省时线上对象丢键。
    #[serde(
        rename = "recoveryToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub recovery_token: Option<String>,
    /// 签名（64 字节 Ed25519 签名的 base64）。
    pub signature: String,
}

/// 待签名载荷（固定键序紧凑 JSON，`recoveryToken ?? null`，org.md §17.2）。
pub fn build_node_card_payload(
    peer_id: &str,
    addresses: &[String],
    timestamp_ms: i64,
    recovery_token: Option<&str>,
) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String(NODE_CARD_TYPE.to_string()),
    );
    map.insert("version".to_string(), Value::Number(1.into()));
    map.insert("peerId".to_string(), Value::String(peer_id.to_string()));
    map.insert(
        "addresses".to_string(),
        Value::Array(addresses.iter().map(|a| Value::String(a.clone())).collect()),
    );
    map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
    map.insert(
        "recoveryToken".to_string(),
        recovery_token.map_or(Value::Null, |t| Value::String(t.to_string())),
    );
    serde_json::to_string(&Value::Object(map)).expect("node card payload is always serializable")
}

/// 签名并构造完整名片。
pub fn sign_node_card(
    keypair: &Keypair,
    peer_id: &str,
    addresses: &[String],
    timestamp_ms: i64,
    recovery_token: Option<String>,
) -> Result<NodeCard, libp2p::identity::SigningError> {
    let payload =
        build_node_card_payload(peer_id, addresses, timestamp_ms, recovery_token.as_deref());
    let signature = keypair.sign(payload.as_bytes())?;
    Ok(NodeCard {
        card_type: NODE_CARD_TYPE.to_string(),
        version: 1,
        peer_id: peer_id.to_string(),
        addresses: addresses.to_vec(),
        timestamp: timestamp_ms,
        recovery_token,
        signature: B64.encode(signature),
    })
}

/// 名片串编码：紧凑 JSON → base64url（无 padding，invite.rs 口径）。
pub fn encode_node_card(card: &NodeCard) -> String {
    let json = serde_json::to_string(card).expect("node card is always serializable");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// 构造 + 签名 + 编码一步完成（kernel `make_node_card` 用）。
pub fn make_node_card(
    keypair: &Keypair,
    peer_id: &str,
    addresses: &[String],
    timestamp_ms: i64,
    recovery_token: Option<String>,
) -> Result<String, libp2p::identity::SigningError> {
    let card = sign_node_card(keypair, peer_id, addresses, timestamp_ms, recovery_token)?;
    Ok(encode_node_card(&card))
}

/// 名片拒绝原因（校验链按序，org.md §17.3）。
///
/// `reason()` 返回向量登记的英文 reason；`Display` 为面向用户的中文文案
/// （invite.rs 中文错误模式）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NodeCardReject {
    /// base64url 不可解码或 JSON 不合法。
    #[error("节点名片格式不正确")]
    Malformed,
    /// type/version 不符或字段形状不匹配。
    #[error("不是有效的星火节点名片")]
    Structure,
    /// 时间戳超出 ±10 min。
    #[error("节点名片已过期，请让对方重新生成")]
    Stale,
    /// recoveryToken 形状不符 `^[0-9a-f]{64}$`。
    #[error("节点名片的恢复 token 格式非法")]
    BadRecoveryToken,
    /// 验签失败（含 peerId 无法提取公钥、签名非本 peerId 持有者所签）。
    #[error("节点名片签名校验失败")]
    BadSignature,
}

impl NodeCardReject {
    /// golden vectors 登记的英文 reason。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Structure => "structure",
            Self::Stale => "stale",
            Self::BadRecoveryToken => "bad-recovery-token",
            Self::BadSignature => "invalid-signature",
        }
    }
}

/// 结构校验（§17.3.1）：base64url 可解码、JSON 合法、字段形状匹配、
/// `type == "spark-node-card" && version == 1`。
pub fn decode_node_card(code: &str) -> Result<NodeCard, NodeCardReject> {
    let trimmed = code.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed.as_bytes())
        .map_err(|_| NodeCardReject::Malformed)?;
    let card: NodeCard = serde_json::from_slice(&bytes).map_err(|_| NodeCardReject::Malformed)?;
    if card.card_type != NODE_CARD_TYPE || card.version != 1 {
        return Err(NodeCardReject::Structure);
    }
    Ok(card)
}

/// 新鲜度 → recoveryToken 形状 → 验签（§17.3.2-3）。
pub fn verify_node_card(card: &NodeCard, now_ms: i64) -> Result<(), NodeCardReject> {
    if (now_ms - card.timestamp).abs() > NODE_CARD_MAX_AGE_MS {
        return Err(NodeCardReject::Stale);
    }
    if let Some(token) = &card.recovery_token {
        let shape_ok = token.len() == 64
            && token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !shape_ok {
            return Err(NodeCardReject::BadRecoveryToken);
        }
    }
    let raw_public =
        public_key_from_peer_id_str(&card.peer_id).ok_or(NodeCardReject::BadSignature)?;
    let sig_bytes = B64
        .decode(&card.signature)
        .map_err(|_| NodeCardReject::BadSignature)?;
    if sig_bytes.len() != 64 {
        return Err(NodeCardReject::BadSignature);
    }
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);
    let payload = build_node_card_payload(
        &card.peer_id,
        &card.addresses,
        card.timestamp,
        card.recovery_token.as_deref(),
    );
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&raw_public)
        .map_err(|_| NodeCardReject::BadSignature)?;
    use ed25519_dalek::Verifier;
    verifying_key
        .verify(
            payload.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .map_err(|_| NodeCardReject::BadSignature)?;
    Ok(())
}

/// 完整校验链：结构 → 新鲜度 → token 形状 → 验签；通过即返回名片。
pub fn parse_and_verify_node_card(code: &str, now_ms: i64) -> Result<NodeCard, NodeCardReject> {
    let card = decode_node_card(code)?;
    verify_node_card(&card, now_ms)?;
    Ok(card)
}
