//! dm（direct message）直连协议集成测试（本机 loopback 双真实 libp2p 节点）：
//! `/spark/dm/1.0.0` request-response 往返——A 调 `dm_direct` 透明投递 dm 信封，
//! B 的宿主 `handle_dm` 记录并回应答，A 拿到应用层应答 JSON。

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use spark_core::p2p::P2pEvent;
use spark_core::p2p::peer_targets::PeerNodeInfo;

use common::p2p::*;

/// 构造样例 dm 信封（p2p 层不解析字段，仅形状对齐 kernel 层约定）。
fn dm_envelope(kind: &str, from: &str, to: &str, ts: i64, body: Value) -> Value {
    json!({
        "kind": kind,
        "from": from,
        "to": to,
        "ts": ts,
        "body": body,
        "pubKey": "c3BraS1kZXItYmFzZTY0",
        "sig": "c2lnLWJhc2U2NA==",
    })
}

/// chat 信封直连往返：B 记录接收并回 `{"ok": true}`，A 拿到该应答。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_chat_roundtrip() {
    let now = 1_720_000_000_000i64;
    let root_a = "aa".repeat(32);
    let root_b = "bb".repeat(32);
    let (mut a, _state_a, _s_a) = start_node(now, Some(&root_a)).await;
    let (mut b, state_b, _s_b) = start_node(now, Some(&root_b)).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let envelope = dm_envelope("chat", &root_a, &root_b, now, json!({"text": "hello dm"}));
    let response = a
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: dialable(&addrs_b),
            },
            envelope.clone(),
        )
        .await
        .expect("dm_direct ok");
    assert_eq!(response, Some(json!({"ok": true})), "A 拿到 B 的应用层应答");

    // B 侧宿主记录：信封原样（透明搬运），remote_peer_id 为 A 的连接层 peerId
    let dms = state_b.lock().unwrap().dms.clone();
    assert_eq!(dms.len(), 1);
    assert_eq!(dms[0].0, envelope);
    assert_eq!(dms[0].1, a.peer_id());

    a.stop().await;
    b.stop().await;
}

/// friend-request / friend-accept 信封走同一通道（透明搬运与 kind 无关）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_friend_request_roundtrip() {
    let now = 1_720_000_000_000i64;
    let root_a = "aa".repeat(32);
    let root_b = "bb".repeat(32);
    let (mut a, _state_a, _s_a) = start_node(now, Some(&root_a)).await;
    let (mut b, state_b, _s_b) = start_node(now, Some(&root_b)).await;
    let addrs_b = started_addresses(&mut b).await;
    let addrs_a = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // A → B：friend-request
    let request = dm_envelope(
        "friend-request",
        &root_a,
        &root_b,
        now,
        json!({"note": "hi"}),
    );
    let response = a
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: dialable(&addrs_b),
            },
            request.clone(),
        )
        .await
        .expect("friend-request ok");
    assert_eq!(response, Some(json!({"ok": true})));

    // B → A：friend-accept（反向同样可达）
    let accept = dm_envelope("friend-accept", &root_b, &root_a, now, json!({}));
    let response = b
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(a.peer_id().to_string()),
                addresses: dialable(&addrs_a),
            },
            accept.clone(),
        )
        .await
        .expect("friend-accept ok");
    assert_eq!(response, Some(json!({"ok": true})));

    let dms_b = state_b.lock().unwrap().dms.clone();
    assert_eq!(dms_b.len(), 1);
    assert_eq!(dms_b[0].0, request);

    a.stop().await;
    b.stop().await;
}

/// P2pEvent 新变体的线形契约：序列化为 `{kind, data}`（src-tauri forwarder
/// 按 serde_json::to_value 原样转发前端，无需手工映射）。
#[test]
fn p2p_event_dm_variants_serialize_kind_data() {
    let cases = [
        (P2pEvent::ChatReceived(json!({"n": 1})), "ChatReceived"),
        (P2pEvent::ChatStatus(json!({"n": 2})), "ChatStatus"),
        (
            P2pEvent::FriendRequestReceived(json!({"n": 3})),
            "FriendRequestReceived",
        ),
        (
            P2pEvent::FriendRequestAccepted(json!({"n": 4})),
            "FriendRequestAccepted",
        ),
    ];
    for (event, kind) in cases {
        let value = serde_json::to_value(&event).expect("serializable");
        assert_eq!(value["kind"], kind);
        assert!(value["data"].is_object(), "{kind} 应携带 data 对象");
    }
}

/// 对端宿主未实现 handle_dm（默认 Err）时：内部错误文本不外泄，
/// A 只拿到固定 `internal-error` 应答（原始错误走 B 本地 Warning 事件）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_host_not_supported() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    // B 用空宿主（NoopHost）：handle_dm 走默认 Err("dm not supported")
    let storage_b = SharedStorage::new();
    let b = spark_core::p2p::P2pNode::start(
        test_config(now),
        storage_b,
        Box::new(spark_core::p2p::host::NoopHost),
    )
    .await
    .expect("node b starts");
    let mut b = b;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let envelope = dm_envelope("chat", &"aa".repeat(32), &"bb".repeat(32), now, json!({}));
    let response = a
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: dialable(&addrs_b),
            },
            envelope,
        )
        .await
        .expect("dm_direct ok");
    assert_eq!(
        response,
        Some(json!({"ok": false, "reason": "internal-error"})),
        "宿主内部错误对外掩码为 internal-error"
    );

    a.stop().await;
    b.stop().await;
}

/// 已连接 peer 空地址也能直发（已连接短路在拨号目标构建之前）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_connected_peer_empty_addresses() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, state_b, _s_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let envelope = dm_envelope("chat", &"aa".repeat(32), &"bb".repeat(32), now, json!({}));
    let response = a
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: Vec::new(), // 空地址：走已连接短路
            },
            envelope,
        )
        .await
        .expect("dm_direct ok");
    assert_eq!(response, Some(json!({"ok": true})));
    assert_eq!(state_b.lock().unwrap().dms.len(), 1);

    a.stop().await;
    b.stop().await;
}

/// 应答侧限流：同一对端 1s 窗口内第二条请求回 `rate-limited`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_rate_limited() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, state_b, _s_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let target = || PeerNodeInfo {
        peer_id: Some(b.peer_id().to_string()),
        addresses: Vec::new(),
    };
    let envelope = dm_envelope("chat", &"aa".repeat(32), &"bb".repeat(32), now, json!({}));
    let first = a
        .dm_direct(&target(), envelope.clone())
        .await
        .expect("first dm ok");
    assert_eq!(first, Some(json!({"ok": true})));
    // 窗口内第二条：被限流（B 侧 now_fn 固定为 now，窗口判定确定）
    let second = a.dm_direct(&target(), envelope).await.expect("second dm ok");
    assert_eq!(
        second,
        Some(json!({"ok": false, "reason": "rate-limited"}))
    );
    // 被限流的请求不进入宿主
    assert_eq!(state_b.lock().unwrap().dms.len(), 1);

    a.stop().await;
    b.stop().await;
}

/// 非法请求帧（非 JSON 对象 payload）回 `invalid-request`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_direct_invalid_request() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, state_b, _s_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let response = a
        .dm_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: Vec::new(),
            },
            Value::Null, // 序列化为 "null"：非 JSON 对象
        )
        .await
        .expect("dm_direct ok");
    assert_eq!(
        response,
        Some(json!({"ok": false, "reason": "invalid-request"}))
    );
    assert!(state_b.lock().unwrap().dms.is_empty());

    a.stop().await;
    b.stop().await;
}

/// request_id 碰撞回归：org_share_rr 与 dm_rr 的 OutboundRequestId 各自递增，
/// 同 id 并发时响应不得错配到对方协议的 attempt（org ack 误判 dm 送达 /
/// dm 响应误判 org 失败）。并发跑一对 org-share + dm，两者都应正确成交。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dm_and_org_share_concurrent_no_crosstalk() {
    let now = 1_720_000_000_000i64;
    let root_a = "aa".repeat(32);
    let root_b = "bb".repeat(32);
    let (mut a, _state_a, _s_a) = start_node(now, Some(&root_a)).await;
    let (mut b, state_b, _s_b) = start_node(now, Some(&root_b)).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let target = PeerNodeInfo {
        peer_id: Some(b.peer_id().to_string()),
        addresses: dialable(&addrs_b),
    };
    let share_payload = json!({
        "targetRootId": root_b,
        "syncId": "0123456789abcdef01234567",
        "organization": {"orgId": "org_0123456789abcdef", "members": []},
        "pluginDocs": [],
        "nodeInfo": {"peerId": b.peer_id(), "addresses": []},
    });
    let envelope = dm_envelope("chat", &root_a, &root_b, now, json!({"text": "concurrent"}));

    // 节点 A 上两类协议的首个出站请求（两侧 request_id 均为 1），并发等待
    let (share_result, dm_result) = tokio::join!(
        a.org_share_direct(&target, share_payload),
        a.dm_direct(&target, envelope.clone()),
    );
    assert!(share_result.expect("share ok"), "org-share 应送达（未被 dm 响应错配）");
    assert_eq!(
        dm_result.expect("dm ok"),
        Some(json!({"ok": true})),
        "dm 应拿到应答（未被 org ack 错配）"
    );

    // B 侧两类接收都记录
    let state = state_b.lock().unwrap();
    assert!(state.shares.iter().any(|(_, s)| *s == "direct"));
    assert_eq!(state.dms.len(), 1);
    assert_eq!(state.dms[0].0, envelope);

    a.stop().await;
    b.stop().await;
}
