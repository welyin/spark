//! p2p 基础协议集成测试（本机 loopback 双真实 libp2p 节点，Rust↔Rust）：
//! gossipsub 消息收发 + 信封验签、node-announce 交换、peer-exchange 请求响应、
//! org-recovery 命中、org-share 直连推送与 pubsub 推送 + ack、keepalive tick。

mod common;

use std::time::Duration;

use serde_json::json;
use spark_core::org::recovery::{RecoveryViewItem, active_recovery_tokens};
use spark_core::org::types::OrganizationNodeInfo;
use spark_core::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::p2p::{KeepaliveStats, P2pEvent, build_org_body, build_update_body};

use common::p2p::*;

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// gossipsub 消息收发 + 信封验签（强制签名类型端到端落库）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gossipsub_envelope_roundtrip() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, state_b, _s_b) = start_node(now, None).await;
    let addrs_a = started_addresses(&mut a).await;
    let addrs_b = started_addresses(&mut b).await;
    let b_peer = b.peer_id().to_string();

    connect(&a, &b_peer, &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    let _ = addrs_a;

    let body = build_update_body(
        "notes",
        "items",
        "doc-1",
        json!({"text": "hello"}),
        json!({"vv": {"nodeA": 1}, "ts": now, "nodeId": a.peer_id()}),
        None,
    );
    let applied = state_b.clone();
    broadcast_until(&a, "spark-sync", body, move || {
        !applied.lock().unwrap().applied.is_empty()
    })
    .await;

    let applied = state_b.lock().unwrap().applied.clone();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].0, "notes");
    assert_eq!(applied[0].1, "items");
    assert_eq!(applied[0].2, "doc-1");
    assert_eq!(applied[0].3, json!({"text": "hello"}));

    // 版本探测：连接后双方应观察到对端 appVersion
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !state_b
        .lock()
        .unwrap()
        .versions
        .iter()
        .any(|(_, v)| v == "9.9.9-test")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "version not observed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    a.stop().await;
    b.stop().await;
}

/// node-announce：发布 → 对端验签通过并 verified 入池。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_announce_exchange() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;
    let a_peer = a.peer_id().to_string();

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 发布重试直到 B 接受（订阅传播需时）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let accepted = loop {
        a.announce_now().await.expect("announce ok");
        tokio::time::sleep(Duration::from_millis(300)).await;
        // B 的事件流里可能已有 AnnounceAccepted
        // 用邻居池判定更直接
        let mut guard = storage_b.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        let hit = store
            .get(&a_peer)
            .ok()
            .flatten()
            .is_some_and(|r| r.verified);
        drop(guard);
        if hit {
            break true;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "announce not accepted in time"
        );
    };
    assert!(accepted);

    a.stop().await;
    b.stop().await;
}

/// peer-exchange：响应侧抽样 + 请求侧合并入池。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_exchange_request_response() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    // 预置：B 的邻居池里有一个第三方线索（C）
    {
        let mut guard = storage_b.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        store
            .remember(
                "12D3KooWFakePeerC1234567890",
                &["/ip4/10.9.8.7/tcp/15002/ws".to_string()],
                OverlayPeerSource::Announce,
                true,
                now,
            )
            .unwrap();
    }

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let merged = a
        .exchange_with_peer(b.peer_id())
        .await
        .expect("exchange ok");
    assert_eq!(merged, 1, "C should be exchanged to A");

    // A 的邻居池应有 C（未验证来源）
    {
        let mut guard = storage_a.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        let record = store
            .get("12D3KooWFakePeerC1234567890")
            .unwrap()
            .expect("C in A pool");
        assert!(!record.verified);
        assert_eq!(record.source, OverlayPeerSource::Exchange);
    }

    a.stop().await;
    b.stop().await;
}

/// org-recovery：token 命中返回成员地址；未命中回空。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_recovery_hit() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, state_b, _s_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    // B 的恢复视图：org + secret + 一个成员地址（固定 now → token 确定）
    state_b.lock().unwrap().recovery_view = vec![RecoveryViewItem {
        org_id: "org_0123456789abcdef".to_string(),
        recovery_secret: "ef".repeat(32),
        member_node_infos: vec![OrganizationNodeInfo {
            peer_id: Some("12D3KooWMemberX".to_string()),
            addresses: vec!["/ip4/10.1.2.3/tcp/15002/ws".to_string()],
        }],
    }];

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 命中
    let [token, _] = active_recovery_tokens("org_0123456789abcdef", &"ef".repeat(32), now);
    let found = a
        .query_recovery(&token, vec![b.peer_id().to_string()], 8)
        .await
        .expect("query ok");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].peer_id.as_deref(), Some("12D3KooWMemberX"));
    assert_eq!(
        found[0].addresses,
        vec!["/ip4/10.1.2.3/tcp/15002/ws".to_string()]
    );

    // 未命中（B 只连 A，无其他邻居可转发）→ 回空
    let [wrong_token, _] = active_recovery_tokens("org_ffffffffffffffff", &"00".repeat(32), now);
    let found = a
        .query_recovery(&wrong_token, vec![b.peer_id().to_string()], 8)
        .await
        .expect("query ok");
    assert!(found.is_empty());

    a.stop().await;
    b.stop().await;
}

/// org-recovery 多跳转发：A → B（未命中）→ C（命中）。覆盖转发批次**首个**
/// 请求 id 的响应汇总分支（曾缺失：回包永不发送、pending_forward 条目泄漏）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_recovery_forward_second_hop() {
    let now = 1_720_000_000_000i64;
    let (a, _state_a, _s_a) = start_node(now, None).await;
    let (mut b, _state_b, _s_b) = start_node(now, None).await;
    let (mut c, state_c, _s_c) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let addrs_c = started_addresses(&mut c).await;

    // C 的恢复视图命中；B 视图留空（本地不命中 → 触发转发）
    state_c.lock().unwrap().recovery_view = vec![RecoveryViewItem {
        org_id: "org_0123456789abcdef".to_string(),
        recovery_secret: "ef".repeat(32),
        member_node_infos: vec![OrganizationNodeInfo {
            peer_id: Some("12D3KooWMemberY".to_string()),
            addresses: vec!["/ip4/10.4.5.6/tcp/15002/ws".to_string()],
        }],
    }];

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    connect(&b, c.peer_id(), &dialable(&addrs_c)).await;
    wait_for(&mut c, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let [token, _] = active_recovery_tokens("org_0123456789abcdef", &"ef".repeat(32), now);
    let found = a
        .query_recovery(&token, vec![b.peer_id().to_string()], 8)
        .await
        .expect("forwarded query ok");
    assert_eq!(found.len(), 1, "B 应转发给 C 并汇总回包");
    assert_eq!(found[0].peer_id.as_deref(), Some("12D3KooWMemberY"));
    assert_eq!(
        found[0].addresses,
        vec!["/ip4/10.4.5.6/tcp/15002/ws".to_string()]
    );

    a.stop().await;
    b.stop().await;
    c.stop().await;
}

/// org-share：直连推送确认 + pubsub 推送 + ack 回流。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_share_direct_and_pubsub_ack() {
    let now = 1_720_000_000_000i64;
    let root_b = "bb".repeat(32);
    let (mut a, state_a, _s_a) = start_node(now, Some(&"aa".repeat(32))).await;
    let (mut b, state_b, _s_b) = start_node(now, Some(&root_b)).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let org = json!({
        "orgId": "org_0123456789abcdef",
        "members": [{"rootId": root_b, "role": "member", "joinedAt": now, "addedBy": "aa".repeat(32)}],
    });
    let payload = json!({
        "targetRootId": root_b,
        "syncId": "0123456789abcdef01234567",
        "organization": org,
        "pluginDocs": [],
        "nodeInfo": {"peerId": b.peer_id(), "addresses": []},
    });

    // 直连推送：ok && syncId 匹配 → true，B 侧记录接收
    let delivered = a
        .org_share_direct(
            &PeerNodeInfo {
                peer_id: Some(b.peer_id().to_string()),
                addresses: dialable(&addrs_b),
            },
            payload.clone(),
        )
        .await
        .expect("direct share ok");
    assert!(delivered, "direct org-share delivered");
    assert!(
        state_b
            .lock()
            .unwrap()
            .shares
            .iter()
            .any(|(_, source)| *source == "direct")
    );

    // pubsub 推送：B 接受 → 广播 org-share-ack → A 的宿主收到 ack
    let mut pubsub_payload = payload.clone();
    pubsub_payload["syncId"] = json!("fedcba9876543210fedcba98");
    let body = build_org_body("org-share", pubsub_payload);
    let acks = state_a.clone();
    broadcast_until(&a, "spark-sync", body, move || {
        !acks.lock().unwrap().acks.is_empty()
    })
    .await;
    let ack = state_a.lock().unwrap().acks[0].clone();
    assert_eq!(ack["syncId"], "fedcba9876543210fedcba98");
    assert_eq!(ack["orgId"], "org_0123456789abcdef");
    assert_eq!(ack["receiverRootId"], root_b);

    a.stop().await;
    b.stop().await;
}

/// keepalive tick：覆盖网维护返回统计（无邻居时为零值）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keepalive_tick_stats() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await;
    let _ = started_addresses(&mut a).await;
    let stats: KeepaliveStats = a.maintain_tick().await.expect("tick ok");
    assert_eq!(stats.overlay_dialed, 0);
    assert_eq!(stats.exchanged, 0);
    a.stop().await;
}
