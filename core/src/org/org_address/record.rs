//! 组织地址记录（§16.1 线形）、固定键序签名载荷、五步校验链与冲突裁决。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::types::is_valid_root_id;
use super::address::{decode_org_address, org_address_from_public_key};

/// 组织地址记录默认 ttl：24h（§16.1 发布方默认）。
pub const ORG_ADDRESS_RECORD_DEFAULT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// 组织地址记录 ttl 上限：7 天（§16.1）。
pub const ORG_ADDRESS_RECORD_MAX_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// `publishedAt` 未来容忍窗口：10 min（与 claim/announce 同口径，§16.3 第 2 步）。
pub const ORG_ADDRESS_FUTURE_TOLERANCE_MS: i64 = 10 * 60 * 1000;

/// gossip 信封 `type`（p2p-messages.md §16：spark-overlay 主题，`domain='system'`）。
pub const ORG_ADDRESS_GOSSIP_TYPE: &str = "org-address";

/// orgId 格式：`^org_[0-9a-f]{16}$`（§16.3 第 3 步）。
fn is_valid_org_id(org_id: &str) -> bool {
    let Some(hex_part) = org_id.strip_prefix("org_") else {
        return false;
    };
    hex_part.len() == 16
        && hex_part
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ------------------------------------------------------------------
// 组织地址记录（§16.1 线形）
// ------------------------------------------------------------------

/// 未签名的记录字段（签名载荷的输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgAddressRecordUnsigned {
    /// 自认证组织地址（55 字符）。
    pub org_address: String,
    /// `org_<16hex>`。
    pub org_id: String,
    /// 组织根公钥 base64（原始 32 字节）。
    pub org_public_key: String,
    /// 展示名（可省；载荷中缺省序列化为 `null`）。
    pub display_name: Option<String>,
    /// 组织网关 rootId 列表。
    pub gateways: Vec<String>,
    /// 同一根密钥下单调递增的发布序号。
    pub seq: u64,
    /// 发布时间（ms）。
    pub published_at: i64,
    /// 有效期（ms，`0 < ttl ≤ 7 天`）。
    pub ttl: i64,
}

/// 组织地址记录完整结构（含签名）。
///
/// serde 形状与 §16.1 线形一致：`displayName` 缺省时**丢键**（不是 `null`）；
/// 验签统一经 [`build_org_address_record_payload`] 的 `?? null` 归一。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgAddressRecord {
    /// 自认证组织地址。
    #[serde(rename = "orgAddress")]
    pub org_address: String,
    /// `org_<16hex>`。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 组织根公钥 base64（原始 32 字节）。
    #[serde(rename = "orgPublicKey")]
    pub org_public_key: String,
    /// 展示名（可省）。
    #[serde(
        rename = "displayName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    /// 组织网关 rootId 列表。
    #[serde(default)]
    pub gateways: Vec<String>,
    /// 发布序号。
    pub seq: u64,
    /// 发布时间（ms）。
    #[serde(rename = "publishedAt")]
    pub published_at: i64,
    /// 有效期（ms）。
    pub ttl: i64,
    /// 签名 base64（64 字节）。
    pub signature: String,
}

impl OrgAddressRecord {
    /// 拆出未签名部分。
    pub fn unsigned(&self) -> OrgAddressRecordUnsigned {
        OrgAddressRecordUnsigned {
            org_address: self.org_address.clone(),
            org_id: self.org_id.clone(),
            org_public_key: self.org_public_key.clone(),
            display_name: self.display_name.clone(),
            gateways: self.gateways.clone(),
            seq: self.seq,
            published_at: self.published_at,
            ttl: self.ttl,
        }
    }

    /// 序列化为 DHT 记录值 / gossip 载荷的紧凑 JSON 字节。
    pub fn to_record_value(&self) -> Vec<u8> {
        serde_json::to_string(self)
            .unwrap_or_else(|_| "{}".to_string())
            .into_bytes()
    }
}

/// JS 字符串 JSON 转义（与 claim.rs 同款：复用 serde_json 的字符串序列化规则）。
fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("string serialization is infallible")
}

/// 待签名载荷（§16.2）：固定键序紧凑 JSON（不含 `signature`）：
/// `{"orgAddress":...,"orgId":...,"orgPublicKey":...,"displayName":<缺省为 null>,
///   "gateways":[...],"seq":...,"publishedAt":...,"ttl":...}`
///
/// 逐字节构造（不走 serde_json object，避免键序依赖）；`displayName` 缺省 →
/// **`null`**（`?? null` 口径）；数字按 JS Number 序列化（整数毫秒，无小数点）。
pub fn build_org_address_record_payload(record: &OrgAddressRecordUnsigned) -> String {
    let display_name_json = match &record.display_name {
        Some(name) => json_string(name),
        None => "null".to_string(),
    };
    let gateways_json = record
        .gateways
        .iter()
        .map(|g| json_string(g))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"orgAddress\":{},\"orgId\":{},\"orgPublicKey\":{},\"displayName\":{},\"gateways\":[{}],\"seq\":{},\"publishedAt\":{},\"ttl\":{}}}",
        json_string(&record.org_address),
        json_string(&record.org_id),
        json_string(&record.org_public_key),
        display_name_json,
        gateways_json,
        record.seq,
        record.published_at,
        record.ttl
    )
}

/// 校验结果（§16.3 五步按序；reason 字符串与 claim 校验链同风格）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgAddressVerification {
    /// 校验通过。
    Ok,
    /// 结构不符（字段形状 / orgAddress 不可解码 / gateways 元素非 64 hex）。
    MalformedRecord,
    /// ttl 非法（≤0 或超 7 天）/ 已过期 / `publishedAt` 超未来容忍。
    TtlWindow,
    /// orgId 非 `org_<16hex>`。
    InvalidOrgId,
    /// 自认证闭环断裂：`orgPublicKey` 非 32 字节或 `sha256(orgPublicKey) ≠ orgAddress 内嵌 digest`。
    AddressMismatch,
    /// Ed25519 验签失败（含签名/公钥长度非法）。
    InvalidSignature,
}

impl OrgAddressVerification {
    /// reason 字符串。
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::MalformedRecord => Some("malformed-record"),
            Self::TtlWindow => Some("ttl-window"),
            Self::InvalidOrgId => Some("invalid-org-id"),
            Self::AddressMismatch => Some("address-mismatch"),
            Self::InvalidSignature => Some("invalid-signature"),
        }
    }

    /// 是否通过。
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }
}

/// `verifyOrgAddressRecord`（org.md §16.3）纯函数，`now_ms` 注入。
///
/// 按序五步：结构 → ttl 窗口（`0 < ttl ≤ 7 天`、`now ≤ publishedAt + ttl`、
/// `publishedAt ≤ now + 10 min`）→ orgId 格式 → 自认证闭环
/// （`orgPublicKey` base64 解码恰 32 字节且 `sha256(orgPublicKey) == orgAddress
/// 内嵌 digest`）→ Ed25519 验签（§16.2 重建载荷；公钥 32 字节、签名 64 字节）。
pub fn verify_org_address_record(record: &OrgAddressRecord, now_ms: i64) -> OrgAddressVerification {
    // 1. 结构：orgAddress 可解码（含 checksum）；gateways 每项匹配 ^[0-9a-f]{64}$
    let Some(digest) = decode_org_address(&record.org_address) else {
        return OrgAddressVerification::MalformedRecord;
    };
    if !record.gateways.iter().all(|g| is_valid_root_id(g)) {
        return OrgAddressVerification::MalformedRecord;
    }

    // 2. ttl 窗口
    if record.ttl <= 0 || record.ttl > ORG_ADDRESS_RECORD_MAX_TTL_MS {
        return OrgAddressVerification::TtlWindow;
    }
    let Some(expiry) = record.published_at.checked_add(record.ttl) else {
        return OrgAddressVerification::TtlWindow;
    };
    if now_ms > expiry {
        return OrgAddressVerification::TtlWindow;
    }
    if record.published_at > now_ms + ORG_ADDRESS_FUTURE_TOLERANCE_MS {
        return OrgAddressVerification::TtlWindow;
    }

    // 3. orgId 格式
    if !is_valid_org_id(&record.org_id) {
        return OrgAddressVerification::InvalidOrgId;
    }

    // 4. 自认证闭环：orgPublicKey 恰 32 字节且 sha256(orgPublicKey) == orgAddress 内嵌 digest
    let Ok(public_key_bytes) = B64.decode(record.org_public_key.as_bytes()) else {
        return OrgAddressVerification::AddressMismatch;
    };
    let Ok(public_key_arr) = <[u8; 32]>::try_from(public_key_bytes.as_slice()) else {
        return OrgAddressVerification::AddressMismatch;
    };
    if Sha256::digest(public_key_arr).as_slice() != digest {
        return OrgAddressVerification::AddressMismatch;
    }

    // 5. Ed25519 验签：载荷（固定键序重建）+ signature(base64) + orgPublicKey(base64)
    let Ok(signature_bytes) = B64.decode(record.signature.as_bytes()) else {
        return OrgAddressVerification::InvalidSignature;
    };
    let Ok(signature_arr) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return OrgAddressVerification::InvalidSignature;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key_arr) else {
        return OrgAddressVerification::InvalidSignature;
    };
    let signature = Signature::from_bytes(&signature_arr);
    let payload = build_org_address_record_payload(&record.unsigned());
    if verifying_key
        .verify(payload.as_bytes(), &signature)
        .is_err()
    {
        return OrgAddressVerification::InvalidSignature;
    }

    OrgAddressVerification::Ok
}

/// 用组织根私钥构造自签地址记录（§16.2）。
///
/// - orgAddress/orgPublicKey 由密钥导出；签名输入 = 固定键序载荷 UTF-8；
///   输出 = 64 字节签名 base64
pub fn sign_org_address_record(
    org_signing_key: &SigningKey,
    org_id: &str,
    display_name: Option<String>,
    gateways: Vec<String>,
    seq: u64,
    published_at: i64,
    ttl: i64,
) -> OrgAddressRecord {
    let public_key_bytes = org_signing_key.verifying_key().to_bytes();
    let unsigned = OrgAddressRecordUnsigned {
        org_address: org_address_from_public_key(&public_key_bytes),
        org_id: org_id.to_string(),
        org_public_key: B64.encode(public_key_bytes),
        display_name,
        gateways,
        seq,
        published_at,
        ttl,
    };
    let payload = build_org_address_record_payload(&unsigned);
    let signature = org_signing_key.sign(payload.as_bytes());
    OrgAddressRecord {
        org_address: unsigned.org_address,
        org_id: unsigned.org_id,
        org_public_key: unsigned.org_public_key,
        display_name: unsigned.display_name,
        gateways: unsigned.gateways,
        seq: unsigned.seq,
        published_at: unsigned.published_at,
        ttl: unsigned.ttl,
        signature: B64.encode(signature.to_bytes()),
    }
}

/// 冲突裁决（p2p-messages.md §16）：candidate 是否比 current 新——
/// `seq` 最大者优先，`seq` 相同取 `publishedAt` 最新（调用方保证同一 orgAddress）。
pub fn is_newer_org_address_record(
    candidate: &OrgAddressRecord,
    current: &OrgAddressRecord,
) -> bool {
    candidate.seq > current.seq
        || (candidate.seq == current.seq && candidate.published_at > current.published_at)
}
