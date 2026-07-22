//! 成员节点信息声明 nodeInfoClaim（对齐 desktop/src/main/organization/node-info-claim.ts）。
//!
//! 防伪绑定：声明携带根公钥与签名，校验方用 `sha256(publicKey) === rootId`
//! 自包含地确认"签名者就是该 rootId 持有者"，无需 PKI。
//!
//! 签名输入 = 固定键序紧凑 JSON（[`build_node_info_claim_payload`]）：
//! `{"type":...,"version":...,"rootId":...,"publicKey":...,"nodeInfo":{"peerId":<peerId ?? null>,"addresses":[...]},"timestamp":...}`
//!
//! ⚠️ 坑（org.md §14.1）：载荷中 `nodeInfo.peerId` 缺省时序列化为 **`null`**
//! （`?? null`）；而线上 claim 对象本身 `peerId: undefined` 时**整个键被丢弃**。
//! 两种序列化不同，验签统一经载荷构造函数归一，故互认无碍。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::types::{OrganizationNodeInfo, is_valid_root_id};

/// nodeInfoClaim 新鲜窗口：±10 min（node-info-claim.ts:14；`Math.abs` 口径，
/// 未来 10 分钟内也接受，与邀请码的"只查过去"不同）。
pub const NODE_INFO_CLAIM_MAX_AGE_MS: i64 = 10 * 60 * 1000;

/// claim 类型标签。
pub const NODE_INFO_CLAIM_TYPE: &str = "spark-node-info-claim";

/// 未签名的 claim 字段（签名载荷的输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfoClaimUnsigned {
    /// 固定 `spark-node-info-claim`。
    pub type_: String,
    /// 固定 1。
    pub version: u32,
    /// 声明者 rootId（签名载荷中使用**原始字符串**，不做归一化——与 TS 一致）。
    pub root_id: String,
    /// 根公钥 base64（原始 32 字节）。
    pub public_key: String,
    /// 声明的节点信息。
    pub node_info: OrganizationNodeInfo,
    /// 声明时间（ms）。
    pub timestamp: i64,
}

/// nodeInfoClaim 完整结构（含签名）。
///
/// serde 形状与 TS 线形一致：`peerId` 缺省时**丢键**（不是 `null`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfoClaim {
    /// 固定 `spark-node-info-claim`。
    #[serde(rename = "type")]
    pub type_: String,
    /// 固定 1。
    pub version: u32,
    /// 声明者 rootId。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 根公钥 base64。
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// 声明的节点信息（`peerId` 可省）。
    #[serde(rename = "nodeInfo")]
    pub node_info: OrganizationNodeInfo,
    /// 声明时间（ms）。
    pub timestamp: i64,
    /// 签名 base64（64 字节）。
    pub signature: String,
}

impl NodeInfoClaim {
    /// 结构校验（`isNodeInfoClaim`，node-info-claim.ts:41-54）。
    ///
    /// Rust 侧大部分类型约束由 serde 反序列化保证；此处补齐 TS 的取值校验
    /// （type/version 常量）。`addresses` 缺数组 / 字段类型错误在反序列化
    /// 阶段即失败，对应 TS 的 `malformed-claim`。
    pub fn is_structurally_valid(&self) -> bool {
        self.type_ == NODE_INFO_CLAIM_TYPE && self.version == 1
    }

    /// 拆出未签名部分。
    pub fn unsigned(&self) -> NodeInfoClaimUnsigned {
        NodeInfoClaimUnsigned {
            type_: self.type_.clone(),
            version: self.version,
            root_id: self.root_id.clone(),
            public_key: self.public_key.clone(),
            node_info: self.node_info.clone(),
            timestamp: self.timestamp,
        }
    }
}

/// JS 字符串 JSON 转义（与 `JSON.stringify(string)` 逐字节一致：
/// `"`、`\` 短转义，`\b \f \n \r \t` 短转义，其余 <0x20 控制字符 `\u00xx` 小写，
/// 非 ASCII 原样输出）。serde_json 的字符串序列化规则相同，直接复用。
fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("string serialization is infallible")
}

/// `buildNodeInfoClaimPayload`（node-info-claim.ts:27-39）：固定键序紧凑 JSON。
///
/// 逐字节构造（不走 serde_json object，避免键序依赖）：
/// - 键序：`type, version, rootId, publicKey, nodeInfo{peerId, addresses}, timestamp`
/// - `nodeInfo.peerId` 缺省 → **`null`**（`?? null`），键恒存在
/// - 数字按 JS Number 序列化（version/timestamp 为整数毫秒，无小数点）
pub fn build_node_info_claim_payload(claim: &NodeInfoClaimUnsigned) -> String {
    let peer_id_json = match &claim.node_info.peer_id {
        Some(p) => json_string(p),
        None => "null".to_string(),
    };
    let addresses_json = claim
        .node_info
        .addresses
        .iter()
        .map(|a| json_string(a))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"type\":{},\"version\":{},\"rootId\":{},\"publicKey\":{},\"nodeInfo\":{{\"peerId\":{},\"addresses\":[{}]}},\"timestamp\":{}}}",
        json_string(&claim.type_),
        claim.version,
        json_string(&claim.root_id),
        json_string(&claim.public_key),
        peer_id_json,
        addresses_json,
        claim.timestamp
    )
}

/// 校验结果（对齐 TS `{ ok, reason? }` 的 reason 字符串）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimVerification {
    /// 校验通过。
    Ok,
    /// 结构不符（type/version/字段形状）。
    MalformedClaim,
    /// 超出 ±10 min 新鲜窗口。
    StaleClaim,
    /// rootId 不是 64 hex。
    InvalidRootId,
    /// publicKey base64 解码失败。
    InvalidPublicKey,
    /// `sha256(publicKey) !== rootId`。
    PublicKeyRootMismatch,
    /// Ed25519 验签失败（含签名/公钥长度非法）。
    InvalidSignature,
}

impl ClaimVerification {
    /// TS reason 字符串。
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::MalformedClaim => Some("malformed-claim"),
            Self::StaleClaim => Some("stale-claim"),
            Self::InvalidRootId => Some("invalid-root-id"),
            Self::InvalidPublicKey => Some("invalid-public-key"),
            Self::PublicKeyRootMismatch => Some("public-key-root-mismatch"),
            Self::InvalidSignature => Some("invalid-signature"),
        }
    }

    /// 是否通过。
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }
}

/// `verifyNodeInfoClaim`（node-info-claim.ts:62-104）纯函数，`now_ms` 注入。
///
/// 按序五步：结构 → 新鲜度（`|now - timestamp| ≤ 10min`）→ rootId 格式 →
/// 身份绑定（`sha256hex(base64decode(publicKey)) === rootId`）→ Ed25519 验签
/// （公钥须 32 字节、签名须 64 字节）。
pub fn verify_node_info_claim(claim: &NodeInfoClaim, now_ms: i64) -> ClaimVerification {
    verify_node_info_claim_with_max_age(claim, now_ms, NODE_INFO_CLAIM_MAX_AGE_MS)
}

/// 带自定义新鲜窗口的 [`verify_node_info_claim`]（对齐 TS `options.maxAgeMs`）。
pub fn verify_node_info_claim_with_max_age(
    claim: &NodeInfoClaim,
    now_ms: i64,
    max_age_ms: i64,
) -> ClaimVerification {
    // 1. 结构校验
    if !claim.is_structurally_valid() {
        return ClaimVerification::MalformedClaim;
    }

    // 2. 新鲜度：Math.abs 口径，未来 10 min 内同样接受
    if (now_ms - claim.timestamp).abs() > max_age_ms {
        return ClaimVerification::StaleClaim;
    }

    // 3. rootId 格式（trim + lowercase 后校验）
    let normalized_root_id = claim.root_id.trim().to_lowercase();
    if !is_valid_root_id(&normalized_root_id) {
        return ClaimVerification::InvalidRootId;
    }

    // 4. 身份绑定：sha256hex(base64decode(publicKey)) === rootId
    //    （TS 不检查解码长度；长度错误自然导致 sha 不匹配）
    let Ok(public_key_bytes) = B64.decode(claim.public_key.as_bytes()) else {
        return ClaimVerification::InvalidPublicKey;
    };
    if hex::encode(Sha256::digest(&public_key_bytes)) != normalized_root_id {
        return ClaimVerification::PublicKeyRootMismatch;
    }

    // 5. Ed25519 验签：载荷（固定键序重建）+ signature(base64) + publicKey(base64)，
    //    公钥须 32 字节、签名须 64 字节
    let Ok(signature_bytes) = B64.decode(claim.signature.as_bytes()) else {
        return ClaimVerification::InvalidSignature;
    };
    let Ok(public_key_arr) = <[u8; 32]>::try_from(public_key_bytes.as_slice()) else {
        return ClaimVerification::InvalidSignature;
    };
    let Ok(signature_arr) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return ClaimVerification::InvalidSignature;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key_arr) else {
        return ClaimVerification::InvalidSignature;
    };
    let signature = Signature::from_bytes(&signature_arr);
    let payload = build_node_info_claim_payload(&claim.unsigned());
    if verifying_key
        .verify(payload.as_bytes(), &signature)
        .is_err()
    {
        return ClaimVerification::InvalidSignature;
    }

    ClaimVerification::Ok
}

/// 用 root 身份私钥构造自签 claim（对齐 bootstrap.ts:28-48 +
/// `signWithRootIdentity`，root-id.ts:747-757）。
///
/// - rootId = sha256hex(公钥)；publicKey = 公钥 base64；timestamp = `now_ms`
/// - 签名输入 = 固定键序载荷 UTF-8；输出 = 64 字节签名 base64
pub fn sign_node_info_claim(
    root_signing_key: &SigningKey,
    node_info: OrganizationNodeInfo,
    now_ms: i64,
) -> NodeInfoClaim {
    let public_key_bytes = root_signing_key.verifying_key().to_bytes();
    let unsigned = NodeInfoClaimUnsigned {
        type_: NODE_INFO_CLAIM_TYPE.to_string(),
        version: 1,
        root_id: hex::encode(Sha256::digest(public_key_bytes)),
        public_key: B64.encode(public_key_bytes),
        node_info,
        timestamp: now_ms,
    };
    let payload = build_node_info_claim_payload(&unsigned);
    let signature = root_signing_key.sign(payload.as_bytes());
    NodeInfoClaim {
        type_: unsigned.type_,
        version: unsigned.version,
        root_id: unsigned.root_id,
        public_key: unsigned.public_key,
        node_info: unsigned.node_info,
        timestamp: unsigned.timestamp,
        signature: B64.encode(signature.to_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{derive_root_identity, parse_mnemonic};

    const NOW: i64 = 1_720_000_000_000;
    const MNEMONIC: &str = "与 祝 产 鸡 永 烂 施 师 蓝 荷 有 邓 朗 防 管 李 原 芳 饿 万 措 走 腰 旅";

    fn test_identity() -> crate::identity::Identity {
        let parsed = parse_mnemonic(MNEMONIC).unwrap();
        derive_root_identity(&parsed.seed)
    }

    fn sample_claim(with_peer: bool) -> NodeInfoClaim {
        let identity = test_identity();
        sign_node_info_claim(
            &identity.signing_key,
            OrganizationNodeInfo {
                peer_id: with_peer.then(|| "12D3KooWSelfPeer".to_string()),
                addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
            },
            NOW,
        )
    }

    #[test]
    fn payload_fixed_key_order_and_null_peer() {
        let claim = sample_claim(false);
        let payload = build_node_info_claim_payload(&claim.unsigned());
        let expected = format!(
            "{{\"type\":\"spark-node-info-claim\",\"version\":1,\"rootId\":\"{}\",\"publicKey\":\"{}\",\"nodeInfo\":{{\"peerId\":null,\"addresses\":[\"/ip4/1.2.3.4/tcp/15002/ws\"]}},\"timestamp\":{}}}",
            claim.root_id, claim.public_key, NOW
        );
        assert_eq!(payload, expected, "peerId 缺省时载荷中必须为 null（?? null 归一）");
        // 线上 claim 对象本身缺 peerId 时丢键（与载荷序列化不同）
        let wire = serde_json::to_string(&claim).unwrap();
        assert!(!wire.contains("peerId"), "线上 claim 缺 peerId 应丢键: {wire}");
    }

    #[test]
    fn payload_with_peer_id() {
        let claim = sample_claim(true);
        let payload = build_node_info_claim_payload(&claim.unsigned());
        assert!(payload.contains("\"peerId\":\"12D3KooWSelfPeer\""));
        assert!(!payload.contains("null"));
    }

    #[test]
    fn sign_verify_roundtrip() {
        for with_peer in [true, false] {
            let claim = sample_claim(with_peer);
            assert_eq!(
                verify_node_info_claim(&claim, NOW),
                ClaimVerification::Ok,
                "with_peer={with_peer}"
            );
            // ±10 min 边界内
            assert!(verify_node_info_claim(&claim, NOW + NODE_INFO_CLAIM_MAX_AGE_MS).is_ok());
            assert!(verify_node_info_claim(&claim, NOW - NODE_INFO_CLAIM_MAX_AGE_MS).is_ok());
        }
    }

    #[test]
    fn verify_rejects_stale() {
        let claim = sample_claim(true);
        assert_eq!(
            verify_node_info_claim(&claim, NOW + NODE_INFO_CLAIM_MAX_AGE_MS + 1),
            ClaimVerification::StaleClaim
        );
        assert_eq!(
            verify_node_info_claim(&claim, NOW - NODE_INFO_CLAIM_MAX_AGE_MS - 1),
            ClaimVerification::StaleClaim
        );
    }

    #[test]
    fn verify_rejects_malformed() {
        let mut claim = sample_claim(true);
        claim.type_ = "other".to_string();
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::MalformedClaim
        );
        let mut claim = sample_claim(true);
        claim.version = 2;
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::MalformedClaim
        );
    }

    #[test]
    fn verify_rejects_invalid_root_id() {
        let mut claim = sample_claim(true);
        claim.root_id = "zz".to_string();
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::InvalidRootId
        );
    }

    #[test]
    fn verify_rejects_invalid_public_key_base64() {
        let mut claim = sample_claim(true);
        claim.public_key = "!!!not-base64!!!".to_string();
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::InvalidPublicKey
        );
    }

    #[test]
    fn verify_rejects_public_key_root_mismatch() {
        // 换一个身份的公钥：base64 合法但 sha256 不匹配 rootId
        let other = parse_mnemonic("legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will").unwrap();
        let other_identity = derive_root_identity(&other.seed);
        let mut claim = sample_claim(true);
        claim.public_key = B64.encode(other_identity.public_key());
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::PublicKeyRootMismatch
        );
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        // 篡改签名内容
        let mut claim = sample_claim(true);
        claim.timestamp = NOW + 1;
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::InvalidSignature
        );
        // 签名长度非法
        let mut claim = sample_claim(true);
        claim.signature = B64.encode([0u8; 32]);
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::InvalidSignature
        );
        // 签名 base64 非法
        let mut claim = sample_claim(true);
        claim.signature = "###".to_string();
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::InvalidSignature
        );
        // 公钥长度非法（32 → 16 字节）：sha256 仍等于 rootId 不可能，
        // 所以落在 mismatch 分支——验证分支顺序
        let mut claim = sample_claim(true);
        claim.public_key = B64.encode([0u8; 16]);
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::PublicKeyRootMismatch
        );
    }

    #[test]
    fn verify_checks_order_structure_before_stale() {
        // 结构错误优先于时间错误
        let mut claim = sample_claim(true);
        claim.version = 2;
        claim.timestamp = NOW - 60 * 60 * 1000;
        assert_eq!(
            verify_node_info_claim(&claim, NOW),
            ClaimVerification::MalformedClaim
        );
    }

    #[test]
    fn custom_max_age() {
        let claim = sample_claim(true);
        assert!(verify_node_info_claim_with_max_age(&claim, NOW + 60_000, 120_000).is_ok());
        assert_eq!(
            verify_node_info_claim_with_max_age(&claim, NOW + 60_000, 30_000),
            ClaimVerification::StaleClaim
        );
    }
}
