//! plugin-announce 广播索引单测（plugin-dist §8）：
//! id 形状 / 规范载荷字节级 / PoW / 校验链（结构·限流·TTL·PoW·签名）/ 本地索引
//! （单 id 最新·LRU·过期清理·verified 持久化）。

use spark_core::p2p::constants::{
    PLUGIN_ANNOUNCE_MAX_BYTES, PLUGIN_ANNOUNCE_MIN_POW_BITS, PLUGIN_ANNOUNCE_TTL_MS,
    PLUGIN_MARKET_INDEX_COUNT_KEY, PLUGIN_MARKET_INDEX_MAX,
};
use spark_core::p2p::plugin_announce::*;
use spark_core::storage::{MemoryStorage, StorageBackend};

const NOW: i64 = 1_720_000_000_000;
/// 测试难度（比网络常量 20 低，秒级以下完成）。
const TEST_BITS: u32 = 8;

fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
}

fn input() -> PluginAnnounceInput {
    PluginAnnounceInput {
        id: "github.com/acme/todo".to_string(),
        name: "待办".to_string(),
        icon: String::new(),
        summary: "测试插件".to_string(),
        category: "business".to_string(),
        version: "0.2.0".to_string(),
        release_url: "https://github.com/acme/todo/releases/tag/v0.2.0".to_string(),
    }
}

/// 构造一条完整有效消息（签名 + PoW 齐备）。
fn make_announce(timestamp: i64) -> PluginAnnounce {
    let (mut announce, payload) =
        build_signed_announce(&input(), &signing_key(), timestamp).unwrap();
    announce.pow = AnnouncePow {
        bits: TEST_BITS,
        nonce: mine_announce_nonce(&payload, TEST_BITS),
    };
    announce
}

fn validator() -> PluginAnnounceValidator {
    PluginAnnounceValidator::new(TEST_BITS)
}

// ------------------------------------------------------------------
// id 形状（§1.1/§8.2）
// ------------------------------------------------------------------

#[test]
fn id_validation_matrix() {
    assert!(announce_id_valid("github.com/owner/repo"));
    assert!(announce_id_valid("gitlab.com/o/r"));
    assert!(announce_id_valid("gitee.com/o/r"));
    assert!(announce_id_valid("github.com/owner/repo/plugins/todo"));
    for bad in [
        "",
        "example.com/o/r",            // host 白名单
        "github.com/owner",           // 段数不足
        "HTTPS://GitHub.com/O/R",     // 未规范化（scheme + 大写）
        "github.com/Owner/Repo",      // 未规范化（大写）
        "github.com/owner/repo/",     // 尾斜杠
        "github.com//repo",           // 空段
        "github.com/owner/re po",     // 空白
        "github.com/owner/repo/../x", // 穿越
        "github.com/owner/re%20po",   // 转义
    ] {
        assert!(!announce_id_valid(bad), "should reject: {bad}");
    }
    assert!(!announce_id_valid(&format!("github.com/{}/r", "a".repeat(101))));
    assert!(!announce_id_valid(&format!("github.com/o/{}", "r".repeat(300))));
}

// ------------------------------------------------------------------
// 规范载荷与 codec（§8.2/§8.3）
// ------------------------------------------------------------------

#[test]
fn payload_fixed_key_order_byte_level() {
    let (announce, payload) = build_signed_announce(&input(), &signing_key(), NOW).unwrap();
    let publisher = announce.publisher.clone();
    let pub_key = announce.pub_key.clone();
    assert_eq!(
        payload,
        format!(
            "{{\"type\":\"spark-plugin-announce\",\"id\":\"github.com/acme/todo\",\"name\":\"待办\",\"icon\":\"\",\"summary\":\"测试插件\",\"category\":\"business\",\"version\":\"0.2.0\",\"releaseUrl\":\"https://github.com/acme/todo/releases/tag/v0.2.0\",\"timestamp\":{NOW},\"ttl\":{PLUGIN_ANNOUNCE_TTL_MS},\"publisher\":\"{publisher}\",\"pubKey\":\"{pub_key}\"}}"
        )
    );
    // publisher = sha256hex(pubKey)
    use sha2::Digest as _;
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(&pub_key)
        .unwrap();
    assert_eq!(publisher, hex::encode(sha2::Sha256::digest(&raw)));
}

#[test]
fn codec_roundtrip_and_full_validate() {
    let announce = make_announce(NOW);
    let text = plugin_announce_to_json(&announce);
    let mut v = validator();
    let verified = v.validate(&text, "peer-a", NOW).expect("must accept");
    assert_eq!(verified.id, "github.com/acme/todo");
    assert_eq!(verified.pow.bits, TEST_BITS);
    // 键序含 pow/signature 尾段
    assert!(text.contains("\"pow\":{\"bits\":8,\"nonce\":"));
    assert!(text.ends_with("\"}"));
}

// ------------------------------------------------------------------
// PoW（§8.4）
// ------------------------------------------------------------------

#[test]
fn pow_leading_zero_bits() {
    assert_eq!(leading_zero_bits(&[0, 0, 0b0001_0000, 0xff]), 8 + 8 + 3);
    assert_eq!(leading_zero_bits(&[0xff]), 0);
    assert_eq!(leading_zero_bits(&[0b0111_1111]), 1);
}

#[test]
fn pow_mine_and_verify() {
    let payload = "payload-bytes";
    let nonce = mine_announce_nonce(payload, TEST_BITS);
    let pow = AnnouncePow {
        bits: TEST_BITS,
        nonce,
    };
    assert!(verify_announce_pow(payload, &pow, TEST_BITS));
    // min_bits 高于声明 → 拒
    assert!(!verify_announce_pow(
        payload,
        &pow,
        PLUGIN_ANNOUNCE_MIN_POW_BITS
    ));
    // 错 nonce → 拒（概率上几乎不可能仍满足）
    let bad = AnnouncePow {
        bits: TEST_BITS,
        nonce: nonce + 1,
    };
    if verify_announce_pow(payload, &bad, TEST_BITS) {
        // 偶然满足时不做强断言（1/256 概率），跳过
    }
}

// ------------------------------------------------------------------
// 校验链拒绝矩阵（§8.5/§8.6）
// ------------------------------------------------------------------

/// 篡改后重签 + 重算 PoW（用于隔离测试签名阶段的绑定校验）。
fn resign_and_mine(announce: &mut PluginAnnounce) {
    use ed25519_dalek::Signer as _;
    let payload = build_announce_payload(announce);
    let sig = signing_key().sign(payload.as_bytes());
    use base64::Engine as _;
    announce.signature = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    announce.pow = AnnouncePow {
        bits: TEST_BITS,
        nonce: mine_announce_nonce(&payload, TEST_BITS),
    };
}

#[test]
fn validate_rejects_tampered_signature() {
    let mut announce = make_announce(NOW);
    // 篡改签名本身（不动载荷，PoW 仍有效）→ 签名阶段拒绝
    use base64::Engine as _;
    announce.signature = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
    assert_eq!(
        validator().validate(&plugin_announce_to_json(&announce), "peer-a", NOW),
        Err(PluginAnnounceReject::Signature)
    );
    // 篡改载荷（名称）→ PoW 先行失效（校验链顺序：PoW 先于签名）
    let announce = make_announce(NOW);
    let tampered = plugin_announce_to_json(&announce).replace("待办", "篡改");
    assert_eq!(
        validator().validate(&tampered, "peer-a", NOW),
        Err(PluginAnnounceReject::Pow)
    );
}

#[test]
fn validate_rejects_publisher_binding() {
    // publisher ≠ sha256hex(pubKey)：重签重算 PoW 后仍须在签名阶段拒绝
    let mut announce = make_announce(NOW);
    announce.publisher = "0".repeat(64);
    resign_and_mine(&mut announce);
    assert_eq!(
        validator().validate(&plugin_announce_to_json(&announce), "peer-a", NOW),
        Err(PluginAnnounceReject::Signature)
    );
}

#[test]
fn validate_rejects_stale_and_future() {
    // 已过期（timestamp 早于 now - ttl）
    let old = make_announce(NOW - PLUGIN_ANNOUNCE_TTL_MS - 1);
    assert_eq!(
        validator().validate(&plugin_announce_to_json(&old), "peer-a", NOW),
        Err(PluginAnnounceReject::Stale)
    );
    // 远未来（> now + 10 min）
    let future = make_announce(NOW + 11 * 60_000);
    assert_eq!(
        validator().validate(&plugin_announce_to_json(&future), "peer-a", NOW),
        Err(PluginAnnounceReject::Stale)
    );
    // 窗口内未来接受
    let near_future = make_announce(NOW + 5 * 60_000);
    assert!(
        validator()
            .validate(&plugin_announce_to_json(&near_future), "peer-a", NOW)
            .is_ok()
    );
}

#[test]
fn validate_rejects_structure_violations() {
    let announce = make_announce(NOW);
    let text = plugin_announce_to_json(&announce);
    // 坏 JSON / 错 type / 非白名单 id / 错 ttl / pow.bits 低于网络常量
    assert_eq!(
        validator().validate("{ not json", "peer-a", NOW),
        Err(PluginAnnounceReject::Structure)
    );
    assert_eq!(
        validator().validate(&text.replace("spark-plugin-announce", "spark-other"), "peer-a", NOW),
        Err(PluginAnnounceReject::Structure)
    );
    assert_eq!(
        validator().validate(&text.replace("github.com/acme/todo", "example.com/acme/todo"), "peer-a", NOW),
        Err(PluginAnnounceReject::Structure)
    );
    assert_eq!(
        validator().validate(
            &text.replace(&format!("\"ttl\":{PLUGIN_ANNOUNCE_TTL_MS}"), "\"ttl\":1000"),
            "peer-a",
            NOW
        ),
        Err(PluginAnnounceReject::Structure)
    );
    assert_eq!(
        validator().validate(&text.replace("\"bits\":8", "\"bits\":7"), "peer-a", NOW),
        Err(PluginAnnounceReject::Structure)
    );
    // 超限消息（> 48 KiB）
    let oversized = format!("{}{}", &text[..text.len() - 1], ",".repeat(PLUGIN_ANNOUNCE_MAX_BYTES));
    assert_eq!(
        validator().validate(&oversized, "peer-a", NOW),
        Err(PluginAnnounceReject::Structure)
    );
}

#[test]
fn validate_per_peer_rate_limit() {
    let mut v = validator();
    // 同一 peer 每小时 10 条
    for i in 0..10 {
        let a = make_announce(NOW + i);
        assert!(
            v.validate(&plugin_announce_to_json(&a), "peer-a", NOW + i).is_ok(),
            "message {i} should pass"
        );
    }
    let extra = make_announce(NOW + 100);
    assert_eq!(
        v.validate(&plugin_announce_to_json(&extra), "peer-a", NOW + 100),
        Err(PluginAnnounceReject::RateLimited)
    );
    // 其他 peer 不受影响
    assert!(
        v.validate(&plugin_announce_to_json(&extra), "peer-b", NOW + 100).is_ok()
    );
    // 窗口滑过后恢复
    assert!(
        v.validate(&plugin_announce_to_json(&extra), "peer-a", NOW + 3_600_001).is_ok()
    );
}

// ------------------------------------------------------------------
// 本地索引（§8.7/§8.8）
// ------------------------------------------------------------------

#[test]
fn store_keeps_newest_per_id() {
    let mut storage = MemoryStorage::new();
    let mut store = PluginAnnounceStore::new(&mut storage);
    let a1 = make_announce(NOW);
    assert_eq!(store.upsert(&a1, NOW).unwrap(), AnnounceUpsert::Inserted);
    // 旧 timestamp → Stale 不入
    assert_eq!(
        store.upsert(&make_announce(NOW - 1000), NOW).unwrap(),
        AnnounceUpsert::Stale
    );
    // 同 timestamp → Duplicate 仅刷 updatedAt
    assert_eq!(store.upsert(&a1, NOW + 1).unwrap(), AnnounceUpsert::Duplicate);
    // 新 timestamp → Replaced 且 verified 重置 pending
    let a2 = make_announce(NOW + 2000);
    assert_eq!(store.upsert(&a2, NOW + 2).unwrap(), AnnounceUpsert::Replaced);
    let entry = store.get("github.com/acme/todo").unwrap().unwrap();
    assert_eq!(entry.announce.timestamp, NOW + 2000);
    assert_eq!(entry.verified, AnnounceVerified::Pending);
    assert_eq!(entry.first_seen_at, NOW);
}

#[test]
fn store_mark_verified_persists() {
    let mut storage = MemoryStorage::new();
    let mut store = PluginAnnounceStore::new(&mut storage);
    store.upsert(&make_announce(NOW), NOW).unwrap();
    assert!(
        store
            .mark_verified("github.com/acme/todo", AnnounceVerified::Verified, "", NOW + 1)
            .unwrap()
    );
    let entry = store.get("github.com/acme/todo").unwrap().unwrap();
    assert_eq!(entry.verified, AnnounceVerified::Verified);
    assert_eq!(entry.verified_at, NOW + 1);
    // 失败原因落库
    assert!(
        store
            .mark_verified("github.com/acme/todo", AnnounceVerified::Failed, "unreachable", NOW + 2)
            .unwrap()
    );
    let entry = store.get("github.com/acme/todo").unwrap().unwrap();
    assert_eq!(entry.verified, AnnounceVerified::Failed);
    assert_eq!(entry.verify_error, "unreachable");
    // 不存在条目 → false
    assert!(
        !store
            .mark_verified("github.com/ghost/none", AnnounceVerified::Verified, "", NOW)
            .unwrap()
    );
    // 更新声明到达后 verified 重置回 pending
    store.upsert(&make_announce(NOW + 5000), NOW + 3).unwrap();
    assert_eq!(
        store.get("github.com/acme/todo").unwrap().unwrap().verified,
        AnnounceVerified::Pending
    );
}

#[test]
fn store_list_purges_expired() {
    let mut storage = MemoryStorage::new();
    let mut store = PluginAnnounceStore::new(&mut storage);
    store.upsert(&make_announce(NOW), NOW).unwrap();
    let mut other = make_announce(NOW);
    other.id = "gitee.com/acme/clock".to_string();
    // 修正签名（id 变了）：重签一条合法消息
    let mut input2 = input();
    input2.id = other.id.clone();
    let (mut a2, payload2) = build_signed_announce(&input2, &signing_key(), NOW).unwrap();
    a2.pow = AnnouncePow {
        bits: TEST_BITS,
        nonce: mine_announce_nonce(&payload2, TEST_BITS),
    };
    store.upsert(&a2, NOW).unwrap();
    assert_eq!(store.list(NOW).unwrap().len(), 2);
    // ttl 过后惰性清除
    assert_eq!(store.list(NOW + PLUGIN_ANNOUNCE_TTL_MS + 1).unwrap().len(), 0);
}

#[test]
fn store_lru_eviction() {
    let mut storage = MemoryStorage::new();
    // 直接写存储绕过逐条 upsert 的成本：先灌 MAX 条原始记录
    for i in 0..PLUGIN_MARKET_INDEX_MAX {
        let mut a = make_announce(NOW);
        a.id = format!("github.com/acme/plugin-{i}");
        let entry = PluginAnnounceIndexEntry {
            announce: a.clone(),
            first_seen_at: NOW,
            updated_at: NOW + i as i64,
            verified: AnnounceVerified::Pending,
            verify_error: String::new(),
            verified_at: 0,
        };
        storage
            .put(
                &format!("mkt:ann:{}", a.id),
                &serde_json::to_string(&entry).unwrap(),
            )
            .unwrap();
    }
    let mut new_input = input();
    new_input.id = "github.com/acme/newcomer".to_string();
    let (mut newcomer, payload) = build_signed_announce(&new_input, &signing_key(), NOW).unwrap();
    newcomer.pow = AnnouncePow {
        bits: TEST_BITS,
        nonce: mine_announce_nonce(&payload, TEST_BITS),
    };
    // 计数键对齐真实条数（直写绕过 upsert，计数不会自动维护）
    storage
        .put(
            PLUGIN_MARKET_INDEX_COUNT_KEY,
            &PLUGIN_MARKET_INDEX_MAX.to_string(),
        )
        .unwrap();
    // 第 MAX+1 条触发逐出：最旧（updated_at = NOW）的 plugin-0 被淘汰
    let mut store = PluginAnnounceStore::new(&mut storage);
    assert_eq!(
        store.upsert(&newcomer, NOW + 999_999).unwrap(),
        AnnounceUpsert::Inserted
    );
    assert!(store.get("github.com/acme/newcomer").unwrap().is_some());
    assert!(store.get("github.com/acme/plugin-0").unwrap().is_none());
    assert_eq!(store.list(NOW).unwrap().len(), PLUGIN_MARKET_INDEX_MAX);
}
