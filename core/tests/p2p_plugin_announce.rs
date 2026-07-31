//! plugin-announce 广播索引集成测试（本机 loopback 真实 libp2p 节点，plugin-dist §8）：
//! 双节点广播收发 + 入索引、relay 资历制门控（三节点 A→B→C：资历不足只收不转）、
//! 无效声明拒收（不入索引、不发事件）。

mod common;

use std::time::Duration;

use spark_core::p2p::plugin_announce::{
    AnnouncePow, PluginAnnounceInput, PluginAnnounceStore, build_signed_announce,
    mine_announce_nonce, plugin_announce_to_json,
};
use spark_core::p2p::{P2pEvent, P2pNode};

use common::p2p::*;

const NOW: i64 = 1_720_000_000_000;
/// 测试难度（网络常量 20 在 debug 构建下偏慢，节点配置覆盖为 8）。
const TEST_BITS: u32 = 8;

fn test_signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
}

fn announce_input(id: &str) -> PluginAnnounceInput {
    PluginAnnounceInput {
        id: id.to_string(),
        name: "待办".to_string(),
        icon: String::new(),
        summary: "测试插件".to_string(),
        category: "business".to_string(),
        version: "0.2.0".to_string(),
        release_url: String::new(),
    }
}

/// 构造一条完整有效声明（真实签名 + 真算 PoW）。
fn make_announce_json(id: &str, timestamp: i64) -> String {
    let (mut announce, payload) =
        build_signed_announce(&announce_input(id), &test_signing_key(), timestamp).unwrap();
    announce.pow = AnnouncePow {
        bits: TEST_BITS,
        nonce: mine_announce_nonce(&payload, TEST_BITS),
    };
    plugin_announce_to_json(&announce)
}

/// 带难度/资历覆盖启动节点。
async fn start_announce_node(
    relay_tenure_ms: i64,
) -> (P2pNode, std::sync::Arc<std::sync::Mutex<HostState>>, SharedStorage) {
    let storage = SharedStorage::new();
    let (host, state) = TestHost::new(None, storage.clone());
    let mut config = test_config(NOW);
    config.plugin_announce_pow_bits = Some(TEST_BITS);
    config.plugin_announce_relay_tenure_ms = Some(relay_tenure_ms);
    // 关 DHT：kad 会自动发现并直连「同连一个中继」的节点，破坏资历制测试
    // 所需的 A—B—C 单路径拓扑（生产保留 kad，自发消息本就不受资历制约束）
    config.dht_mode = spark_core::p2p::DhtMode::Off;
    let node = P2pNode::start(config, storage.clone(), Box::new(host))
        .await
        .expect("node starts");
    (node, state, storage)
}

fn index_has(storage: &SharedStorage, id: &str) -> bool {
    let mut s = storage.clone();
    PluginAnnounceStore::new(&mut s).get(id).ok().flatten().is_some()
}

/// 双节点：声明广播 → 对端校验通过入索引并发 PluginAnnounceReceived 事件。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announce_broadcast_two_nodes() {
    let (mut a, _state_a, _s_a) = start_announce_node(0).await;
    let (mut b, _state_b, s_b) = start_announce_node(0).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;
    connect(&a, &b.peer_id().to_string(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 每次重试换新 nonce（新 message id，避免 gossipsub 去重吞掉重试）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut nonce_offset = 0i64;
    loop {
        let json = make_announce_json("github.com/acme/todo", NOW + nonce_offset);
        a.publish_plugin_announce(&json).await.expect("publish ok");
        for _ in 0..5 {
            if index_has(&s_b, "github.com/acme/todo") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if index_has(&s_b, "github.com/acme/todo") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "announce not delivered");
        nonce_offset += 1;
    }

    // 事件与索引内容
    wait_for(&mut b, Duration::from_secs(5), |e| {
        matches!(e, P2pEvent::PluginAnnounceReceived { id, .. } if id == "github.com/acme/todo")
    })
    .await;
    let mut s = s_b.clone();
    let entry = PluginAnnounceStore::new(&mut s)
        .get("github.com/acme/todo")
        .unwrap()
        .expect("indexed");
    assert_eq!(entry.announce.name, "待办");

    a.stop().await;
    b.stop().await;
}

/// relay 资历制（§8.6）：三节点 A—B—C（A 与 C 不直连）。
/// B 资历阈值 0（合格中继）时 C 收到转发；阈值极大（资历不足）时 C 只收不到，
/// 但 B 本地照常入索引（只收不转）。
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn announce_relay_tenure_gating() {
    // 场景一：B 是合格中继（tenure = 0）→ C 经 B 转发收到
    let (mut a, _sa, _s_a) = start_announce_node(0).await;
    let (mut b, _sb, _s_b) = start_announce_node(0).await;
    let (mut c, _sc, s_c) = start_announce_node(0).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;
    let _ = started_addresses(&mut c).await;
    connect(&a, &b.peer_id().to_string(), &dialable(&addrs_b)).await;
    connect(&c, &b.peer_id().to_string(), &dialable(&addrs_b)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut nonce_offset = 0i64;
    loop {
        let json = make_announce_json("github.com/acme/relay-ok", NOW + nonce_offset);
        a.publish_plugin_announce(&json).await.expect("publish ok");
        for _ in 0..5 {
            if index_has(&s_c, "github.com/acme/relay-ok") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if index_has(&s_c, "github.com/acme/relay-ok") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "qualified relay did not forward"
        );
        nonce_offset += 1;
    }
    a.stop().await;
    b.stop().await;
    c.stop().await;

    // 场景二：B 资历阈值极大 → C 收不到，B 本地仍入索引（只收不转）
    let (mut a2, _sa2, _s_a2) = start_announce_node(0).await;
    let (mut b2, _sb2, s_b2) = start_announce_node(i64::MAX).await;
    let (mut c2, _sc2, s_c2) = start_announce_node(0).await;
    let addrs_b2 = started_addresses(&mut b2).await;
    let _ = started_addresses(&mut a2).await;
    let _ = started_addresses(&mut c2).await;
    connect(&a2, &b2.peer_id().to_string(), &dialable(&addrs_b2)).await;
    connect(&c2, &b2.peer_id().to_string(), &dialable(&addrs_b2)).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut nonce_offset = 0i64;
    loop {
        let json = make_announce_json("github.com/acme/relay-no", NOW + nonce_offset);
        a2.publish_plugin_announce(&json).await.expect("publish ok");
        for _ in 0..5 {
            if index_has(&s_b2, "github.com/acme/relay-no") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if index_has(&s_b2, "github.com/acme/relay-no") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "relay itself did not index"
        );
        nonce_offset += 1;
    }
    // 给足转发窗口（数次心跳 + 额外重发），C 必须仍为空
    let wait_deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < wait_deadline {
        let json = make_announce_json("github.com/acme/relay-no", NOW + 10_000 + nonce_offset);
        a2.publish_plugin_announce(&json).await.expect("publish ok");
        nonce_offset += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        !index_has(&s_c2, "github.com/acme/relay-no"),
        "unqualified relay must not forward"
    );
    a2.stop().await;
    b2.stop().await;
    c2.stop().await;
}

/// 无效声明（PoW 不足）拒收：不入索引、不发事件。
/// 负向断言前先发一条合法声明证明通路正常（排除「通路本就断」的假性通过）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announce_invalid_rejected() {
    let (mut a, _sa, _s_a) = start_announce_node(0).await;
    let (mut b, _sb, s_b) = start_announce_node(0).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;
    connect(&a, &b.peer_id().to_string(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 正向：合法声明必须能送达并入索引（每次重试换新 nonce 避免 gossipsub 去重）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut nonce_offset = 0i64;
    loop {
        let json = make_announce_json("github.com/acme/good", NOW + nonce_offset);
        a.publish_plugin_announce(&json).await.expect("publish ok");
        for _ in 0..5 {
            if index_has(&s_b, "github.com/acme/good") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if index_has(&s_b, "github.com/acme/good") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "valid announce not delivered"
        );
        nonce_offset += 1;
    }

    // bits 低于节点下限（结构拒）
    let (mut bad, payload) =
        build_signed_announce(&announce_input("github.com/acme/bad"), &test_signing_key(), NOW)
            .unwrap();
    bad.pow = AnnouncePow {
        bits: 1,
        nonce: mine_announce_nonce(&payload, 1),
    };
    a.publish_plugin_announce(&plugin_announce_to_json(&bad))
        .await
        .expect("publish ok");

    // 等足传播窗口：B 不得入索引（通路已被上面的正向声明证明是通的）
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!index_has(&s_b, "github.com/acme/bad"));
    assert!(index_has(&s_b, "github.com/acme/good"));
    a.stop().await;
    b.stop().await;
}
