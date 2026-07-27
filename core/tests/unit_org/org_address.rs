//! 组织地址与地址记录单测（生成解析、载荷、五步校验链、裁决、缓存、根密钥密文）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use spark_core::org::org_address::*;
use spark_core::storage::MemoryStorage;

const NOW: i64 = 1_720_000_000_000;
const ORG_ID: &str = "org_0123456789abcdef";
const ORG_SECRET: &str = "ab";

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn test_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn org_secret() -> String {
    ORG_SECRET.repeat(32)
}

fn sample_record(with_display_name: bool) -> OrgAddressRecord {
    sign_org_address_record(
        &test_key(7),
        ORG_ID,
        with_display_name.then(|| "星火公开组织".to_string()),
        vec![rid('a'), rid('b')],
        1,
        NOW,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    )
}

// --------------------------------------------------------------
// orgAddress 生成与解析
// --------------------------------------------------------------

#[test]
fn org_address_shape_and_roundtrip() {
    let key = test_key(7);
    let address = org_address_from_public_key(&key.verifying_key().to_bytes());
    assert_eq!(address.len(), ORG_ADDRESS_LEN);
    assert!(
        address
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7')),
        "RFC 4648 小写字母表: {address}"
    );
    // 解析返回的 digest == sha256(公钥)
    let digest = decode_org_address(&address).expect("valid address");
    assert_eq!(
        digest.as_slice(),
        Sha256::digest(key.verifying_key().to_bytes()).as_slice()
    );
    assert!(is_valid_org_address(&address));
    // 确定性
    assert_eq!(
        address,
        org_address_from_public_key(&key.verifying_key().to_bytes())
    );
    // 不同公钥 → 不同地址
    assert_ne!(
        address,
        org_address_from_public_key(&test_key(8).verifying_key().to_bytes())
    );
}

#[test]
fn decode_rejects_bad_checksum_and_shape() {
    let address = org_address_from_public_key(&test_key(7).verifying_key().to_bytes());
    // 篡改一个字符（checksum 段或 digest 段都拒绝）
    let mut chars: Vec<char> = address.chars().collect();
    chars[10] = if chars[10] == 'a' { 'b' } else { 'a' };
    assert!(decode_org_address(&chars.into_iter().collect::<String>()).is_none());
    // 大写/长度/非法字符
    assert!(decode_org_address(&address.to_uppercase()).is_none());
    assert!(decode_org_address(&address[..54]).is_none());
    assert!(decode_org_address(&format!("{address}x")).is_none());
    assert!(decode_org_address("!!!!").is_none());
    assert!(decode_org_address("").is_none());
    // trim 容忍
    assert_eq!(
        decode_org_address(&format!("  {address} ")),
        decode_org_address(&address)
    );
}

#[test]
fn dht_key_is_embedded_digest() {
    let key = test_key(7);
    let address = org_address_from_public_key(&key.verifying_key().to_bytes());
    let dht_key = org_address_dht_key(&address).unwrap();
    assert_eq!(
        dht_key,
        Sha256::digest(key.verifying_key().to_bytes()).to_vec()
    );
    assert!(org_address_dht_key("bad").is_none());
}

// --------------------------------------------------------------
// 签名载荷与签/验
// --------------------------------------------------------------

#[test]
fn payload_fixed_key_order_and_null_display_name() {
    let record = sample_record(false);
    let payload = build_org_address_record_payload(&record.unsigned());
    let expected = format!(
        "{{\"orgAddress\":\"{}\",\"orgId\":\"{}\",\"orgPublicKey\":\"{}\",\"displayName\":null,\"gateways\":[\"{}\",\"{}\"],\"seq\":1,\"publishedAt\":{},\"ttl\":{}}}",
        record.org_address,
        ORG_ID,
        record.org_public_key,
        rid('a'),
        rid('b'),
        NOW,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS
    );
    assert_eq!(payload, expected, "displayName 缺省时载荷中必须为 null");
    // 线上记录缺 displayName 时丢键
    let wire = serde_json::to_string(&record).unwrap();
    assert!(
        !wire.contains("displayName"),
        "线上记录缺 displayName 应丢键: {wire}"
    );
}

#[test]
fn payload_with_display_name() {
    let record = sample_record(true);
    let payload = build_org_address_record_payload(&record.unsigned());
    assert!(payload.contains("\"displayName\":\"星火公开组织\""));
    assert!(!payload.contains("null"));
    let wire = serde_json::to_string(&record).unwrap();
    assert!(wire.contains("\"displayName\":\"星火公开组织\""));
}

#[test]
fn sign_verify_roundtrip() {
    for with_name in [true, false] {
        let record = sample_record(with_name);
        assert_eq!(
            verify_org_address_record(&record, NOW),
            OrgAddressVerification::Ok,
            "with_display_name={with_name}"
        );
        // ttl 边界：now == publishedAt + ttl 仍未过期
        assert!(
            verify_org_address_record(&record, NOW + ORG_ADDRESS_RECORD_DEFAULT_TTL_MS).is_ok()
        );
        // 未来容忍：publishedAt ≤ now + 10min
        assert!(verify_org_address_record(&record, NOW - ORG_ADDRESS_FUTURE_TOLERANCE_MS).is_ok());
    }
}

#[test]
fn deterministic_resign() {
    let key = test_key(7);
    let a = sample_record(true);
    let b = sign_org_address_record(
        &key,
        ORG_ID,
        Some("星火公开组织".to_string()),
        vec![rid('a'), rid('b')],
        1,
        NOW,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    assert_eq!(a, b, "Ed25519 确定性签名：同密钥同载荷同签名");
}

#[test]
fn verify_rejects_malformed() {
    // orgAddress checksum 错
    let mut record = sample_record(true);
    let other = org_address_from_public_key(&test_key(9).verifying_key().to_bytes());
    record.org_address = other;
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::AddressMismatch,
        "换一个合法地址（公钥不换）落在闭环断裂而非结构"
    );
    let mut record = sample_record(true);
    record.org_address = "not-an-address".to_string();
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::MalformedRecord
    );
    // gateways 元素非法
    let mut record = sample_record(true);
    record.gateways = vec!["zz".to_string()];
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::MalformedRecord
    );
}

#[test]
fn verify_rejects_ttl_window() {
    let record = sample_record(true);
    // 过期
    assert_eq!(
        verify_org_address_record(&record, NOW + ORG_ADDRESS_RECORD_DEFAULT_TTL_MS + 1),
        OrgAddressVerification::TtlWindow
    );
    // 未来 publishedAt 超容忍
    assert_eq!(
        verify_org_address_record(&record, NOW - ORG_ADDRESS_FUTURE_TOLERANCE_MS - 1),
        OrgAddressVerification::TtlWindow
    );
    // ttl ≤ 0 / 超 7 天
    let mut zero_ttl = sample_record(true);
    zero_ttl.ttl = 0;
    assert_eq!(
        verify_org_address_record(&zero_ttl, NOW),
        OrgAddressVerification::TtlWindow
    );
    let mut huge_ttl = sample_record(true);
    huge_ttl.ttl = ORG_ADDRESS_RECORD_MAX_TTL_MS + 1;
    assert_eq!(
        verify_org_address_record(&huge_ttl, NOW),
        OrgAddressVerification::TtlWindow
    );
    // ttl = 7 天边界通过（用对应 ttl 重签）
    let max_ttl = sign_org_address_record(
        &test_key(7),
        ORG_ID,
        None,
        vec![],
        1,
        NOW,
        ORG_ADDRESS_RECORD_MAX_TTL_MS,
    );
    assert!(verify_org_address_record(&max_ttl, NOW).is_ok());
}

#[test]
fn verify_rejects_invalid_org_id() {
    let mut record = sample_record(true);
    record.org_id = "org_zz".to_string();
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::InvalidOrgId
    );
    let mut record = sample_record(true);
    record.org_id = "ORG_0123456789ABCDEF".to_string();
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::InvalidOrgId
    );
}

#[test]
fn verify_rejects_address_mismatch() {
    // 另一个密钥的公钥：base64 合法、32 字节，但 sha256 ≠ orgAddress digest
    let mut record = sample_record(true);
    record.org_public_key = B64.encode(test_key(8).verifying_key().to_bytes());
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::AddressMismatch
    );
    // 公钥长度非法（16 字节）
    let mut record = sample_record(true);
    record.org_public_key = B64.encode([0u8; 16]);
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::AddressMismatch
    );
    // 公钥 base64 非法
    let mut record = sample_record(true);
    record.org_public_key = "!!!".to_string();
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::AddressMismatch
    );
}

#[test]
fn verify_rejects_tampered_signature() {
    // 篡改字段
    let mut record = sample_record(true);
    record.seq = 2;
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::InvalidSignature
    );
    // 签名长度非法
    let mut record = sample_record(true);
    record.signature = B64.encode([0u8; 32]);
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::InvalidSignature
    );
    // 签名 base64 非法
    let mut record = sample_record(true);
    record.signature = "###".to_string();
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::InvalidSignature
    );
    // 校验顺序：结构错误优先于 ttl 错误
    let mut record = sample_record(true);
    record.org_address = "bad".to_string();
    record.ttl = 0;
    assert_eq!(
        verify_org_address_record(&record, NOW),
        OrgAddressVerification::MalformedRecord
    );
}

#[test]
fn verification_reasons() {
    assert_eq!(OrgAddressVerification::Ok.reason(), None);
    assert_eq!(
        OrgAddressVerification::MalformedRecord.reason(),
        Some("malformed-record")
    );
    assert_eq!(
        OrgAddressVerification::TtlWindow.reason(),
        Some("ttl-window")
    );
    assert_eq!(
        OrgAddressVerification::InvalidOrgId.reason(),
        Some("invalid-org-id")
    );
    assert_eq!(
        OrgAddressVerification::AddressMismatch.reason(),
        Some("address-mismatch")
    );
    assert_eq!(
        OrgAddressVerification::InvalidSignature.reason(),
        Some("invalid-signature")
    );
}

// --------------------------------------------------------------
// 冲突裁决
// --------------------------------------------------------------

#[test]
fn arbitration_seq_then_published_at() {
    let older = sample_record(true);
    // seq 大者优先（publishedAt 更旧也赢）
    let mut newer_seq = sign_org_address_record(
        &test_key(7),
        ORG_ID,
        None,
        vec![],
        2,
        NOW - 1000,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    assert!(is_newer_org_address_record(&newer_seq, &older));
    assert!(!is_newer_org_address_record(&older, &newer_seq));
    // seq 相同取 publishedAt 最新
    let mut same_seq_later = older.clone();
    same_seq_later.published_at = NOW + 1;
    assert!(is_newer_org_address_record(&same_seq_later, &older));
    assert!(!is_newer_org_address_record(&older, &same_seq_later));
    // 完全相同不算更新
    assert!(!is_newer_org_address_record(&older, &older));
    newer_seq.seq = 1;
    newer_seq.published_at = NOW;
    assert!(!is_newer_org_address_record(&newer_seq, &older));
}

// --------------------------------------------------------------
// 缓存
// --------------------------------------------------------------

#[test]
fn cache_arbitration_and_expiry() {
    let mut storage = MemoryStorage::new();
    let record = sample_record(true);
    assert!(cache_org_address_record(&mut storage, &record).unwrap());
    // 同记录重复写入被裁决拒绝
    assert!(!cache_org_address_record(&mut storage, &record).unwrap());
    // 更旧记录被拒绝
    let older = sign_org_address_record(
        &test_key(7),
        ORG_ID,
        None,
        vec![],
        1,
        NOW - 1000,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    assert!(!cache_org_address_record(&mut storage, &older).unwrap());
    // seq 更大者覆盖
    let newer = sign_org_address_record(
        &test_key(7),
        ORG_ID,
        None,
        vec![],
        2,
        NOW,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    assert!(cache_org_address_record(&mut storage, &newer).unwrap());
    let cached = read_cached_org_address_record(&storage, &record.org_address).unwrap();
    assert_eq!(cached.seq, 2);

    // 过期判定
    assert!(!org_address_record_expired(
        &cached,
        NOW + ORG_ADDRESS_RECORD_DEFAULT_TTL_MS
    ));
    assert!(org_address_record_expired(
        &cached,
        NOW + ORG_ADDRESS_RECORD_DEFAULT_TTL_MS + 1
    ));
}

#[test]
fn cache_search_substring_and_expiry_filter() {
    let mut storage = MemoryStorage::new();
    let record = sample_record(true);
    cache_org_address_record(&mut storage, &record).unwrap();
    // 另一条：无 displayName、已过期（publishedAt 很久以前，ttl 内仍签得出来，
    // 但相对搜索 now 已过期）
    let expired = sign_org_address_record(
        &test_key(8),
        ORG_ID,
        Some("别的组织".to_string()),
        vec![],
        1,
        NOW - ORG_ADDRESS_RECORD_DEFAULT_TTL_MS - 1,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    cache_org_address_record(&mut storage, &expired).unwrap();

    // displayName 子串（大小写不敏感）
    let hits = search_cached_org_address_records(&storage, "公开", NOW);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].org_address, record.org_address);
    // orgAddress 子串
    let hits = search_cached_org_address_records(&storage, &record.org_address[..12], NOW);
    assert_eq!(hits.len(), 1);
    // 空关键字列出全部未过期（过期的被滤掉）
    let hits = search_cached_org_address_records(&storage, "", NOW);
    assert_eq!(hits.len(), 1);
    // 未命中
    assert!(search_cached_org_address_records(&storage, "不存在", NOW).is_empty());
}

// --------------------------------------------------------------
// 根密钥对密文
// --------------------------------------------------------------

#[test]
fn seal_open_roundtrip_and_wrong_secret() {
    let key = test_key(7);
    let sealed = seal_org_root_secret(&key, &org_secret());
    let opened = open_org_root_secret(&sealed, &org_secret()).expect("open ok");
    assert_eq!(
        opened.verifying_key().to_bytes(),
        key.verifying_key().to_bytes()
    );
    // 每次加密 nonce 随机 → 密文不同
    assert_ne!(sealed, seal_org_root_secret(&key, &org_secret()));
    // 错误 orgSecret / 坏密文 → None
    assert!(open_org_root_secret(&sealed, &"cd".repeat(32)).is_none());
    assert!(open_org_root_secret("!!!", &org_secret()).is_none());
    assert!(open_org_root_secret(&B64.encode([0u8; 8]), &org_secret()).is_none());
}

#[test]
fn strip_org_root_secret_from_wire_value() {
    let mut value = serde_json::json!({
        "orgId": ORG_ID,
        "orgRootSecret": "ciphertext",
        "orgSecret": "kept",
    });
    strip_org_root_secret(&mut value);
    assert!(value.get("orgRootSecret").is_none());
    assert!(value.get("orgSecret").is_some());
    // 非对象/无该键：空操作
    strip_org_root_secret(&mut serde_json::json!("x"));
}
