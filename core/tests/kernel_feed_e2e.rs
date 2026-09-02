//! feed 编排 + DM 链路 E2E 集成测试（social-feed S6）。
//!
//! 覆盖：
//! - E2E 出站加密 → 入站解密 → feed 落收件箱 + FeedReceived 事件（双端种子
//!   已知，派生双方 dm-e2e 域身份验证完整链路）；
//! - 无 ephPub 明文 feed 兼容入站（原样分发）；
//! - 入站 feed 的 blocked / invalid-body / 查重；
//! - 收件箱落库（经 `feed:inbox:` 键域扫描断言）。

mod common;

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::dm_e2e::{
    derive_session_key_ephemeral, encrypt_body_with_key, generate_ephemeral_keypair,
};
use spark_core::kernel::{dm_envelope, handle_inbound_dm_with_e2e};
use spark_core::p2p::P2pEvent;
use spark_core::p2p::node::system_now_ms;
use spark_core::storage::{MemoryStorage, ScanOptions, StorageBackend};
use spark_core::sync::orgsync::ed_pk_to_x25519;

const NODE: &str = "local-node";

/// 根身份（seed 派生），用于签名信封；E2E 密钥协商用 root 密钥直接转换
/// （2026-08-11 架构师裁决）。
fn root_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// 根身份对应的 rootId（= sha256hex(pubKey)）。
fn root_id_of(key: &SigningKey) -> String {
    hex::encode(Sha256::digest(key.verifying_key().to_bytes()))
}

fn feed_body(topic: &str, feed_id: &str, payload: Value) -> Value {
    json!({ "topic": topic, "feedId": feed_id, "payload": payload })
}

/// 收件箱条目数（`feed:inbox:{pluginId}:` 前缀扫描）。
fn inbox_count(s: &MemoryStorage, plugin_id: &str) -> usize {
    s.scan(&ScanOptions::prefix(&format!("feed:inbox:{plugin_id}:")))
        .unwrap()
        .len()
}

/// 构造 E2E 加密的 feed 信封：发送方用「我方临时私钥 + 对端 root 公钥
/// X25519」派生临时会话密钥加密 body，携带 ephPub 构造签名信封。
fn build_e2e_feed_envelope(
    sender_key: &SigningKey,
    sender_root: &str,
    receiver_key: &SigningKey,
    receiver_root: &str,
    feed_id: &str,
    text: &str,
) -> Value {
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    let peer_x25519 = ed_pk_to_x25519(&receiver_key.verifying_key().to_bytes()).unwrap();
    let session_key =
        derive_session_key_ephemeral(&eph_priv, &peer_x25519, sender_root, receiver_root).unwrap();
    let ts = system_now_ms();
    let body = feed_body("moments:p", feed_id, json!({ "text": text }));
    let encrypted =
        encrypt_body_with_key(&session_key, sender_root, receiver_root, "feed", ts, &body).unwrap();
    dm_envelope::build_envelope_with_eph(
        "feed",
        sender_root,
        receiver_root,
        ts,
        encrypted,
        Some(&B64.encode(eph_pub)),
        sender_key,
    )
}

#[test]
fn e2e_feed_encrypted_lands_inbox_and_emits_event() {
    let sender_key = root_key(2);
    let sender_root = root_id_of(&sender_key);
    let receiver_key = root_key(3);
    let receiver_root = root_id_of(&receiver_key);

    let envelope = build_e2e_feed_envelope(
        &sender_key,
        &sender_root,
        &receiver_key,
        &receiver_root,
        "f-e2e-1",
        "你好朋友圈",
    );

    // 接收方入站：验签 → ephPub 派生解密（我方 root 私钥）→ feed handler
    let mut s = MemoryStorage::new();
    let result = handle_inbound_dm_with_e2e(
        &mut s,
        &receiver_root,
        "我",
        envelope,
        "peer-sender",
        &HashSet::new(),
        system_now_ms(),
        NODE,
        None,
        Some(&receiver_key),
    )
    .unwrap();
    assert_eq!(result.response["ok"], json!(true));
    let P2pEvent::FeedReceived(data) = &result.events[0] else {
        panic!("应派发 FeedReceived");
    };
    assert_eq!(data["topic"], json!("moments:p"));
    assert_eq!(data["from"], json!(sender_root));
    assert_eq!(data["feedId"], json!("f-e2e-1"));
    assert_eq!(data["payload"]["text"], json!("你好朋友圈"));
    assert_eq!(inbox_count(&s, "moments"), 1, "E2E 解密后落收件箱");
}

#[test]
fn e2e_plaintext_feed_inbound_compatible() {
    // 无 ephPub 明文 feed 信封（旧对端）：入站原样分发（E2E 只对 encrypted
    // body 解密，明文不拦截），兼容对端/同步类。
    let sender_key = root_key(2);
    let sender_root = root_id_of(&sender_key);
    let receiver_root = root_id_of(&root_key(3));
    let ts = system_now_ms();
    let body = feed_body("moments:p", "f-plain", json!({ "text": "plain" }));
    let envelope =
        dm_envelope::build_envelope("feed", &sender_root, &receiver_root, ts, body, &sender_key);

    let mut s = MemoryStorage::new();
    let result = handle_inbound_dm_with_e2e(
        &mut s,
        &receiver_root,
        "我",
        envelope,
        "peer-sender",
        &HashSet::new(),
        ts,
        NODE,
        None,
        Some(&root_key(3)),
    )
    .unwrap();
    assert_eq!(result.response["ok"], json!(true), "明文 feed 正常入站");
    assert_eq!(result.events.len(), 1, "明文 feed 仍派发 FeedReceived");
    assert_eq!(inbox_count(&s, "moments"), 1);
}

#[test]
fn feed_inbound_dedup_and_blocked() {
    let sender_key = root_key(2);
    let sender_root = root_id_of(&sender_key);
    let receiver_root = root_id_of(&root_key(3));
    let ts = system_now_ms();
    let mut s = MemoryStorage::new();
    let empty = HashSet::new();

    // 正常收一条明文 feed
    let body = feed_body("moments:p", "f1", json!({ "text": "hi" }));
    let envelope =
        dm_envelope::build_envelope("feed", &sender_root, &receiver_root, ts, body, &sender_key);
    let r = handle_inbound_dm_with_e2e(
        &mut s,
        &receiver_root,
        "我",
        envelope,
        "peer-s",
        &empty,
        ts,
        NODE,
        None,
        Some(&root_key(3)),
    )
    .unwrap();
    assert_eq!(r.events.len(), 1);

    // 重复 (from, feedId)：幂等，不再事件/落库
    let body = feed_body("moments:p", "f1", json!({ "text": "hi" }));
    let envelope =
        dm_envelope::build_envelope("feed", &sender_root, &receiver_root, ts, body, &sender_key);
    let r = handle_inbound_dm_with_e2e(
        &mut s,
        &receiver_root,
        "我",
        envelope,
        "peer-s",
        &empty,
        ts,
        NODE,
        None,
        Some(&root_key(3)),
    )
    .unwrap();
    assert_eq!(r.response["ok"], json!(true));
    assert!(r.events.is_empty(), "重复投递不重复事件");
    assert_eq!(inbox_count(&s, "moments"), 1, "重复投递不重复落库");

    // 拉黑后拒收（拉黑集合键 `ct:blocked:{rootId}`，值 "1"）
    s.put(&format!("ct:blocked:{sender_root}"), "1").unwrap();
    let body = feed_body("moments:p", "f2", json!({ "text": "blocked" }));
    let envelope =
        dm_envelope::build_envelope("feed", &sender_root, &receiver_root, ts, body, &sender_key);
    let r = handle_inbound_dm_with_e2e(
        &mut s,
        &receiver_root,
        "我",
        envelope,
        "peer-s",
        &empty,
        ts,
        NODE,
        None,
        Some(&root_key(3)),
    )
    .unwrap();
    assert_eq!(r.response["reason"], json!("blocked"));
    assert!(r.events.is_empty());
    assert_eq!(inbox_count(&s, "moments"), 1, "被拉黑后不再落收件箱");
}
