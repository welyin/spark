//! feed 纯逻辑层测试：body 校验、收件箱查重/落库/容量、pull 游标、blob 来源登记。

use super::*;
use crate::storage::MemoryStorage;
use serde_json::json;

fn rec(from: &str, feed_id: &str, ts: i64, topic: &str) -> FeedInboxRecord {
    FeedInboxRecord {
        from: from.to_string(),
        topic: topic.to_string(),
        feed_id: feed_id.to_string(),
        payload: json!({ "text": "hello" }),
        reply_to: None,
        ts,
    }
}

// ── body 校验 ─────────────────────────────────────────────────────────

#[test]
fn validate_topic_shape_and_charset() {
    // 合法
    assert!(validate_feed_body("ai-chat:feed", "f1", &json!({}), None).is_ok());
    assert!(validate_feed_body("moments:posts:v2", "f2", &json!({}), Some("f1")).is_ok());
    // 缺 sub / 纯 pluginId
    assert!(validate_feed_body("moments", "f1", &json!({}), None).is_err());
    // 字符集非法（大写/空格/中文）
    assert!(validate_feed_body("Moments:x", "f1", &json!({}), None).is_err());
    assert!(validate_feed_body("moments:x y", "f1", &json!({}), None).is_err());
    // 超长 topic
    let long = format!("{}:x", "a".repeat(FEED_TOPIC_MAX_CHARS + 1));
    assert!(validate_feed_body(&long, "f1", &json!({}), None).is_err());
}

#[test]
fn validate_feed_id_length() {
    assert!(validate_feed_body("p:sub", "", &json!({}), None).is_err(), "空 feedId 拒绝");
    assert!(validate_feed_body("p:sub", &"a".repeat(65), &json!({}), None).is_err(), "超 64 拒绝");
    assert!(validate_feed_body("p:sub", &"a".repeat(64), &json!({}), None).is_ok());
}

#[test]
fn validate_payload_size_limit() {
    // payload 紧凑序列化 ≤ 32 KiB
    let big_payload = json!({ "data": "x".repeat(FEED_PAYLOAD_MAX_BYTES) });
    assert!(validate_feed_body("p:sub", "f", &big_payload, None).is_err(), "超 32 KiB 拒绝");
    let ok_payload = json!({ "data": "x".repeat(1024) });
    assert!(validate_feed_body("p:sub", "f", &ok_payload, None).is_ok());
}

#[test]
fn validate_reply_to_must_be_nonempty() {
    assert!(validate_feed_body("p:sub", "f", &json!({}), Some("")).is_err(), "空 replyTo 拒绝");
    assert!(validate_feed_body("p:sub", "f", &json!({}), Some("f0")).is_ok());
}

// ── 收件箱：查重 / 落库 / 容量 / pull 游标 ─────────────────────────────

#[test]
fn inbox_dedup_by_from_and_feed_id() {
    let mut s = MemoryStorage::new();
    inbox_put(&mut s, "moments", 1000, &rec("a", "f1", 1000, "moments:p")).unwrap();
    assert!(inbox_has(&s, "moments", "a", "f1").unwrap(), "同 (from, feedId) 查重命中");
    assert!(!inbox_has(&s, "moments", "b", "f1").unwrap(), "不同 from 不判重");
    assert!(!inbox_has(&s, "moments", "a", "f2").unwrap(), "不同 feedId 不判重");
    // 不同 pluginId 域隔离
    assert!(!inbox_has(&s, "other", "a", "f1").unwrap());
}

#[test]
fn inbox_put_and_pull_pagination() {
    let mut s = MemoryStorage::new();
    // 同一 pluginId 下两条（ts 递增）
    inbox_put(&mut s, "moments", 1000, &rec("a", "f1", 1000, "moments:p")).unwrap();
    inbox_put(&mut s, "moments", 2000, &rec("b", "f2", 2000, "moments:p")).unwrap();
    // 另一个 pluginId 的记录不进本域
    inbox_put(&mut s, "ai-chat", 3000, &rec("c", "f3", 3000, "ai-chat:sub")).unwrap();

    // pull 第一页（limit 1）：游标补读语义
    let (page1, cursor) = inbox_pull(&s, "moments", None, 1).unwrap();
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].feed_id, "f1", "ts 升序，先出最早的");
    let c = cursor.expect("有下一页应有游标");
    // 第二页（limit 10）从游标后补读
    let (page2, cursor2) = inbox_pull(&s, "moments", Some(&c), 10).unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].feed_id, "f2");
    assert!(cursor2.is_none(), "无更多数据不应有游标");
    // ai-chat 域只看到自己的记录
    let (pa, _) = inbox_pull(&s, "ai-chat", None, 10).unwrap();
    assert_eq!(pa.len(), 1);
    assert_eq!(pa[0].feed_id, "f3");
}

#[test]
fn inbox_capacity_evicts_oldest() {
    let mut s = MemoryStorage::new();
    // 塞满容量 + 1
    for i in 0..(FEED_INBOX_CAP + 5) {
        let feed_id = format!("f{i:03}");
        let ts = 1000 + i as i64;
        inbox_put(&mut s, "moments", ts, &rec("a", &feed_id, ts, "moments:p")).unwrap();
    }
    let (all, _) = inbox_pull(&s, "moments", None, FEED_INBOX_CAP + 100).unwrap();
    assert_eq!(all.len(), FEED_INBOX_CAP, "容量封顶");
    // 最旧的 5 条被淘汰（最早 ts 的 f000..f004）
    assert!(all.iter().all(|r| r.feed_id != "f000"), "最旧被淘汰");
    assert_eq!(all[0].feed_id, "f005", "幸存最旧为第 6 条");
}

// ── feed-blob 来源登记 ────────────────────────────────────────────────

#[test]
fn blob_source_registration_ttl() {
    let mut s = MemoryStorage::new();
    let payload = json!({
        "text": "hi",
        "img": { "$blob": "hashA", "size": 3 },
        "list": [{ "$blob": "hashB" }]
    });
    register_blob_sources(&mut s, "rootA", &payload, 1000).unwrap();
    // 两个 hash 都登记到 rootA
    assert_eq!(blob_source(&s, "hashA", 1000).unwrap().as_deref(), Some("rootA"));
    assert_eq!(blob_source(&s, "hashB", 1000).unwrap().as_deref(), Some("rootA"));
    // 无引用 hash 未登记
    assert_eq!(blob_source(&s, "hashC", 1000).unwrap(), None);
    // TTL 30 天过期
    assert_eq!(
        blob_source(&s, "hashA", 1000 + FEED_BLOB_SRC_TTL_MS + 1).unwrap(),
        None,
        "TTL 过期返回 None"
    );
    // 换一个 from 重新登记（同 hash 幂等覆盖，TTL 重置）
    register_blob_sources(&mut s, "rootB", &payload, 1000 + FEED_BLOB_SRC_TTL_MS).unwrap();
    assert_eq!(
        blob_source(&s, "hashA", 1000 + FEED_BLOB_SRC_TTL_MS).unwrap().as_deref(),
        Some("rootB"),
        "重新登记后来源更新为最新"
    );
}

#[test]
fn blob_source_strict_expiry_reregister_survives() {
    // I1：旧记录严格过期（TTL+1ms）后再登记，必须生效且不被清理分支误删。
    // 回归点：过期登记走「先 put 后清理」时，清理分支用旧 since 会删掉
    // 刚写入的记录（自杀 bug）；修复后 put 与 delete 互斥（过期只清理不刷新）。
    let mut s = MemoryStorage::new();
    let payload = json!({ "img": { "$blob": "hashX", "size": 3 } });
    // 首次登记（ts=1000）
    register_blob_sources(&mut s, "rootOld", &payload, 1000).unwrap();
    assert_eq!(
        blob_source(&s, "hashX", 1000).unwrap().as_deref(),
        Some("rootOld")
    );
    // 严格过期（TTL+1ms）：旧记录已过期，此时再登记新来源应刷新且生效
    let expired_now = 1000 + FEED_BLOB_SRC_TTL_MS + 1;
    register_blob_sources(&mut s, "rootNew", &payload, expired_now).unwrap();
    // 关键断言：新登记必须存活（不被清理分支误删）
    assert_eq!(
        blob_source(&s, "hashX", expired_now).unwrap().as_deref(),
        Some("rootNew"),
        "严格过期后重登记必须生效（不被自杀清理误删）"
    );
}

// ── feed.deliver 调用级限流（§9.3）──────────────────────────────────────

#[test]
fn deliver_rate_limit_allows_quota_and_rejects_excess() {
    let mut limiter = FeedDeliverRateLimiter::default();
    // 第 1..10 次放行
    for i in 0..FEED_DELIVER_RATE_LIMIT {
        assert!(limiter.check("personal", "moments", 1000 + i as i64), "第 {i} 次应放行");
    }
    // 第 11 次超限拒绝
    assert!(!limiter.check("personal", "moments", 1000 + FEED_DELIVER_RATE_LIMIT as i64), "第 11 次超限");
    assert_eq!(limiter.rejected_count("personal", "moments"), 1);
    // 不同 pluginId 独立配额
    assert!(limiter.check("personal", "ai-chat", 1000 + FEED_DELIVER_RATE_LIMIT as i64), "其它插件独立配额");
}

#[test]
fn deliver_rate_limit_window_resets() {
    let mut limiter = FeedDeliverRateLimiter::default();
    let start = 5_000;
    // 打满配额
    for i in 0..FEED_DELIVER_RATE_LIMIT {
        assert!(limiter.check("personal", "moments", start + i as i64));
    }
    // 窗口内仍超限
    assert!(!limiter.check("personal", "moments", start + FEED_DELIVER_RATE_LIMIT as i64 - 1));
    // 跨过 60s 窗口边界 → 重置放行
    let after = start + FEED_DELIVER_RATE_WINDOW_MS;
    assert!(limiter.check("personal", "moments", after), "窗口过期重置");
    assert_eq!(limiter.count("personal", "moments"), 1);
}
