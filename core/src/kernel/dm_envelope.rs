//! dm 信封（`/spark/dm/1.0.0` 承载的应用层信封）的构造与校验。
//!
//! 线形 JSON：
//! ```json
//! { "kind": "chat|read|recall|friend-request|friend-accept",
//!   "from": "<rootId>", "to": "<rootId>", "ts": 123,
//!   "body": { ... }, "pubKey": "<base64>", "sig": "<base64>" }
//! ```
//!
//! - 签名载荷 = 固定键序（body/from/kind/to/ts）紧凑 JSON 串，签其 UTF-8 字节
//!   （serde_json `preserve_order`：按序构建 `Map` 序列化即确定性）；
//! - `pubKey` 为根身份 ed25519 公钥原始 32 字节的 base64（与
//!   [`crate::identity::verify_ed25519_signature`] 口径一致），
//!   `from` = sha256hex(pubKey)（rootId 定义，`Identity::id`）；
//! - 入站校验：字段齐全 → `to` 指向本机 → ts 新鲜度（±10 min，防重放）→
//!   pubKey 与 from 绑定 → 验签。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::identity::verify_ed25519_signature;

/// 信封 kind：聊天消息。
pub const KIND_CHAT: &str = "chat";
/// 信封 kind：已读回执。
pub const KIND_READ: &str = "read";
/// 信封 kind：撤回通知。
pub const KIND_RECALL: &str = "recall";
/// 信封 kind：好友申请。
pub const KIND_FRIEND_REQUEST: &str = "friend-request";
/// 信封 kind：好友申请通过。
pub const KIND_FRIEND_ACCEPT: &str = "friend-accept";

/// 签名载荷：固定键序 body/from/kind/to/ts 的紧凑 JSON 串。
pub fn build_signing_payload(kind: &str, from: &str, to: &str, ts: i64, body: &Value) -> String {
    let mut map = Map::new();
    map.insert("body".to_string(), body.clone());
    map.insert("from".to_string(), Value::from(from));
    map.insert("kind".to_string(), Value::from(kind));
    map.insert("to".to_string(), Value::from(to));
    map.insert("ts".to_string(), Value::from(ts));
    serde_json::to_string(&Value::Object(map)).expect("dm signing payload is always serializable")
}

/// 构造并签名完整信封（出站侧）。
pub fn build_envelope(
    kind: &str,
    from: &str,
    to: &str,
    ts: i64,
    body: Value,
    signing_key: &SigningKey,
) -> Value {
    let payload = build_signing_payload(kind, from, to, ts, &body);
    let signature = signing_key.sign(payload.as_bytes());
    let mut map = Map::new();
    map.insert("kind".to_string(), Value::from(kind));
    map.insert("from".to_string(), Value::from(from));
    map.insert("to".to_string(), Value::from(to));
    map.insert("ts".to_string(), Value::from(ts));
    map.insert("body".to_string(), body);
    map.insert(
        "pubKey".to_string(),
        Value::from(B64.encode(signing_key.verifying_key().to_bytes())),
    );
    map.insert(
        "sig".to_string(),
        Value::from(B64.encode(signature.to_bytes())),
    );
    Value::Object(map)
}

/// 信封时间戳新鲜度窗口（±10 分钟，与 node-challenge 窗口口径一致）：
/// ts 参与签名但此前从不校验，重放旧信封可绕过一切内容校验。
pub const ENVELOPE_TS_WINDOW_MS: i64 = 10 * 60_000;

/// 校验通过的入站信封。
#[derive(Clone, Debug)]
pub struct VerifiedDm {
    pub kind: String,
    /// 发送方 rootId（已与 pubKey 绑定校验）。
    pub from: String,
    pub ts: i64,
    pub body: Value,
}

/// 入站信封校验；任一失败返回 `Err(reason)`（reason 供 `{"ok":false,"reason"}`
/// 应答原样回传）。ts 与 `now_ms` 偏差超过 [`ENVELOPE_TS_WINDOW_MS`] 拒绝
/// （reason `stale`，防重放）。
pub fn verify_envelope(payload: &Value, my_root_id: &str, now_ms: i64) -> Result<VerifiedDm, String> {
    let invalid = || "invalid-envelope".to_string();
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let from = payload
        .get("from")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let to = payload
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let ts = payload
        .get("ts")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    let body = payload.get("body").filter(|v| v.is_object()).ok_or_else(invalid)?;
    let pub_key = payload
        .get("pubKey")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let sig = payload
        .get("sig")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;

    if to != my_root_id {
        return Err("not-for-me".to_string());
    }
    // ts 对端可控：非正时间戳直接拒绝；窗口比较用饱和算术，避免
    // `now_ms - ts` 在极端值（如 i64::MIN）下溢出 panic / 回绕绕过窗口
    if ts <= 0 {
        return Err("stale".to_string());
    }
    if now_ms.saturating_sub(ts).saturating_abs() > ENVELOPE_TS_WINDOW_MS {
        return Err("stale".to_string());
    }
    let pub_key_bytes = B64.decode(pub_key).map_err(|_| "bad-pubkey".to_string())?;
    if hex::encode(Sha256::digest(&pub_key_bytes)) != from {
        return Err("bad-pubkey".to_string());
    }
    let signing_payload = build_signing_payload(kind, from, to, ts, body);
    if !verify_ed25519_signature(&signing_payload, sig, pub_key) {
        return Err("bad-signature".to_string());
    }
    Ok(VerifiedDm {
        kind: kind.to_string(),
        from: from.to_string(),
        ts,
        body: body.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 固定私钥对应的 rootId（from = sha256hex(pubKey)）。
    fn test_root_id() -> String {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        hex::encode(Sha256::digest(signing_key.verifying_key().to_bytes()))
    }

    /// 构造合法签名信封（from == to == 本机 rootId，走通全部前置校验）。
    fn signed_envelope(ts: i64) -> (Value, String) {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let from = test_root_id();
        let envelope = build_envelope(KIND_CHAT, &from, &from, ts, json!({}), &signing_key);
        (envelope, from)
    }

    #[test]
    fn rejects_non_positive_and_extreme_ts_without_overflow() {
        let now = 1_720_000_000_000i64;
        for ts in [i64::MIN, -1, 0, i64::MAX] {
            let (envelope, from) = signed_envelope(ts);
            let err = verify_envelope(&envelope, &from, now).unwrap_err();
            assert_eq!(err, "stale", "ts={ts} 应以 stale 拒绝且不溢出");
        }
    }

    #[test]
    fn accepts_ts_at_window_edges() {
        let now = 1_720_000_000_000i64;
        for ts in [now, now - ENVELOPE_TS_WINDOW_MS, now + ENVELOPE_TS_WINDOW_MS] {
            let (envelope, from) = signed_envelope(ts);
            let verified = verify_envelope(&envelope, &from, now)
                .unwrap_or_else(|e| panic!("ts={ts} 窗口边界内应通过，得到 {e}"));
            assert_eq!(verified.ts, ts);
        }
    }

    #[test]
    fn rejects_ts_just_outside_window() {
        let now = 1_720_000_000_000i64;
        for ts in [
            now - ENVELOPE_TS_WINDOW_MS - 1,
            now + ENVELOPE_TS_WINDOW_MS + 1,
        ] {
            let (envelope, from) = signed_envelope(ts);
            let err = verify_envelope(&envelope, &from, now).unwrap_err();
            assert_eq!(err, "stale", "ts={ts} 超出窗口应 stale");
        }
    }
}
