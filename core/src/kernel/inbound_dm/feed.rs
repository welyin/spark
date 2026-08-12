//! dm 入站编排（feed 系）：社交定向投递入站处理。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §4.2/§8] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.5]。从 `inbound_dm` 拆出的子模块，
//! 共享父模块的 [`InboundContext`]/应答助手/[`is_blocked`]。
//!
//! 验签/解密由分发层（`handle_inbound_dm_inner`）统一完成——本 handler
//! 收到的是**明文 body**。职责：
//!
//! 1. 拉黑检查（`is_blocked` → `blocked`）；
//! 2. body 校验（topic 形态/feedId/payload ≤32 KiB/replyTo）；
//! 3. 收件箱 `(from, feedId)` 查重（幂等：重复回 `ok:true`，不重复落库/事件）；
//! 4. 落收件箱 + feed-blob 来源登记；
//! 5. 派发 `P2pEvent::FeedReceived`（含 topic/from/feedId/payload/ts）。
//!
//! `ts` 用**信封时间戳**（发送方时间，`VerifiedDm.ts`）而非接收方本地
//! `ctx.now_ms`（I6）：离线补投的旧动态按发送方时间落库/展示，SDK 类型注释
//! 的「信封时间戳」语义与之对齐；`ctx.now_ms` 仅用于 blob 来源登记等接收侧
//! 时间语义。
//!
//! feed 不校验组织空间（feed 是个人空间功能）：body 无 spaceKey，直接按
//! personal 空间语义处理。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, Result, done, fail_response, is_blocked, ok_response,
};
use crate::kernel::feed::{
    FeedInboxRecord, inbox_has, inbox_put, plugin_id_of_topic, register_blob_sources,
    validate_feed_body,
};
use crate::p2p::P2pEvent;
use crate::storage::StorageBackend;

/// feed 入站处理（明文 body，验签/解密已由分发层完成）。
///
/// `ts` 为**信封时间戳**（`VerifiedDm.ts`，发送方时间）：feed 落收件箱记录与
/// FeedReceived 事件的时间戳用它（I6），离线补投的旧动态显示发送方时间；
/// `ctx` 仅用于 blob 来源登记/事件派发等接收侧语义。
pub(super) fn handle_feed<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
    ts: i64,
) -> Result<InboundDmResult> {
    // 1. 拉黑：feed 是社交投递，拉黑集合命中直接拒收（§5.1 入站语义）
    if is_blocked(storage, "personal", from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    // 2. body 校验
    let Some(topic) = body.get("topic").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(feed_id) = body.get("feedId").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(payload) = body.get("payload") else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let reply_to = body.get("replyTo").and_then(Value::as_str);
    if let Err(_) = validate_feed_body(topic, feed_id, payload, reply_to) {
        return done(fail_response("invalid-body"), Vec::new());
    }
    let plugin_id = plugin_id_of_topic(topic);
    // 3. 收件箱查重（应用级幂等，§19.5）
    if inbox_has(storage, plugin_id, from, feed_id)? {
        return done(ok_response(), Vec::new());
    }
    let record = FeedInboxRecord {
        from: from.to_string(),
        topic: topic.to_string(),
        feed_id: feed_id.to_string(),
        payload: payload.clone(),
        reply_to: reply_to.map(String::from),
        ts,
    };
    // 4. 落收件箱 + feed-blob 来源登记（扫描 payload 中 {$blob: hash} 引用）。
    // 收件箱键与记录 ts 用信封 ts（发送方时间，I6）；blob 来源登记用 ctx.now_ms
    // （接收侧 LWW/TTL 时间语义）。
    inbox_put(storage, plugin_id, ts, &record)?;
    register_blob_sources(storage, from, payload, ctx.now_ms)?;
    // 5. 派发 FeedReceived 事件（壳层/插件按 topic 前缀路由实时刷新）
    log::info!(
        "[FEED] received topic={} from={} feedId={}",
        topic,
        &from[..std::cmp::min(16, from.len())],
        feed_id
    );
    let mut data = json!({
        "topic": topic,
        "from": from,
        "feedId": feed_id,
        "payload": payload,
        "ts": ts,
    });
    if let Some(r) = reply_to {
        data["replyTo"] = json!(r);
    }
    done(
        ok_response(),
        vec![P2pEvent::FeedReceived(data)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use crate::contact::{ContactService, FriendRecord};
    use crate::kernel::feed::{FEED_INBOX_PREFIX, inbox_pull};
    use crate::storage::{MemoryStorage, ScanOptions};

    /// 空在线 peer 集合（每个测试独立持有，避免返回借用局部临时值）。
    fn ctx<'a>(now: i64, online: &'a HashSet<String>) -> InboundContext<'a> {
        InboundContext {
            my_root_id: "me",
            my_nickname: "我",
            remote_peer_id: "peer-x",
            online_peers: online,
            node_id: "node-me",
            now_ms: now,
            kverify: None,
        }
    }

    fn empty_online() -> HashSet<String> {
        HashSet::new()
    }

    fn body(topic: &str, feed_id: &str, payload: Value, reply_to: Option<&str>) -> Value {
        let mut b = serde_json::json!({
            "topic": topic,
            "feedId": feed_id,
            "payload": payload,
        });
        if let Some(r) = reply_to {
            b["replyTo"] = Value::from(r);
        }
        b
    }

    fn friend(root_id: &str) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            nickname: "朋友".to_string(),
            permission: "open".to_string(),
            ..Default::default()
        }
    }

    fn blocked(storage: &mut MemoryStorage, root_id: &str) {
        use crate::contact::BLOCKED_PREFIX;
        storage.put(&format!("{BLOCKED_PREFIX}{root_id}"), "1").unwrap();
    }

    /// 信封 ts（发送方时间）；测试里与 ctx.now_ms 可不同，验证记录/事件用信封 ts。
    fn env_ts() -> i64 {
        900
    }

    fn inbox_count(s: &MemoryStorage, plugin_id: &str) -> usize {
        s.scan(&ScanOptions::prefix(&format!("{FEED_INBOX_PREFIX}{plugin_id}:")))
            .unwrap()
            .len()
    }

    /// 正常投递：落收件箱 + FeedReceived 事件 + blob 来源登记。
    #[test]
    fn valid_feed_lands_inbox_and_emits_event() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("fromA")).unwrap();
        let payload = json!({ "text": "hi", "img": { "$blob": "hashA" } });
        let result = handle_feed(&mut s, &ctx(1000, &empty_online()), "fromA", &body("moments:p", "f1", payload, None), env_ts()).unwrap();
        assert_eq!(result.response["ok"], json!(true));
        // FeedReceived 事件
        assert_eq!(result.events.len(), 1);
        let P2pEvent::FeedReceived(data) = &result.events[0] else {
            panic!("应派发 FeedReceived");
        };
        assert_eq!(data["topic"], json!("moments:p"));
        assert_eq!(data["from"], json!("fromA"));
        assert_eq!(data["feedId"], json!("f1"));
        assert_eq!(data["payload"]["text"], json!("hi"));
        // I6：事件 ts 用信封时间戳（发送方时间 env_ts=900），非接收方 ctx.now_ms（1000）
        assert_eq!(data["ts"], json!(env_ts()), "事件 ts 应为信封时间戳而非接收方本地时间");
        // 收件箱落库：记录 ts 同样用信封时间戳
        let (items, _) = inbox_pull(&s, "moments", None, 10).unwrap();
        assert_eq!(items[0].ts, env_ts(), "收件箱记录 ts 应为信封时间戳");
        // blob 来源登记（feed 入站扫描 {$blob: hash}）
        assert!(s.get(&crate::kernel::feed::feed_blob_src_key("hashA")).unwrap().is_some());
    }

    /// 拉黑拒收：blocked。
    #[test]
    fn blocked_sender_rejected() {
        let mut s = MemoryStorage::new();
        blocked(&mut s, "evil");
        let result = handle_feed(&mut s, &ctx(1000, &empty_online()), "evil", &body("moments:p", "f1", json!({}), None), env_ts()).unwrap();
        assert_eq!(result.response["reason"], json!("blocked"));
        assert!(result.events.is_empty());
        assert_eq!(inbox_count(&s, "moments"), 0);
    }

    /// invalid-body：topic 缺 sub / payload 超限 / feedId 空。
    #[test]
    fn invalid_body_rejected() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("a")).unwrap();
        // topic 无 sub
        let r = handle_feed(&mut s, &ctx(1000, &empty_online()), "a", &body("moments", "f1", json!({}), None), env_ts()).unwrap();
        assert_eq!(r.response["reason"], json!("invalid-body"));
        // payload 超 32 KiB
        let big = json!({ "d": "x".repeat(crate::kernel::feed::FEED_PAYLOAD_MAX_BYTES) });
        let r = handle_feed(&mut s, &ctx(1000, &empty_online()), "a", &body("moments:p", "f1", big, None), env_ts()).unwrap();
        assert_eq!(r.response["reason"], json!("invalid-body"));
        assert_eq!(inbox_count(&s, "moments"), 0);
    }

    /// 查重：重复 (from, feedId) 幂等回 ok、不重复落库/事件。
    #[test]
    fn duplicate_feed_idempotent() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("a")).unwrap();
        let b = body("moments:p", "f1", json!({ "t": "x" }), None);
        let r1 = handle_feed(&mut s, &ctx(1000, &empty_online()), "a", &b, env_ts()).unwrap();
        assert_eq!(r1.events.len(), 1);
        let r2 = handle_feed(&mut s, &ctx(2000, &empty_online()), "a", &b, env_ts()).unwrap();
        assert_eq!(r2.response["ok"], json!(true));
        assert!(r2.events.is_empty(), "重复投递不重复事件");
        assert_eq!(inbox_count(&s, "moments"), 1, "重复投递不重复落库");
    }

    /// pull 补读：落库后可经 inbox_pull 补读到（插件重启恢复路径）。
    #[test]
    fn inbox_recoverable_via_pull() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("a")).unwrap();
        handle_feed(&mut s, &ctx(1000, &empty_online()), "a", &body("moments:p", "f1", json!({}), None), env_ts()).unwrap();
        handle_feed(&mut s, &ctx(2000, &empty_online()), "a", &body("moments:p", "f2", json!({}), Some("f1")), env_ts()).unwrap();
        let (items, _) = inbox_pull(&s, "moments", None, 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].feed_id, "f1");
        assert_eq!(items[1].feed_id, "f2");
        assert_eq!(items[1].reply_to.as_deref(), Some("f1"));
    }
}
