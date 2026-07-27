//! node-challenge：挑战-应答身份确认（`/spark/node-challenge/1.0.0`）。
//!
//! 三层身份确认的第③层（见 development_plan）：请求方发随机 nonce，
//! 响应方用自己的 **libp2p Ed25519 私钥** 对固定键序紧凑 JSON
//! `{"nonce":...,"timestamp":...}` 签名回执；请求方从对端 PeerId 内嵌提取
//! 公钥验签（复用 announce.rs 的 `public_key_from_peer_id_str`），证明对端
//! 确实持有该 PeerId 的私钥（防 DHT 记录投毒/冒充）。
//!
//! 帧形状照 direct.rs 纯函数风格：整段 JSON 单帧，解析失败返回 None。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use libp2p::identity::Keypair;
use serde_json::{Map, Value};

use super::announce::public_key_from_peer_id_str;
use super::constants::CHALLENGE_MAX_AGE_MS;

/// 挑战请求（type + nonce + timestamp）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChallengeRequest {
    pub nonce: String,
    pub timestamp: i64,
}

/// 生成随机 nonce（16 字节 → 32 hex）。
pub fn generate_nonce() -> String {
    use rand::Rng as _;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 构造挑战请求帧文本（固定键序）。
pub fn build_challenge_request(nonce: &str, timestamp_ms: i64) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("node-challenge-request".to_string()),
    );
    map.insert("nonce".to_string(), Value::String(nonce.to_string()));
    map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
    serde_json::to_string(&Value::Object(map)).expect("challenge request is always serializable")
}

/// 解析挑战请求帧；形状非法返回 None。
pub fn parse_challenge_request(text: &str) -> Option<ChallengeRequest> {
    let value: Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "node-challenge-request" {
        return None;
    }
    let nonce = obj.get("nonce")?.as_str()?;
    if nonce.is_empty() || nonce.len() > 128 {
        return None;
    }
    let timestamp = obj.get("timestamp")?.as_i64()?;
    Some(ChallengeRequest {
        nonce: nonce.to_string(),
        timestamp,
    })
}

/// 待签名回执载荷（固定键序紧凑 JSON：`{"nonce":...,"timestamp":...}`）。
pub fn build_challenge_payload(nonce: &str, timestamp_ms: i64) -> String {
    let mut map = Map::new();
    map.insert("nonce".to_string(), Value::String(nonce.to_string()));
    map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
    serde_json::to_string(&Value::Object(map)).expect("challenge payload is always serializable")
}

/// 用本机 libp2p 私钥对 nonce|timestamp 签名，构造完整回执帧文本。
pub fn sign_challenge_response(
    keypair: &Keypair,
    nonce: &str,
    timestamp_ms: i64,
) -> Result<String, libp2p::identity::SigningError> {
    let payload = build_challenge_payload(nonce, timestamp_ms);
    let signature = keypair.sign(payload.as_bytes())?;
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("node-challenge-response".to_string()),
    );
    map.insert("nonce".to_string(), Value::String(nonce.to_string()));
    map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
    map.insert(
        "signature".to_string(),
        Value::String(B64.encode(signature)),
    );
    Ok(serde_json::to_string(&Value::Object(map))
        .expect("challenge response is always serializable"))
}

/// 回执拒绝原因（校验链按序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeReject {
    /// JSON 解析或结构不符。
    Structure,
    /// nonce 与请求不符。
    NonceMismatch,
    /// 时间戳超出 ±60s。
    Stale,
    /// 验签失败（含 peerId 无法提取公钥）。
    BadSignature,
}

/// 校验挑战回执：结构 → nonce 匹配 → 新鲜度 → PeerId 内嵌公钥验签。
pub fn verify_challenge_response(
    text: &str,
    peer_id: &str,
    expected_nonce: &str,
    now_ms: i64,
) -> Result<(), ChallengeReject> {
    let value: Value = serde_json::from_str(text).map_err(|_| ChallengeReject::Structure)?;
    let obj = value.as_object().ok_or(ChallengeReject::Structure)?;
    if obj.get("type").and_then(Value::as_str) != Some("node-challenge-response") {
        return Err(ChallengeReject::Structure);
    }
    let nonce = obj
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or(ChallengeReject::Structure)?;
    let timestamp = obj
        .get("timestamp")
        .and_then(Value::as_i64)
        .ok_or(ChallengeReject::Structure)?;
    let signature = obj
        .get("signature")
        .and_then(Value::as_str)
        .ok_or(ChallengeReject::Structure)?;

    if nonce != expected_nonce {
        return Err(ChallengeReject::NonceMismatch);
    }
    if (now_ms - timestamp).abs() > CHALLENGE_MAX_AGE_MS {
        return Err(ChallengeReject::Stale);
    }

    let raw_public = public_key_from_peer_id_str(peer_id).ok_or(ChallengeReject::BadSignature)?;
    let sig_bytes = B64
        .decode(signature)
        .map_err(|_| ChallengeReject::BadSignature)?;
    if sig_bytes.len() != 64 {
        return Err(ChallengeReject::BadSignature);
    }
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);
    let payload = build_challenge_payload(nonce, timestamp);
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&raw_public)
        .map_err(|_| ChallengeReject::BadSignature)?;
    use ed25519_dalek::Verifier;
    verifying_key
        .verify(
            payload.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .map_err(|_| ChallengeReject::BadSignature)?;

    Ok(())
}
