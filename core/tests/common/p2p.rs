//! p2p 集成测试共用夹具：共享存储、可编程测试宿主与节点启动/等待/广播助手。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use spark_core::org::recovery::RecoveryViewItem;
use spark_core::p2p::overlay_store::{OverlayPeerRecord, OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::p2p::{
    P2pConfig, P2pEvent, P2pHost, P2pNode, announce_to_json, sign_node_announce,
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
// 测试宿主：记录回调、可编程恢复视图
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct HostState {
    pub applied: Vec<(String, String, String, Value)>,
    pub versions: Vec<(String, String)>,
    pub recovery_view: Vec<RecoveryViewItem>,
    /// 组织私有 DHT 命中的成员提示（on_org_member_hints 回调记录）。
    pub org_member_hints: Vec<spark_core::org::OrgMemberHint>,
    /// dm 直连接收记录（handle_dm 回调：(payload, remote_peer_id)）。
    pub dms: Vec<(Value, String)>,
    /// 运行期可切换的「视为已撤销」peer：建连后再标记可触及 dm/challenge
    /// 入站黑名单分支（ConnectionEstablished 守卫断开前的纵深防御）。
    pub revoked_peer: Option<String>,
    /// org-mail 宿主回调用的本机 libp2p peerId（节点启动后由测试回填——
    /// 挑战签名载荷绑网关 peerId，宿主验签要与节点真实 peerId 一致）。
    pub my_peer_id: Option<String>,
}

pub struct TestHost {
    root_id: Option<String>,
    state: Arc<Mutex<HostState>>,
    /// 邻居池/活跃度回填（on_org_member_hints 的宿主口径与 KernelHost 一致）。
    storage: SharedStorage,
    /// 撤销 peer 集合：命中即被连接层黑名单拦截（测试连接层四拦截点）。
    revoked_peers: std::collections::HashSet<String>,
}

impl TestHost {
    pub fn new(root_id: Option<&str>, storage: SharedStorage) -> (Self, Arc<Mutex<HostState>>) {
        let state = Arc::new(Mutex::new(HostState::default()));
        (
            Self {
                root_id: root_id.map(ToString::to_string),
                state: state.clone(),
                storage,
                revoked_peers: std::collections::HashSet::new(),
            },
            state,
        )
    }

    /// 构造一个把指定 peer 视为已撤销的测试宿主（连接层黑名单拦截用）。
    pub fn new_with_revoked(
        root_id: Option<&str>,
        storage: SharedStorage,
        revoked: &[&str],
    ) -> (Self, Arc<Mutex<HostState>>) {
        let state = Arc::new(Mutex::new(HostState::default()));
        (
            Self {
                root_id: root_id.map(ToString::to_string),
                state: state.clone(),
                storage,
                revoked_peers: revoked.iter().map(|s| s.to_string()).collect(),
            },
            state,
        )
    }
}

impl P2pHost for TestHost {
    fn current_root_id(&mut self) -> Option<String> {
        self.root_id.clone()
    }

    fn is_revoked_peer(&mut self, peer_id: &str) -> bool {
        self.revoked_peers.contains(peer_id)
            || self.state.lock().unwrap().revoked_peer.as_deref() == Some(peer_id)
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

    /// dm 直连接收：记录信封与对端 peerId，回 `{"ok": true}` 应答。
    fn handle_dm(&mut self, payload: Value, remote_peer_id: &str) -> Result<Value, String> {
        self.state
            .lock()
            .unwrap()
            .dms
            .push((payload, remote_peer_id.to_string()));
        Ok(serde_json::json!({"ok": true}))
    }

    /// org-mail 直连接收（阶段四E）：委托 kernel 层真实入站分发（deliver/
    /// fetch 两 op），本机 peerId 取 HostState 回填值（挑战验签绑定）。
    fn handle_org_mail(&mut self, payload: &Value, remote_peer_id: &str) -> Result<Value, String> {
        let my_peer_id = self
            .state
            .lock()
            .unwrap()
            .my_peer_id
            .clone()
            .unwrap_or_default();
        spark_core::kernel::org_mail_ops::handle_org_mail_inbound(
            &mut self.storage,
            self.root_id.as_deref(),
            &my_peer_id,
            payload,
            remote_peer_id,
        )
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
                None,
                &std::collections::HashSet::new(),
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
        leaf_mode: false,
        plugin_announce_pow_bits: None,
        plugin_announce_relay_tenure_ms: None,
        dht_republish_ticks: None,
        enable_relay_server: false,
        enable_dcutr: true,
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

/// 启动一个把指定 peer 视为已撤销的节点（测试连接层黑名单拦截）。
pub async fn start_node_with_revoked(
    now_ms: i64,
    root_id: Option<&str>,
    revoked: &[&str],
) -> (P2pNode, Arc<Mutex<HostState>>, SharedStorage) {
    let storage = SharedStorage::new();
    let (host, state) = TestHost::new_with_revoked(root_id, storage.clone(), revoked);
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

/// 带超时的事件等待：在 timeout 内命中 pred 返回 true，否则 false（不 panic）。
pub async fn wait_for_timeout(
    node: &mut P2pNode,
    timeout: Duration,
    mut pred: impl FnMut(&P2pEvent) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, node.next_event()).await {
            Ok(Some(event)) => {
                if pred(&event) {
                    return true;
                }
            }
            _ => return false,
        }
    }
}

/// 从共享存储读取 overlay 邻居池（测试检查节点落库）。
pub fn overlay_peers(storage: &SharedStorage) -> Vec<OverlayPeerRecord> {
    let mut guard = storage.0.lock().unwrap();
    let mut store = OverlayPeerStore::new(&mut *guard);
    store.list_all().expect("list overlay")
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
