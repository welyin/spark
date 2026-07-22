//! p2p 集成测试：本机 loopback 上的两个真实 libp2p 节点（Rust↔Rust）。
//!
//! 覆盖：gossipsub 消息收发 + 信封验签、node-announce 交换、peer-exchange
//! 请求响应、org-recovery 命中、org-share 直连推送与 pubsub 推送 + ack。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use spark_core::org::recovery::{RecoveryViewItem, active_recovery_tokens};
use spark_core::org::types::OrganizationNodeInfo;
use spark_core::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::p2p::{
    KeepaliveStats, OrgShareAck, P2pConfig, P2pEvent, P2pHost, P2pNode,
    build_org_body, build_update_body,
};
use spark_core::storage::{BatchOperation, MemoryStorage, ScanOptions, StorageBackend};

// ---------------------------------------------------------------------------
// 共享存储（测试从外部检查节点写入）
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct SharedStorage(Arc<Mutex<MemoryStorage>>);

impl SharedStorage {
    fn new() -> Self {
        Self::default()
    }
}

impl StorageBackend for SharedStorage {
    fn get(&self, key: &str) -> spark_core::storage::Result<Option<String>> {
        self.0.lock().unwrap().get(key)
    }
    fn put(&mut self, key: &str, value: &str) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().put(key, value)
    }
    fn delete(&mut self, key: &str) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().delete(key)
    }
    fn batch(&mut self, operations: Vec<BatchOperation>) -> spark_core::storage::Result<()> {
        self.0.lock().unwrap().batch(operations)
    }
    fn scan(&self, options: &ScanOptions) -> spark_core::storage::Result<Vec<(String, String)>> {
        self.0.lock().unwrap().scan(options)
    }
}

// ---------------------------------------------------------------------------
// 测试宿主：记录回调、可编程 org-share/pull 响应与恢复视图
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HostState {
    applied: Vec<(String, String, String, Value)>,
    shares: Vec<(Value, &'static str)>,
    acks: Vec<Value>,
    versions: Vec<(String, String)>,
    recovery_view: Vec<RecoveryViewItem>,
}

struct TestHost {
    root_id: Option<String>,
    state: Arc<Mutex<HostState>>,
    /// 接受所有指向本机 rootId 的 org-share。
    accept_shares: bool,
}

impl TestHost {
    fn new(root_id: Option<&str>) -> (Self, Arc<Mutex<HostState>>) {
        let state = Arc::new(Mutex::new(HostState::default()));
        (
            Self {
                root_id: root_id.map(ToString::to_string),
                state: state.clone(),
                accept_shares: true,
            },
            state,
        )
    }
}

impl P2pHost for TestHost {
    fn current_root_id(&mut self) -> Option<String> {
        self.root_id.clone()
    }

    fn apply_remote_update(
        &mut self,
        domain: &str,
        collection: &str,
        id: &str,
        payload: Value,
        _meta: Value,
        _schema: Option<Value>,
    ) -> Result<(), String> {
        self.state.lock().unwrap().applied.push((
            domain.to_string(),
            collection.to_string(),
            id.to_string(),
            payload,
        ));
        Ok(())
    }

    fn apply_incoming_org_share(
        &mut self,
        payload: Value,
        source: &'static str,
    ) -> Result<Option<OrgShareAck>, String> {
        self.state
            .lock()
            .unwrap()
            .shares
            .push((payload.clone(), source));
        if !self.accept_shares {
            return Ok(None);
        }
        let target_root_id = payload
            .get("targetRootId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if Some(target_root_id.as_str()) != self.root_id.as_deref() {
            return Ok(None);
        }
        let org_id = payload
            .get("organization")
            .and_then(|o| o.get("orgId"))
            .and_then(Value::as_str)
            .unwrap_or("org_unknown")
            .to_string();
        Ok(Some(OrgShareAck {
            sync_id: payload
                .get("syncId")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            org_id,
            target_root_id,
            receiver_root_id: self.root_id.clone().unwrap_or_default(),
        }))
    }

    fn recovery_view(&mut self) -> Vec<RecoveryViewItem> {
        self.state.lock().unwrap().recovery_view.clone()
    }

    fn on_peer_version(&mut self, version: &str, peer_id: &str) {
        self.state
            .lock()
            .unwrap()
            .versions
            .push((peer_id.to_string(), version.to_string()));
    }

    fn on_org_share_ack(&mut self, payload: Value) {
        self.state.lock().unwrap().acks.push(payload);
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn test_config(now_ms: i64) -> P2pConfig {
    P2pConfig {
        app_version: "9.9.9-test".to_string(),
        preferred_port: Some(0),
        port_scan: false,
        enable_tcp: true,
        enable_ws: false,
        enable_ipv6: false,
        enable_mdns: false,
        enable_upnp: false,
        keepalive_interval: None,
        now_fn: Arc::new(move || now_ms),
    }
}

async fn start_node(now_ms: i64, root_id: Option<&str>) -> (P2pNode, Arc<Mutex<HostState>>, SharedStorage) {
    let (host, state) = TestHost::new(root_id);
    let storage = SharedStorage::new();
    let node = P2pNode::start(test_config(now_ms), storage.clone(), Box::new(host))
        .await
        .expect("node starts");
    (node, state, storage)
}

async fn wait_for(
    node: &mut P2pNode,
    timeout: Duration,
    mut pred: impl FnMut(&P2pEvent) -> bool,
) -> P2pEvent {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(remaining > Duration::ZERO, "timed out waiting for event");
        let event = tokio::time::timeout(remaining, node.next_event())
            .await
            .expect("event within timeout")
            .expect("event stream open");
        if pred(&event) {
            return event;
        }
    }
}

async fn started_addresses(node: &mut P2pNode) -> Vec<String> {
    match wait_for(node, Duration::from_secs(10), |e| matches!(e, P2pEvent::Started { .. })).await {
        P2pEvent::Started { listen_addresses, .. } => listen_addresses,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// 取节点的可拨 loopback 地址（通配监听替换为 127.0.0.1）。
fn dialable(addresses: &[String]) -> Vec<String> {
    addresses
        .iter()
        .filter(|a| a.contains("/ip4/"))
        .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
        .collect()
}

async fn connect(a: &P2pNode, b_peer_id: &str, b_addrs: &[String]) {
    a.connect_peer(&PeerNodeInfo {
        peer_id: Some(b_peer_id.to_string()),
        addresses: b_addrs.to_vec(),
    })
    .await
    .expect("connect succeeds");
}

/// gossipsub 订阅传播与 mesh 需要一点时间；发布重试直到对端收到或超时。
async fn broadcast_until(
    node: &P2pNode,
    topic: &str,
    body: serde_json::Map<String, Value>,
    mut received: impl FnMut() -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        node.broadcast(topic, body.clone()).await.expect("broadcast ok");
        for _ in 0..5 {
            if received() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(tokio::time::Instant::now() < deadline, "message not delivered in time");
    }
}

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
    wait_for(&mut b, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;
    wait_for(&mut a, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;
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
    while !state_b.lock().unwrap().versions.iter().any(|(_, v)| v == "9.9.9-test") {
        assert!(tokio::time::Instant::now() < deadline, "version not observed");
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
    wait_for(&mut b, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;

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
        assert!(tokio::time::Instant::now() < deadline, "announce not accepted in time");
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
    wait_for(&mut b, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;

    let merged = a
        .exchange_with_peer(b.peer_id())
        .await
        .expect("exchange ok");
    assert_eq!(merged, 1, "C should be exchanged to A");

    // A 的邻居池应有 C（未验证来源）
    {
        let mut guard = storage_a.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        let record = store.get("12D3KooWFakePeerC1234567890").unwrap().expect("C in A pool");
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
    wait_for(&mut b, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;

    // 命中
    let [token, _] = active_recovery_tokens("org_0123456789abcdef", &"ef".repeat(32), now);
    let found = a
        .query_recovery(&token, vec![b.peer_id().to_string()], 8)
        .await
        .expect("query ok");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].peer_id.as_deref(), Some("12D3KooWMemberX"));
    assert_eq!(found[0].addresses, vec!["/ip4/10.1.2.3/tcp/15002/ws".to_string()]);

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
    wait_for(&mut b, Duration::from_secs(10), |e| matches!(e, P2pEvent::PeerConnected { .. })).await;

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
