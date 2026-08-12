//! feed 命令单测：直调 *_inner，不依赖 WebView。真内核（tempdir + sled）。

use super::*;
use base64::Engine as _;
use serde_json::json;
use spark_core::contact::{ContactService, FriendRecord};
use spark_core::kernel::KernelConfig;
use spark_core::storage::StorageBackend;

const PASSWORD: &str = "correct-horse-battery";

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

/// 写入一个朋友（feed 通道放行：permission "open"，带可寻址 peer 以便投递）。
fn seed_friend(kernel: &mut Kernel, root_id: &str) {
    use spark_core::contact::PeerRef;
    let friend = FriendRecord {
        root_id: root_id.to_string(),
        permission: "open".to_string(),
        peers: vec![PeerRef {
            peer_id: "peer-1".to_string(),
            addresses: vec![],
        ..Default::default()}],
        ..Default::default()
    };
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::upsert_friend(&mut storage, &friend).unwrap();
}

/// 为收件人写入 dm-e2e 密钥表对端 root 公钥（`peerRootPub`，出站 E2E 读取，
/// 见 social-feed §4.1 root 密钥直接转换；正常路径入站验签时积累，测试直写）。
fn seed_peer_root_pub(kernel: &mut Kernel, peer_root_id: &str) {
    use ed25519_dalek::SigningKey;
    use spark_core::dm_e2e::record_inbound_peer_root_pub;
    use spark_core::p2p::node::system_now_ms;
    let key = SigningKey::from_bytes(&[7; 32]);
    let pub_b64 = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
    let mut storage = kernel.__test_storage().unwrap();
    record_inbound_peer_root_pub(&mut storage, peer_root_id, &pub_b64, "local-node", system_now_ms()).unwrap();
}

/// 写入一条收件箱记录（feed_pull 补读数据源；键 `feed:inbox:{pluginId}:{ts:013}:{feedId}`）。
fn seed_inbox(kernel: &mut Kernel, plugin_id: &str, feed_id: &str, ts: i64, from: &str, payload: serde_json::Value) {
    let rec = json!({
        "from": from,
        "topic": format!("{plugin_id}:posts"),
        "feedId": feed_id,
        "payload": payload,
        "ts": ts,
    });
    let key = format!("feed:inbox:{plugin_id}:{ts:013}:{feed_id}");
    let mut storage = kernel.__test_storage().unwrap();
    storage.put(&key, &rec.to_string()).unwrap();
}

#[test]
fn deliver_topic_prefix_mismatch_rejected() {
    let (_dir, mut kernel) = unlocked_kernel();
    let err = feed_deliver_inner(
        &mut kernel,
        "moments",
        "evil:posts",
        &json!({ "text": "hi" }),
        vec!["bob".to_string()],
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.contains("InvalidTopic") && err.contains("does not match plugin"),
        "前缀非本插件被拒：{err}"
    );
}

/// deliver 参数透传 + 聚合计数：topic 前缀 == pluginId 放行，accepted 按
/// 「放行」口径（朋友放行计数），requested 按入参总数。
#[test]
fn deliver_passes_through_and_counts() {
    let (_dir, mut kernel) = unlocked_kernel();
    let bob = "bb".repeat(32);
    seed_friend(&mut kernel, &bob);
    seed_peer_root_pub(&mut kernel, &bob);

    let out = feed_deliver_inner(
        &mut kernel,
        "moments",
        "moments:posts",
        &json!({ "text": "你好" }),
        vec![bob.clone(), "stranger".to_string()],
        None,
        None,
    )
    .unwrap();
    // requested = 入参总数；accepted = 放行（朋友）数（stranger 静默跳过）
    assert_eq!(out["requested"], json!(2));
    assert_eq!(out["accepted"], json!(1));
}

/// deliver 显式 feedId 透传（缺省壳层生成）。
#[test]
fn deliver_passes_explicit_feed_id() {
    let (_dir, mut kernel) = unlocked_kernel();
    let bob = "bb".repeat(32);
    seed_friend(&mut kernel, &bob);
    seed_peer_root_pub(&mut kernel, &bob);

    let out = feed_deliver_inner(
        &mut kernel,
        "moments",
        "moments:posts",
        &json!({ "text": "hi" }),
        vec![bob.clone()],
        Some("orig-feed"),
        Some("feed-explicit-1"),
    )
    .unwrap();
    assert_eq!(out["requested"], json!(1));
    assert_eq!(out["accepted"], json!(1));
    // 显式 feedId + replyTo 投递成功（E2E 信封 body 为密文，feedId/replyTo 无法
    // 在密文内断言；此处验证显式参数不引发错误且计数正确）
    let storage = kernel.__test_storage().unwrap();
    let pending_prefix = format!("dm:pending:{bob}:");
    let pending: Vec<_> = storage.scan(&spark_core::storage::ScanOptions::prefix(&pending_prefix)).unwrap();
    assert_eq!(pending.len(), 1, "p2p 未启动时投递失败入离线队列（同步落库）");
}

/// S8 遗留：feed:deliver 调用级限流（§9.3，内核单点）——每 (space, pluginId)
/// 60s 内 10 次调用，第 11 次拒绝并回 `RateLimited`。窗口同内核限流器实例。
#[test]
fn deliver_rate_limited_after_quota() {
    let (_dir, mut kernel) = unlocked_kernel();
    let bob = "bb".repeat(32);
    seed_friend(&mut kernel, &bob);
    seed_peer_root_pub(&mut kernel, &bob);
    // 打满配额（10 次，同一窗口）
    for i in 0..10 {
        let out = feed_deliver_inner(
            &mut kernel,
            "moments",
            "moments:posts",
            &json!({ "n": i }),
            vec![bob.clone()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(out["accepted"], json!(1), "第 {i} 次应放行");
    }
    // 第 11 次超限 → RateLimited
    let err = feed_deliver_inner(
        &mut kernel,
        "moments",
        "moments:posts",
        &json!({ "n": 10 }),
        vec![bob.clone()],
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.contains("RateLimited"),
        "超限应回 RateLimited，实际：{err}"
    );
}

/// pull 游标：按 topic 前缀匹配 pluginId，返回 items + 分页 nextCursor。
#[test]
fn pull_cursor_and_next_cursor() {
    let (_dir, mut kernel) = unlocked_kernel();
    seed_inbox(&mut kernel, "moments", "f1", 1000, "alice", json!({ "n": 1 }));
    seed_inbox(&mut kernel, "moments", "f2", 2000, "bob", json!({ "n": 2 }));
    // 其它插件前缀不入本插件分页
    seed_inbox(&mut kernel, "other", "fX", 3000, "carol", json!({ "n": 9 }));

    // limit=1 → 返回 f1 + nextCursor 指向下一条
    let page1 = feed_pull_inner(&kernel, "moments", "moments:posts", None, Some(1)).unwrap();
    let items = page1["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["feedId"], json!("f1"));
    assert!(page1["nextCursor"].is_string());

    // 游标续读 → f2，无 nextCursor
    let cursor = page1["nextCursor"].as_str().unwrap().to_string();
    let page2 = feed_pull_inner(&kernel, "moments", "moments:posts", Some(&cursor), Some(10)).unwrap();
    let items = page2["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["feedId"], json!("f2"));
    assert!(page2.get("nextCursor").is_none(), "末页无 nextCursor");

    // 空收件箱返回空数组
    let (_dir2, empty) = unlocked_kernel();
    let out = feed_pull_inner(&empty, "moments", "moments:posts", None, None).unwrap();
    assert_eq!(out["items"].as_array().unwrap().len(), 0);
}

/// B1：feed 逐 recipient 隔离——混合名单（部分有对端 root 公钥、部分无）：
/// 无公钥收件人 E2E 加密失败被跳过（不计 accepted），有公钥的照常投递。
#[test]
fn deliver_mixed_recipients_skips_encrypt_failure_per_recipient() {
    let (_dir, mut kernel) = unlocked_kernel();
    let has_key = "aa".repeat(32);
    let no_key = "bb".repeat(32);
    // 两个朋友都有可寻址 peer
    seed_friend(&mut kernel, &has_key);
    seed_friend(&mut kernel, &no_key);
    // 只有 has_key 有对端 root 公钥（无公钥者 E2E 加密失败）
    seed_peer_root_pub(&mut kernel, &has_key);

    let out = feed_deliver_inner(
        &mut kernel,
        "moments",
        "moments:posts",
        &json!({ "text": "hi" }),
        vec![has_key.clone(), no_key.clone()],
        None,
        None,
    )
    .unwrap();
    // requested = 入参 2；accepted = 仅成功构造信封的 has_key（no_key 加密失败跳过）
    assert_eq!(out["requested"], json!(2));
    assert_eq!(
        out["accepted"],
        json!(1),
        "无对端 root 公钥的收件人应被逐人跳过，不计 accepted"
    );
    // 只有 has_key 的信封入离线队列（p2p 未启动），no_key 不产生
    let storage = kernel.__test_storage().unwrap();
    let has_pending: Vec<_> = storage
        .scan(&spark_core::storage::ScanOptions::prefix(&format!("dm:pending:{has_key}:")))
        .unwrap();
    assert_eq!(has_pending.len(), 1, "有公钥者正常投递（离线入队）");
    let no_pending: Vec<_> = storage
        .scan(&spark_core::storage::ScanOptions::prefix(&format!("dm:pending:{no_key}:")))
        .unwrap();
    assert_eq!(no_pending.len(), 0, "无公钥者加密失败被跳过，不入队");
}

/// B2：pull 跨插件归属校验——插件读他人收件箱 topic 被拒（防越权读收件箱）。
#[test]
fn pull_cross_plugin_topic_rejected() {
    let (_dir, mut kernel) = unlocked_kernel();
    // 其它插件（other）的收件箱记录
    seed_inbox(&mut kernel, "other", "fX", 3000, "carol", json!({ "n": 9 }));

    // 本插件（moments）拉取 other:posts → topic 前缀非本插件 → 拒绝
    let err = feed_pull_inner(&kernel, "moments", "other:posts", None, None).unwrap_err();
    assert!(
        err.contains("InvalidTopic") && err.contains("does not match plugin"),
        "跨插件 pull 应被拒：{err}"
    );

    // 本插件 topic 前缀匹配 → 放行（但看不到 other 的收件箱）
    let out = feed_pull_inner(&kernel, "moments", "moments:posts", None, None).unwrap();
    assert_eq!(out["items"].as_array().unwrap().len(), 0, "本插件域看不到其它插件收件箱");
}
