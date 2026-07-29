//! p2p 集成测试共用夹具：共享存储、可编程测试宿主与节点启动/等待/广播助手。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use spark_core::org::recovery::RecoveryViewItem;
use spark_core::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::p2p::{
    OrgShareAck, P2pConfig, P2pEvent, P2pHost, P2pNode, announce_to_json, sign_node_announce,
};
use spark_core::storage::{BatchOperation, MemoryStorage, ScanOptions, StorageBackend};

// ---------------------------------------------------------------------------
// 共享存储（测试从外部检查节点写入）
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct SharedStorage(pub Arc<Mutex<MemoryStorage>>);

impl SharedStorage {
    pub fn new() -> Self {
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
pub struct HostState {
    pub applied: Vec<(String, String, String, Value)>,
    pub shares: Vec<(Value, &'static str)>,
    pub acks: Vec<Value>,
    pub versions: Vec<(String, String)>,
    pub recovery_view: Vec<RecoveryViewItem>,
    /// 组织私有 DHT 命中的成员提示（on_org_member_hints 回调记录）。
    pub org_member_hints: Vec<spark_core::org::OrgMemberHint>,
    /// dm 直连接收记录（handle_dm 回调：(payload, remote_peer_id)）。
    pub dms: Vec<(Value, String)>,
}

pub struct TestHost {
    root_id: Option<String>,
    state: Arc<Mutex<HostState>>,
    /// 接受所有指向本机 rootId 的 org-share。
    accept_shares: bool,
    /// 邻居池/活跃度回填（on_org_member_hints 的宿主口径与 KernelHost 一致）。
    storage: SharedStorage,
}

impl TestHost {
    pub fn new(root_id: Option<&str>, storage: SharedStorage) -> (Self, Arc<Mutex<HostState>>) {
        let state = Arc::new(Mutex::new(HostState::default()));
        (
            Self {
                root_id: root_id.map(ToString::to_string),
                state: state.clone(),
                accept_shares: true,
                storage,
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

    /// dm 直连接收：记录信封与对端 peerId，回 `{"ok": true}` 应答。
    fn handle_dm(&mut self, payload: Value, remote_peer_id: &str) -> Result<Value, String> {
        self.state
            .lock()
            .unwrap()
            .dms
            .push((payload, remote_peer_id.to_string()));
        Ok(serde_json::json!({"ok": true}))
    }

    /// 组织私有 DHT 成员提示回填（§15）：记录回调 + 按未验证口径入邻居池
    /// （与 KernelHost 同口径，信任边界不变）。
    fn on_org_member_hints(&mut self, hints: &[spark_core::org::OrgMemberHint]) {
        let now = 1_720_000_000_000i64;
        self.state
            .lock()
            .unwrap()
            .org_member_hints
            .extend(hints.iter().cloned());
        let mut guard = self.storage.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        for hint in hints {
            let _ = store.remember(
                &hint.peer_id,
                &hint.addresses,
                OverlayPeerSource::Exchange,
                false,
                now,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

pub fn test_config(now_ms: i64) -> P2pConfig {
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
        dht_mode: spark_core::p2p::DhtMode::Server,
        now_fn: Arc::new(move || now_ms),
    }
}

pub async fn start_node(
    now_ms: i64,
    root_id: Option<&str>,
) -> (P2pNode, Arc<Mutex<HostState>>, SharedStorage) {
    let storage = SharedStorage::new();
    let (host, state) = TestHost::new(root_id, storage.clone());
    let node = P2pNode::start(test_config(now_ms), storage.clone(), Box::new(host))
        .await
        .expect("node starts");
    (node, state, storage)
}

pub async fn wait_for(
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

pub async fn started_addresses(node: &mut P2pNode) -> Vec<String> {
    match wait_for(node, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::Started { .. })
    })
    .await
    {
        P2pEvent::Started {
            listen_addresses, ..
        } => listen_addresses,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// 取节点的可拨 loopback 地址（通配监听替换为 127.0.0.1）。
pub fn dialable(addresses: &[String]) -> Vec<String> {
    addresses
        .iter()
        .filter(|a| a.contains("/ip4/"))
        .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
        .collect()
}

pub async fn connect(a: &P2pNode, b_peer_id: &str, b_addrs: &[String]) {
    a.connect_peer(&PeerNodeInfo {
        peer_id: Some(b_peer_id.to_string()),
        addresses: b_addrs.to_vec(),
    })
    .await
    .expect("connect succeeds");
}

/// gossipsub 订阅传播与 mesh 需要一点时间；发布重试直到对端收到或超时。
pub async fn broadcast_until(
    node: &P2pNode,
    topic: &str,
    body: serde_json::Map<String, Value>,
    mut received: impl FnMut() -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        node.broadcast(topic, body.clone())
            .await
            .expect("broadcast ok");
        for _ in 0..5 {
            if received() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "message not delivered in time"
        );
    }
}

/// 用节点真实 libp2p 身份签一条节点存在记录（与节点周期发布的格式一致）。
pub fn sign_presence_record(
    storage: &SharedStorage,
    peer_id: &str,
    addresses: &[String],
    now: i64,
) -> Vec<u8> {
    let mut guard = storage.0.lock().unwrap();
    let keypair = spark_core::p2p::identity_store::get_or_create_libp2p_keypair(&mut *guard)
        .expect("libp2p keypair");
    let announce = sign_node_announce(&keypair, peer_id, addresses, now).expect("sign announce");
    announce_to_json(&announce).into_bytes()
}
