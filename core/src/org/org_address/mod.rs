//! 自认证组织地址（orgAddress）与组织地址记录（org.md §15-16、p2p-messages.md §16）。
//!
//! - **orgAddress = 组织根公钥指纹（自认证地址）**：
//!   `digest = sha256(orgPublicKey 原始 32 字节)`；
//!   `checksum = sha256("spark:org-address:" ‖ digest) 前 2 字节`；
//!   `orgAddress = base32(digest ‖ checksum)`（RFC 4648 字母表，小写，去 padding，
//!   34 字节 → 55 字符）。
//! - **组织地址记录**：`{orgAddress, orgId, orgPublicKey, displayName?, gateways, seq,
//!   publishedAt, ttl, signature}`，组织根 Ed25519 私钥签名；待签名载荷 = 固定键序
//!   紧凑 JSON（`displayName ?? null` 口径，同 nodeInfoClaim 的 `nodeInfo.peerId`）。
//! - **校验链（§16.3 五步）**：结构 → ttl 窗口 → orgId 格式 →
//!   `sha256(orgPublicKey) == orgAddress 内嵌 digest` 自认证闭环 → Ed25519 验签。
//! - **冲突裁决（p2p-messages.md §16）**：同一 orgAddress 取 `seq` 最大者，
//!   `seq` 相同取 `publishedAt` 最新。
//! - **本地缓存**：sled 键前缀 `p2p:org-address:`，尊重记录 `ttl`（过期即失效）。
//!
//! 签名/验签模式照抄 claim.rs（固定键序紧凑 JSON 载荷 + `now_ms` 注入的纯函数）。
//!
//! 代码组织：orgAddress 生成/解析在 `address`，地址记录与五步校验链在 `record`，
//! 本地缓存在 `cache`，组织根密钥对与密文存取在 `root_key`；单测在
//! `core/tests/unit_org/org_address.rs`。

mod address;
mod cache;
mod record;
mod root_key;

pub use address::{
    ORG_ADDRESS_CHECKSUM_DOMAIN, ORG_ADDRESS_LEN, decode_org_address, is_valid_org_address,
    org_address_dht_key, org_address_from_public_key,
};
pub use cache::{
    ORG_ADDRESS_CACHE_PREFIX, cache_org_address_record, org_address_cache_key,
    org_address_record_expired, read_cached_org_address_record, search_cached_org_address_records,
};
pub use record::{
    ORG_ADDRESS_FUTURE_TOLERANCE_MS, ORG_ADDRESS_GOSSIP_TYPE, ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    ORG_ADDRESS_RECORD_MAX_TTL_MS, OrgAddressRecord, OrgAddressRecordUnsigned,
    OrgAddressVerification, build_org_address_record_payload, is_newer_org_address_record,
    sign_org_address_record, verify_org_address_record,
};
pub use root_key::{
    generate_org_root_signing_key, open_org_root_secret, org_root_signing_key,
    seal_org_root_secret, strip_org_root_secret,
};
