//! pubsub 信封（P2PMessageBody）与签名体系（对齐 core/spec/p2p-messages.md §3，
//! p2p-node.ts:843-857,950-969）。
//!
//! - 算法：Ed25519（PureEd25519，无预哈希）
//! - 签名密钥：**每次进程启动临时生成，不持久化**；既不是 root 身份也不是 libp2p 节点密钥
//! - `pubKey` = 该临时公钥的 **SPKI DER 的 base64**（与 TS 的 SPKI PEM 等价：
//!   PEM 即 DER 的 base64 加头尾；验签侧两种形态都接受，见 [`decode_envelope_public_key`]）
//! - 签名输入 = 信封去 `signature` 字段后的紧凑 JSON，**键序 = 对象插入序**：
//!   `version` → body 各键 → `evidenceHeadHash`（恒存在，可为 null）→ `timestamp` → `pubKey`
//! - 验签输入：对接收文本 JSON.parse（serde_json preserve_order 保留 wire 键序）后做同样变换
//! - ⚠️ 验签是"自证式"：公钥取自消息自身，只提供完整性/反垃圾门槛，
//!   **不构成任何身份凭证**（不得与 rootId/peerId 关联假设）
//!
//! 数字序列化：JS Number→JSON 规则；本模块时间戳一律取整数毫秒，serde_json
//! 整数输出与 JS 一致（无小数点）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use serde_json::{Map, Value};

use super::{P2pError, Result};

/// 信封版本（固定 "1"）。
pub const ENVELOPE_VERSION: &str = "1";

/// Ed25519 SPKI DER 前缀（`302a300506032b6570032100`），后接 32 字节原始公钥。
const ED25519_SPKI_DER_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// PEM 头（TS 侧 `pubKey` 字段的形态，验签时兼容接受）。
const PEM_HEADER: &str = "-----BEGIN PUBLIC KEY-----";
const PEM_FOOTER: &str = "-----END PUBLIC KEY-----";

/// 原始公钥 → SPKI DER（44 字节）。
pub fn spki_der_from_raw(raw: &[u8; 32]) -> [u8; 44] {
    let mut der = [0u8; 44];
    der[..12].copy_from_slice(&ED25519_SPKI_DER_PREFIX);
    der[12..].copy_from_slice(raw);
    der
}

/// SPKI DER → base64（Rust 内核上线形态）。
pub fn spki_der_base64(raw: &[u8; 32]) -> String {
    B64.encode(spki_der_from_raw(raw))
}

/// SPKI DER → PEM（TS `crypto` 导出形态，64 列折行 + 尾换行）。
pub fn spki_der_pem(raw: &[u8; 32]) -> String {
    let b64 = B64.encode(spki_der_from_raw(raw));
    let mut pem = String::from(PEM_HEADER);
    pem.push('\n');
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        pem.push('\n');
    }
    pem.push_str(PEM_FOOTER);
    pem.push('\n');
    pem
}

/// 解析信封内嵌公钥：接受 base64(SPKI DER)、SPKI PEM、base64(原始 32 字节)。
pub fn decode_envelope_public_key(field: &str) -> Option<VerifyingKey> {
    let trimmed = field.trim();
    let b64 = if trimmed.contains(PEM_HEADER) {
        // PEM：剥掉头尾行与全部空白，得到 DER 的 base64
        trimmed
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .map(str::trim)
            .collect::<String>()
    } else {
        trimmed.to_string()
    };
    let bytes = B64.decode(b64).ok()?;
    if bytes.len() == 44 && bytes[..12] == ED25519_SPKI_DER_PREFIX {
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&bytes[12..]);
        VerifyingKey::from_bytes(&raw).ok()
    } else if bytes.len() == 32 {
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&bytes);
        VerifyingKey::from_bytes(&raw).ok()
    } else {
        None
    }
}

/// 临时信封签名密钥（每次进程启动生成，不持久化）。
#[derive(Clone)]
pub struct EnvelopeSigner {
    signing_key: SigningKey,
    public_key_b64: String,
}

impl EnvelopeSigner {
    /// 随机生成（对齐 TS `crypto.generateKeyPairSync('ed25519')`，每次启动一把）。
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// 从 32 字节种子构造（测试固定向量用）。
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&seed);
        let raw: [u8; 32] = signing_key.verifying_key().to_bytes();
        Self {
            signing_key,
            public_key_b64: spki_der_base64(&raw),
        }
    }

    /// 信封 `pubKey` 字段值（base64 SPKI DER）。
    pub fn public_key(&self) -> &str {
        &self.public_key_b64
    }

    /// 对消息字节做 PureEd25519 签名，输出 base64（64 字节签名）。
    pub fn sign_base64(&self, message: &[u8]) -> String {
        B64.encode(self.signing_key.sign(message).to_bytes())
    }
}

/// 构造 `update` 类信封 body（键序 = TS 调用方书写顺序：
/// `type, domain, collection, id, payload, meta, schema`；
/// `delete` 时 payload 传 `Value::Null`）。
pub fn build_update_body(
    domain: &str,
    collection: &str,
    id: &str,
    payload: Value,
    meta: Value,
    schema: Option<Value>,
) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("type".to_string(), Value::String("update".to_string()));
    body.insert("domain".to_string(), Value::String(domain.to_string()));
    body.insert(
        "collection".to_string(),
        Value::String(collection.to_string()),
    );
    body.insert("id".to_string(), Value::String(id.to_string()));
    body.insert("payload".to_string(), payload);
    body.insert("meta".to_string(), meta);
    if let Some(schema) = schema {
        body.insert("schema".to_string(), schema);
    }
    body
}

/// 构造 `delete` 类信封 body（payload 恒 null，键序同 update）。
pub fn build_delete_body(
    domain: &str,
    collection: &str,
    id: &str,
    meta: Value,
    schema: Option<Value>,
) -> Map<String, Value> {
    let mut body = build_update_body(domain, collection, id, Value::Null, meta, schema);
    body.insert("type".to_string(), Value::String("delete".to_string()));
    body
}

/// 构造 `org-share` / `org-share-ack` 类信封 body（键序 `type, domain, payload`）。
pub fn build_org_body(msg_type: &str, payload: Value) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("type".to_string(), Value::String(msg_type.to_string()));
    body.insert("domain".to_string(), Value::String("system".to_string()));
    body.insert("payload".to_string(), payload);
    body
}

/// pubsub 信封：保持插入序的 JSON 对象。
///
/// 构造顺序（对齐 p2p-node.ts:845-853）：
/// `{ version:'1', ...body, evidenceHeadHash, timestamp }` → `pubKey` → `signature`。
#[derive(Clone, Debug)]
pub struct Envelope {
    map: Map<String, Value>,
}

impl Envelope {
    /// 组装未签名信封：`version` → body 各键（插入序）→ `evidenceHeadHash`（恒存在，
    /// 无存证头时为 `Value::Null`）→ `timestamp`。
    pub fn new(
        body: Map<String, Value>,
        evidence_head_hash: Option<String>,
        timestamp_ms: i64,
    ) -> Self {
        let mut map = Map::new();
        map.insert(
            "version".to_string(),
            Value::String(ENVELOPE_VERSION.to_string()),
        );
        for (key, value) in body {
            map.insert(key, value);
        }
        map.insert(
            "evidenceHeadHash".to_string(),
            evidence_head_hash.map_or(Value::Null, Value::String),
        );
        map.insert("timestamp".to_string(), Value::Number(timestamp_ms.into()));
        Self { map }
    }

    /// 内嵌签名公钥（`pubKey` 键，位于 timestamp 之后）。
    pub fn attach_public_key(&mut self, signer: &EnvelopeSigner) {
        self.map.insert(
            "pubKey".to_string(),
            Value::String(signer.public_key().to_string()),
        );
    }

    /// 签名输入字节 = 信封去 `signature` 键后的紧凑 JSON（UTF-8）。
    pub fn signing_input(&self) -> Vec<u8> {
        let mut copy = self.map.clone();
        copy.remove("signature");
        serde_json::to_string(&Value::Object(copy))
            .expect("envelope is always serializable")
            .into_bytes()
    }

    /// 签名并写入 `signature` 键（最后追加）。
    pub fn sign(&mut self, signer: &EnvelopeSigner) {
        self.attach_public_key(signer);
        let input = self.signing_input();
        let signature = signer.sign_base64(&input);
        self.map
            .insert("signature".to_string(), Value::String(signature));
    }

    /// 发布字节 = 完整信封的紧凑 JSON。
    pub fn to_compact_json(&self) -> String {
        serde_json::to_string(&Value::Object(self.map.clone()))
            .expect("envelope is always serializable")
    }

    /// 访问内部对象。
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.map
    }
}

/// 信封验签结果（自证式，不携带任何身份语义）。
#[derive(Clone, Debug)]
pub struct VerifiedEnvelope {
    /// 解析后的信封对象（保留 wire 键序）。
    pub map: Map<String, Value>,
    /// 信封 `type` 字段。
    pub msg_type: String,
    /// 是否携带签名且验签通过。
    pub signature_valid: bool,
    /// 是否携带 pubKey+signature（无论验签结果）。
    pub signed: bool,
}

/// 解析入站信封文本并（在携带签名时）验签。
///
/// - 解析失败 → `Err(Malformed)`（TS 为告警丢弃）
/// - 携带 pubKey+signature 且验签失败 → `Err(SignatureInvalid)`（所有类型一视同仁）
/// - 未携带签名 → `Ok(signature_valid=false, signed=false)`
pub fn parse_and_verify_envelope(text: &str) -> Result<VerifiedEnvelope> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| P2pError::Malformed(format!("invalid json: {e}")))?;
    let map = match value {
        Value::Object(map) => map,
        _ => return Err(P2pError::Malformed("envelope is not an object".to_string())),
    };
    let msg_type = map
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let pub_key = map.get("pubKey").and_then(Value::as_str);
    let signature = map.get("signature").and_then(Value::as_str);
    let signed = pub_key.is_some() && signature.is_some();
    if !signed {
        return Ok(VerifiedEnvelope {
            map,
            msg_type,
            signature_valid: false,
            signed,
        });
    }

    // 验签输入 = 接收对象移除 signature 键后按 wire 键序再序列化
    // （JSON.parse 保留键序，preserve_order 的 Map 同样保留）。
    let mut unsigned = map.clone();
    unsigned.remove("signature");
    let input = serde_json::to_vec(&Value::Object(unsigned))
        .map_err(|e| P2pError::Malformed(format!("reserialize failed: {e}")))?;

    let verifying_key = decode_envelope_public_key(pub_key.unwrap_or_default())
        .ok_or(P2pError::SignatureInvalid)?;
    let sig_bytes = B64
        .decode(signature.unwrap_or_default())
        .map_err(|_| P2pError::SignatureInvalid)?;
    if sig_bytes.len() != 64 {
        return Err(P2pError::SignatureInvalid);
    }
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);
    let signature = ed25519_dalek::Signature::from_bytes(&sig_arr);
    verifying_key
        .verify(&input, &signature)
        .map_err(|_| P2pError::SignatureInvalid)?;

    Ok(VerifiedEnvelope {
        map,
        msg_type,
        signature_valid: true,
        signed,
    })
}

/// 数据写入类消息类型（强制签名，pubsub-message-handler.ts:68-72）。
pub fn is_signature_mandatory_type(msg_type: &str) -> bool {
    matches!(msg_type, "update" | "delete" | "history-response")
}

#[cfg(test)]
mod tests;
