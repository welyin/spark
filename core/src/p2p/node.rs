//! P2pNode：节点生命周期、事件循环、命令接口与事件流。
//!
//! - `P2pNode::start(config, storage, host)` 装配 libp2p（TCP+WS 双栈、双协议栈同端口）、
//!   持久化 Ed25519 身份、端口扫描与写回、订阅两个主题、注册四个直连协议；
//! - 事件循环在独立 tokio 任务内运行，宿主经 [`P2pEvent`] 流接收通知、经命令方法驱动；
//! - keepalive 60s tick：覆盖网维护（补拨/peer-exchange/node-announce）由循环内完成，
//!   组织层保活（候选拨号/反熵拉取/补副本/恢复触发）经 `P2pEvent::KeepaliveTick`
//!   交由宿主执行（宿主以 [`P2pNode`] 命令完成拨号与拉取）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use libp2p::swarm::SwarmEvent;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId, Swarm, gossipsub, mdns, request_response};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};

use crate::storage::StorageBackend;

use super::behaviour::{BehaviourOptions, SparkBehaviour, SparkBehaviourEvent, build_behaviour};
use super::constants::{
    NODE_ANNOUNCE_INTERVAL_MS, ORG_KEEPALIVE_INTERVAL_MS, OVERLAY_TOPIC,
    PEER_EXCHANGE_MIN_INTERVAL_MS, P2P_LISTEN_WS_PORT, RECOVERY_QUERY_MIN_INTERVAL_MS,
    SYNC_TOPIC,
};
use super::direct::{
    self, MinIntervalRateLimiter,
};
use super::envelope::{Envelope, EnvelopeSigner};
use super::host::P2pHost;
use super::identity_store::get_or_create_libp2p_keypair;
use super::keepalive;
use super::listen_port;
use super::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use super::peer_activity::{NodeObservation, PeerActivityStore};
use super::peer_targets::{PeerNodeInfo, extract_peer_id};
use super::announce::{
    NodeAnnounceValidator, announce_to_json, prepare_publish_addresses, sign_node_announce,
};
use super::{P2pError, Result};

/// 时间源（now_ms 注入）。
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// 系统时间 now_ms（生产默认）。
pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 节点配置。
#[derive(Clone)]
pub struct P2pConfig {
    /// 应用版本（`/spark/version/1.0.0` 响应）。
    pub app_version: String,
    /// 首选监听端口；None 时读持久化值，再退化默认 15002。
    pub preferred_port: Option<u16>,
    /// 显式指定端口为 0 时跳过扫描（OS 分配临时端口，测试用）。
    pub port_scan: bool,
    /// 监听裸 TCP（Rust 侧双协议栈）。
    pub enable_tcp: bool,
    /// 监听 WebSocket。
    pub enable_ws: bool,
    /// 允许 IPv6 双栈（OS 不支持时自动回退）。
    pub enable_ipv6: bool,
    /// mDNS 本地发现。
    pub enable_mdns: bool,
    /// UPnP 端口映射。
    pub enable_upnp: bool,
    /// keepalive 周期；`None` 禁用（测试）。
    pub keepalive_interval: Option<Duration>,
    /// 时间源注入。
    pub now_fn: NowFn,
}

impl Default for P2pConfig {
    fn default() -> Self {
        Self {
            app_version: "0.0.0".to_string(),
            preferred_port: None,
            port_scan: true,
            enable_tcp: true,
            enable_ws: true,
            enable_ipv6: true,
            enable_mdns: true,
            enable_upnp: true,
            keepalive_interval: Some(Duration::from_millis(ORG_KEEPALIVE_INTERVAL_MS as u64)),
            now_fn: Arc::new(system_now_ms),
        }
    }
}

/// 对外诊断信息（TS `LocalP2PNodeInfo`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalP2PNodeInfo {
    pub started: bool,
    pub peer_id: Option<String>,
    pub addresses: Vec<String>,
    pub connected_peers: Vec<String>,
    pub spark_sync_subscribers: Vec<String>,
}

/// keepalive tick 统计（宿主组织层保活的触发信号）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepaliveStats {
    pub overlay_dialed: usize,
    pub exchanged: usize,
    pub announced: bool,
}

/// 节点事件流。
///
/// serde 线形：相邻标签 `{kind, data}`（`kind` 为变体名，`data` 仅结构化变体携带；
/// 单元变体如 `Stopped` 无 `data` 键），壳层可直接序列化转发前端。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", content = "data", rename_all_fields = "camelCase")]
pub enum P2pEvent {
    /// 节点启动完成（首个监听地址确认）。
    Started { peer_id: String, listen_addresses: Vec<String> },
    /// 实际监听端口写回持久化。
    ListenPortPersisted { port: u16 },
    PeerConnected { peer_id: String },
    PeerDisconnected { peer_id: String },
    /// 对端版本观察。
    PeerVersion { peer_id: String, app_version: String },
    /// node-announce 已发布。
    AnnouncePublished { addresses: usize },
    /// 入站 announce 验签通过并入池。
    AnnounceAccepted { peer_id: String },
    /// peer-exchange 完成（合并条目数）。
    PeerExchangeCompleted { responder: String, merged: usize },
    /// org-share 推送被接受（pubsub/直连）。
    OrgShareAccepted { org_id: String, sync_id: Option<String>, source: &'static str },
    /// 数据类消息已交宿主落库。
    SyncMessageApplied { msg_type: String, domain: String },
    /// 消息被丢弃（验签失败/强制签名缺失/形状非法）。
    MessageDropped { reason: String },
    /// keepalive tick 完成（宿主应执行组织层保活）。
    KeepaliveTick(KeepaliveStats),
    /// 非致命告警。
    Warning(String),
    /// 节点已停止。
    Stopped,
}

enum Command {
    Broadcast {
        topic: String,
        body: Map<String, Value>,
        tx: oneshot::Sender<Result<()>>,
    },
    AnnounceNow {
        tx: oneshot::Sender<Result<bool>>,
    },
    ConnectPeer {
        node_info: PeerNodeInfo,
        tx: oneshot::Sender<Result<()>>,
    },
    ExchangeWithPeer {
        peer_id: String,
        tx: oneshot::Sender<Result<usize>>,
    },
    QueryRecovery {
        token: String,
        neighbors: Vec<String>,
        want: usize,
        tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
    },
    OrgShareDirect {
        node_info: PeerNodeInfo,
        payload: Value,
        tx: oneshot::Sender<Result<bool>>,
    },
    OrgPullRequest {
        node_info: PeerNodeInfo,
        request_json: String,
        tx: oneshot::Sender<Result<Option<Value>>>,
    },
    LocalNodeInfo {
        tx: oneshot::Sender<LocalP2PNodeInfo>,
    },
    Tick {
        tx: oneshot::Sender<KeepaliveStats>,
    },
    Shutdown,
}

/// P2P 节点句柄。
pub struct P2pNode {
    peer_id: String,
    cmd_tx: mpsc::UnboundedSender<Command>,
    event_rx: mpsc::UnboundedReceiver<P2pEvent>,
    /// 事件循环任务句柄（Mutex 使 `stop` 仅需 `&self`，节点可放入 `Arc`
    /// 与宿主侧编排任务共享）。
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl P2pNode {
    /// 启动节点：加载/生成 libp2p 身份，装配双栈监听，订阅主题，注册直连协议。
    pub async fn start<S: StorageBackend + Send + 'static>(
        config: P2pConfig,
        mut storage: S,
        host: Box<dyn P2pHost>,
    ) -> Result<Self> {
        let keypair = get_or_create_libp2p_keypair(&mut storage)?;
        let peer_id = PeerId::from_public_key(&keypair.public());
        let peer_id_str = peer_id.to_base58();

        let persisted_port = storage
            .get(P2P_LISTEN_WS_PORT)?
            .and_then(|v| v.trim().parse::<u16>().ok());
        let preferred = config
            .preferred_port
            .or(persisted_port)
            .unwrap_or(listen_port::default_listen_port());
        let ipv6 = config.enable_ipv6 && listen_port::supports_ipv6();
        let port = if config.port_scan {
            listen_port::pick_listen_port(preferred, None, ipv6)
        } else {
            preferred
        };

        let behaviour_options = BehaviourOptions {
            enable_mdns: config.enable_mdns,
            enable_upnp: config.enable_upnp,
        };
        let mut swarm = build_swarm(&keypair, &behaviour_options).await?;

        let addrs = build_listen_addrs(port, ipv6, config.enable_tcp, config.enable_ws);
        let mut listen_failed = false;
        for addr in &addrs {
            let ma: Multiaddr = addr
                .parse()
                .map_err(|e| P2pError::Swarm(format!("invalid listen addr {addr}: {e}")))?;
            if swarm.listen_on(ma).is_err() {
                listen_failed = true;
                break;
            }
        }
        if listen_failed && ipv6 {
            // 双栈绑定失败回退 IPv4 单栈（探测与绑定间的竞态兜底）
            swarm = build_swarm(&keypair, &behaviour_options).await?;
            for addr in build_listen_addrs(port, false, config.enable_tcp, config.enable_ws) {
                let ma: Multiaddr = addr
                    .parse()
                    .map_err(|e| P2pError::Swarm(format!("invalid listen addr {addr}: {e}")))?;
                swarm
                    .listen_on(ma)
                    .map_err(|e| P2pError::Swarm(format!("listen failed on {addr}: {e}")))?;
            }
        } else if listen_failed {
            return Err(P2pError::Swarm("listen failed".to_string()));
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let event_loop = EventLoop {
            swarm,
            storage,
            host,
            keypair,
            signer: EnvelopeSigner::generate(),
            now_fn: config.now_fn.clone(),
            app_version: config.app_version.clone(),
            cmd_rx,
            event_tx,
            announce_validator: NodeAnnounceValidator::new(),
            exchange_limiter: MinIntervalRateLimiter::new(PEER_EXCHANGE_MIN_INTERVAL_MS),
            recovery_limiter: MinIntervalRateLimiter::new(RECOVERY_QUERY_MIN_INTERVAL_MS),
            last_announced_at: 0,
            overlay_exchange_cursor: 0,
            started_emitted: false,
            port_persisted: false,
            pending_connects: Vec::new(),
            pending_overlay_dials: HashMap::new(),
            version_probe_in_flight: HashSet::new(),
            pending_version: HashMap::new(),
            pending_exchange: HashMap::new(),
            pending_recovery: HashMap::new(),
            pending_recovery_extra: HashMap::new(),
            pending_forward: HashMap::new(),
            pending_forward_extra: HashMap::new(),
            pending_org_attempts: Vec::new(),
        };
        let keepalive_interval = config.keepalive_interval;
        let task = tokio::spawn(async move {
            event_loop.run(keepalive_interval).await;
        });

        Ok(Self {
            peer_id: peer_id_str,
            cmd_tx,
            event_rx,
            task: std::sync::Mutex::new(Some(task)),
        })
    }

    /// 本机 PeerId 字符串。
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// 拉取下一个事件。
    pub async fn next_event(&mut self) -> Option<P2pEvent> {
        self.event_rx.recv().await
    }

    /// 取走事件接收端（一次性）：供宿主把事件泵到自己的广播通道。
    /// 取走后 `next_event` 恒返回 `None`。
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<P2pEvent> {
        let (_tx, rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.event_rx, rx)
    }

    /// 停止节点（`&self` 语义：发送 Shutdown 并等待事件循环退出；重复调用安全）。
    pub async fn stop(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    fn send_cmd(&self, cmd: Command) -> Result<()> {
        self.cmd_tx.send(cmd).map_err(|_| P2pError::NotStarted)
    }

    /// 广播业务消息：自动填充 version/evidenceHeadHash/timestamp 并签名。
    pub async fn broadcast(&self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::Broadcast {
            topic: topic.to_string(),
            body,
            tx,
        })?;
        rx.await.map_err(|_| P2pError::NotStarted)?
    }

    /// 立即发布一次 node-announce（地址变化补发之外的主动触发）。
    pub async fn announce_now(&self) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::AnnounceNow { tx })?;
        rx.await.map_err(|_| P2pError::NotStarted)?
    }

    /// 按候选地址列表拨号连接目标成员（10s 超时）。
    pub async fn connect_peer(&self, node_info: &PeerNodeInfo) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::ConnectPeer {
            node_info: node_info.clone(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Dial("connect timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 向一个已连接邻居发起 peer-exchange，返回合并条目数。
    pub async fn exchange_with_peer(&self, peer_id: &str) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::ExchangeWithPeer {
            peer_id: peer_id.to_string(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("exchange timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 向一组已连接邻居发出 org-recovery 查询（ttl=2、去重合并、截断 16）。
    pub async fn query_recovery(
        &self,
        token: &str,
        neighbors: Vec<String>,
        want: usize,
    ) -> Result<Vec<PeerNodeInfo>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::QueryRecovery {
            token: token.to_string(),
            neighbors,
            want,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("recovery query timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 直连 org-share 推送（逐地址尝试，ok && syncId 匹配即送达）。
    pub async fn org_share_direct(&self, node_info: &PeerNodeInfo, payload: Value) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::OrgShareDirect {
            node_info: node_info.clone(),
            payload,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("org-share timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 直连 org-pull 请求（org-pull-list / org-pull-org 帧文本），返回首个可解析响应。
    pub async fn org_pull_request(
        &self,
        node_info: &PeerNodeInfo,
        request_json: &str,
    ) -> Result<Option<Value>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::OrgPullRequest {
            node_info: node_info.clone(),
            request_json: request_json.to_string(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("org-pull timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 节点状态快照（UI 诊断）。
    pub async fn local_node_info(&self) -> Result<LocalP2PNodeInfo> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::LocalNodeInfo { tx })?;
        rx.await.map_err(|_| P2pError::NotStarted)
    }

    /// 手动触发一次 keepalive tick（测试用；周期 tick 由循环内 interval 驱动）。
    pub async fn maintain_tick(&self) -> Result<KeepaliveStats> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::Tick { tx })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("tick timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)
    }
}

/// 构造监听地址（按开关过滤）。
fn build_listen_addrs(port: u16, ipv6: bool, tcp: bool, ws: bool) -> Vec<String> {
    let mut addrs = Vec::new();
    if tcp {
        addrs.push(format!("/ip4/0.0.0.0/tcp/{port}"));
        if ipv6 {
            addrs.push(format!("/ip6/::/tcp/{port}"));
        }
    }
    if ws {
        addrs.push(format!("/ip4/0.0.0.0/tcp/{port}/ws"));
        if ipv6 {
            addrs.push(format!("/ip6/::/tcp/{port}/ws"));
        }
    }
    addrs
}

async fn build_swarm(
    keypair: &libp2p::identity::Keypair,
    options: &BehaviourOptions,
) -> Result<Swarm<SparkBehaviour>> {
    let options = options.clone();
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| P2pError::Swarm(format!("tcp security: {e}")))?
        .with_websocket(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .await
        .map_err(|e| P2pError::Swarm(format!("websocket: {e}")))?
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .map_err(|e| P2pError::Swarm(format!("relay client: {e}")))?
        .with_behaviour(|key, relay_client| {
            build_behaviour(key, relay_client, &options)
                .expect("behaviour construction is infallible for valid keypair")
        })
        .expect("behaviour constructor is infallible")
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    Ok(swarm)
}

struct PendingConnect {
    node_info: PeerNodeInfo,
    targets: VecDeque<String>,
    current: Option<String>,
    tx: oneshot::Sender<Result<()>>,
    last_error: Option<String>,
}

struct RecoverySession {
    remaining: usize,
    collected: Vec<PeerNodeInfo>,
    tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
}

struct ForwardCtx {
    channel: request_response::ResponseChannel<String>,
    remaining: usize,
    collected: Vec<PeerNodeInfo>,
    want: usize,
}

enum OrgAttemptKind {
    /// org-share 直连推送：ok && syncId 匹配即 true。
    Share { expected_sync_id: String },
    /// org-pull：返回首个可解析响应 JSON。
    Pull,
}

/// org 直连尝试的最终结果通道（按类别直接回传给调用方）。
enum OrgTx {
    Share(oneshot::Sender<Result<bool>>),
    Pull(oneshot::Sender<Result<Option<Value>>>),
}

struct OrgAttempt {
    kind: OrgAttemptKind,
    targets: VecDeque<String>,
    current_target: Option<String>,
    current_peer: Option<PeerId>,
    request_json: String,
    in_flight: Option<request_response::OutboundRequestId>,
    tx: OrgTx,
}

impl OrgAttempt {
    /// 地址/重试耗尽：按类别回传终态。
    fn finish_exhausted(self) {
        match self.tx {
            OrgTx::Share(tx) => {
                let _ = tx.send(Ok(false));
            }
            OrgTx::Pull(tx) => {
                let _ = tx.send(Ok(None));
            }
        }
    }
}

struct EventLoop<S: StorageBackend> {
    swarm: Swarm<SparkBehaviour>,
    storage: S,
    host: Box<dyn P2pHost>,
    keypair: libp2p::identity::Keypair,
    signer: EnvelopeSigner,
    now_fn: NowFn,
    app_version: String,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: mpsc::UnboundedSender<P2pEvent>,
    announce_validator: NodeAnnounceValidator,
    exchange_limiter: MinIntervalRateLimiter,
    recovery_limiter: MinIntervalRateLimiter,
    last_announced_at: i64,
    overlay_exchange_cursor: u64,
    started_emitted: bool,
    port_persisted: bool,
    pending_connects: Vec<PendingConnect>,
    pending_overlay_dials: HashMap<PeerId, ()>,
    version_probe_in_flight: HashSet<PeerId>,
    pending_version: HashMap<request_response::OutboundRequestId, PeerId>,
    pending_exchange: HashMap<request_response::OutboundRequestId, (PeerId, oneshot::Sender<Result<usize>>)>,
    pending_recovery: HashMap<request_response::OutboundRequestId, RecoverySession>,
    /// 同一恢复 session 的其余请求 → 首个请求 id。
    pending_recovery_extra: HashMap<request_response::OutboundRequestId, request_response::OutboundRequestId>,
    pending_forward: HashMap<request_response::OutboundRequestId, ForwardCtx>,
    /// 同一转发批次的其余请求 → 首个请求 id。
    pending_forward_extra: HashMap<request_response::OutboundRequestId, request_response::OutboundRequestId>,
    pending_org_attempts: Vec<OrgAttempt>,
}

impl<S: StorageBackend> EventLoop<S> {
    fn now(&self) -> i64 {
        (self.now_fn)()
    }

    fn emit(&self, event: P2pEvent) {
        let _ = self.event_tx.send(event);
    }

    fn self_peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }

    fn connected_peers(&self) -> HashSet<PeerId> {
        self.swarm.connected_peers().copied().collect()
    }

    fn listen_addr_strings(&self) -> Vec<String> {
        self.swarm
            .listeners()
            .chain(self.swarm.external_addresses())
            .map(ToString::to_string)
            .collect()
    }

    async fn run(mut self, keepalive_interval: Option<Duration>) {
        use libp2p::futures::StreamExt;
        let mut keepalive = keepalive_interval.map(tokio::time::interval);
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    if self.handle_command(cmd) {
                        break;
                    }
                }
                _ = async {
                    match keepalive.as_mut() {
                        Some(interval) => interval.tick().await,
                        None => std::future::pending::<tokio::time::Instant>().await,
                    }
                } => {
                    let stats = self.run_keepalive_tick();
                    self.emit(P2pEvent::KeepaliveTick(stats));
                }
            }
        }
        self.emit(P2pEvent::Stopped);
    }

    /// 返回 true 表示收到 Shutdown。
    fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Broadcast { topic, body, tx } => {
                let _ = tx.send(self.publish_envelope(&topic, body));
            }
            Command::AnnounceNow { tx } => {
                let _ = tx.send(self.publish_announce());
            }
            Command::ConnectPeer { node_info, tx } => self.begin_connect(node_info, tx),
            Command::ExchangeWithPeer { peer_id, tx } => self.begin_exchange(&peer_id, tx),
            Command::QueryRecovery { token, neighbors, want, tx } => {
                self.begin_recovery_query(&token, &neighbors, want, tx);
            }
            Command::OrgShareDirect { node_info, payload, tx } => {
                self.begin_org_attempt(node_info, payload, OrgTx::Share(tx), true);
            }
            Command::OrgPullRequest { node_info, request_json, tx } => {
                self.begin_org_attempt(node_info, Value::String(request_json), OrgTx::Pull(tx), false);
            }
            Command::LocalNodeInfo { tx } => {
                let _ = tx.send(self.local_node_info());
            }
            Command::Tick { tx } => {
                let _ = tx.send(self.run_keepalive_tick());
            }
            Command::Shutdown => return true,
        }
        false
    }

    fn local_node_info(&mut self) -> LocalP2PNodeInfo {
        let topic = gossipsub::IdentTopic::new(SYNC_TOPIC).hash();
        let subscribers: Vec<String> = self
            .swarm
            .behaviour()
            .gossipsub
            .all_peers()
            .filter(|(_, topics)| topics.contains(&&topic))
            .map(|(peer, _)| peer.to_base58())
            .collect();
        LocalP2PNodeInfo {
            started: true,
            peer_id: Some(self.self_peer_id().to_base58()),
            addresses: self.listen_addr_strings(),
            connected_peers: self.connected_peers().iter().map(ToString::to_string).collect(),
            spark_sync_subscribers: subscribers,
        }
    }

    // ------------------------------------------------------------------
    // 广播与信封
    // ------------------------------------------------------------------

    fn publish_envelope(&mut self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let evidence_head = self.host.evidence_head_hash();
        let mut envelope = Envelope::new(body, evidence_head, self.now());
        envelope.sign(&self.signer);
        let bytes = envelope.to_compact_json().into_bytes();
        self.publish_raw(topic, bytes)
    }

    fn publish_raw(&mut self, topic: &str, bytes: Vec<u8>) -> Result<()> {
        let ident = gossipsub::IdentTopic::new(topic);
        match self.swarm.behaviour_mut().gossipsub.publish(ident, bytes) {
            Ok(_) => Ok(()),
            // 对齐 allowPublishToZeroTopicPeers：零订阅者不算失败
            Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => Ok(()),
            Err(e) => Err(P2pError::Protocol(format!("publish failed: {e}"))),
        }
    }

    // ------------------------------------------------------------------
    // node-announce
    // ------------------------------------------------------------------

    fn publish_announce(&mut self) -> Result<bool> {
        let Some(addresses) = prepare_publish_addresses(&self.listen_addr_strings()) else {
            return Ok(false);
        };
        let count = addresses.len();
        let announce = sign_node_announce(
            &self.keypair,
            &self.self_peer_id().to_base58(),
            &addresses,
            self.now(),
        )
        .map_err(|e| P2pError::Swarm(format!("announce sign failed: {e}")))?;
        self.publish_raw(OVERLAY_TOPIC, announce_to_json(&announce).into_bytes())?;
        self.last_announced_at = self.now();
        self.emit(P2pEvent::AnnouncePublished { addresses: count });
        Ok(true)
    }

    fn handle_inbound_announce(&mut self, text: &str) {
        let now = self.now();
        let self_id = self.self_peer_id().to_base58();
        // 限流判定需要邻居池已存地址；先按消息里的 peerId 读取
        let known = {
            let parsed_peer = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.get("peerId")?.as_str().map(ToString::to_string));
            let mut store = OverlayPeerStore::new(&mut self.storage);
            match parsed_peer {
                Some(pid) => store
                    .get(&pid)
                    .ok()
                    .flatten()
                    .map(|r| r.addresses)
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        };
        match self.announce_validator.validate(text, &self_id, &known, now) {
            Ok(announce) => {
                let mut store = OverlayPeerStore::new(&mut self.storage);
                let _ = store.remember(
                    &announce.peer_id,
                    &announce.addresses,
                    OverlayPeerSource::Announce,
                    true,
                    now,
                );
                self.emit(P2pEvent::AnnounceAccepted {
                    peer_id: announce.peer_id,
                });
            }
            Err(_) => { /* 静默丢弃（TS 口径） */ }
        }
    }

    // ------------------------------------------------------------------
    // pubsub 业务消息（spark-sync）
    // ------------------------------------------------------------------

    fn handle_sync_message(&mut self, text: &str) {
        let verified = match super::envelope::parse_and_verify_envelope(text) {
            Ok(v) => v,
            Err(P2pError::SignatureInvalid) => {
                self.emit(P2pEvent::MessageDropped {
                    reason: "signature invalid".to_string(),
                });
                return;
            }
            Err(_) => {
                self.emit(P2pEvent::MessageDropped {
                    reason: "invalid json".to_string(),
                });
                return;
            }
        };
        if super::envelope::is_signature_mandatory_type(&verified.msg_type) && !verified.signed {
            self.emit(P2pEvent::MessageDropped {
                reason: format!("unsigned data message: {}", verified.msg_type),
            });
            return;
        }

        let map = &verified.map;
        let get_str = |key: &str| map.get(key).and_then(Value::as_str).map(ToString::to_string);
        match verified.msg_type.as_str() {
            "update" | "delete" => {
                let (Some(domain), Some(collection), Some(id)) =
                    (get_str("domain"), get_str("collection"), get_str("id"))
                else {
                    return;
                };
                let Some(meta) = map.get("meta").cloned() else {
                    return;
                };
                if meta.is_null() {
                    return;
                }
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                let schema = map.get("schema").cloned();
                if let Err(e) =
                    self.host.apply_remote_update(&domain, &collection, &id, payload, meta, schema)
                {
                    self.emit(P2pEvent::Warning(format!("apply remote update failed: {e}")));
                    return;
                }
                // 存证头不一致仅告警不丢弃
                if let Some(remote_head) = get_str("evidenceHeadHash")
                    && !remote_head.is_empty()
                    && self.host.evidence_head_hash().as_deref() != Some(remote_head.as_str())
                {
                    self.emit(P2pEvent::Warning(
                        "evidence head mismatch, peer may have diverged".to_string(),
                    ));
                }
                self.emit(P2pEvent::SyncMessageApplied {
                    msg_type: verified.msg_type.clone(),
                    domain,
                });
            }
            "history-response" => {
                let (Some(domain), Some(collection), Some(id)) =
                    (get_str("domain"), get_str("collection"), get_str("id"))
                else {
                    return;
                };
                let Some(meta) = map.get("meta").cloned().filter(|m| !m.is_null()) else {
                    return;
                };
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                let schema = map.get("schema").cloned();
                if let Err(e) =
                    self.host.apply_remote_update(&domain, &collection, &id, payload, meta, schema)
                {
                    self.emit(P2pEvent::Warning(format!("apply history-response failed: {e}")));
                    return;
                }
                self.emit(P2pEvent::SyncMessageApplied {
                    msg_type: verified.msg_type,
                    domain,
                });
            }
            "org-share" => {
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                match self.host.apply_incoming_org_share(payload, "pubsub") {
                    Ok(Some(ack)) => {
                        let org_id = ack.org_id.clone();
                        let sync_id = ack.sync_id.clone();
                        self.emit(P2pEvent::OrgShareAccepted {
                            org_id,
                            sync_id: sync_id.clone(),
                            source: "pubsub",
                        });
                        if let Some(sync_id) = &ack.sync_id {
                            let ack_payload = serde_json::json!({
                                "syncId": sync_id,
                                "orgId": ack.org_id,
                                "targetRootId": ack.target_root_id,
                                "receiverRootId": ack.receiver_root_id,
                            });
                            let body = super::envelope::build_org_body("org-share-ack", ack_payload);
                            if let Err(e) = self.publish_envelope(SYNC_TOPIC, body) {
                                self.emit(P2pEvent::Warning(format!("org-share-ack broadcast failed: {e}")));
                            }
                        }
                    }
                    Ok(None) => { /* 未接受，静默 */ }
                    Err(e) => self.emit(P2pEvent::Warning(format!("org-share apply failed: {e}"))),
                }
            }
            "org-share-ack" => {
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                if payload.get("syncId").and_then(Value::as_str).is_some() {
                    self.host.on_org_share_ack(payload);
                }
            }
            _ => { /* 插件自定义等：不强制签名，p2p 不处理 */ }
        }
    }

    // ------------------------------------------------------------------
    // 连接管理
    // ------------------------------------------------------------------

    fn begin_connect(&mut self, node_info: PeerNodeInfo, tx: oneshot::Sender<Result<()>>) {
        // 已连接即成功（重拨同一地址会因 TCP 四元组冲突失败，也无必要；
        // TS 侧 libp2p dial 已连接 peer 同样为 no-op 成功）
        if extract_peer_id(&node_info)
            .and_then(|s| s.parse::<PeerId>().ok())
            .is_some_and(|p| self.swarm.is_connected(&p))
        {
            let _ = tx.send(Ok(()));
            return;
        }
        let targets = match super::peer_targets::build_dial_targets(&node_info) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        let mut pending = PendingConnect {
            node_info,
            targets,
            current: None,
            tx,
            last_error: None,
        };
        if let Some(err) = self.dial_next_connect_target(&mut pending) {
            let info = pending.node_info.clone();
            self.remember_node_observation(&info, NodeObservation::Failure, Some(&err));
            let _ = pending.tx.send(Err(P2pError::Dial(format!(
                "Failed to connect peer by provided addresses: {err}"
            ))));
            return;
        }
        self.pending_connects.push(pending);
    }

    /// 尝试下一个拨号目标；全部耗尽时返回错误文本（由调用方回传终态）。
    fn dial_next_connect_target(&mut self, pending: &mut PendingConnect) -> Option<String> {
        while let Some(target) = pending.targets.pop_front() {
            match target.parse::<Multiaddr>() {
                Ok(ma) => {
                    let opts = if target.contains("/p2p/") {
                        DialOpts::from(ma)
                    } else {
                        DialOpts::unknown_peer_id().address(ma).build()
                    };
                    if self.swarm.dial(opts).is_ok() {
                        pending.current = Some(target);
                        return None;
                    }
                    pending.last_error = Some(format!("dial rejected: {target}"));
                }
                Err(e) => {
                    pending.last_error = Some(format!("invalid addr {target}: {e}"));
                }
            }
        }
        Some(
            pending
                .last_error
                .clone()
                .unwrap_or_else(|| "no dial targets".to_string()),
        )
    }

    fn remember_node_observation(&mut self, info: &PeerNodeInfo, obs: NodeObservation, error: Option<&str>) {
        let now = self.now();
        let mut store = PeerActivityStore::new(&mut self.storage);
        let _ = store.remember_node_info(info, obs, error, now);
    }

    // ------------------------------------------------------------------
    // peer-exchange
    // ------------------------------------------------------------------

    fn begin_exchange(&mut self, peer_id: &str, tx: oneshot::Sender<Result<usize>>) {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            let _ = tx.send(Err(P2pError::Malformed("invalid peer id".to_string())));
            return;
        };
        if !self.connected_peers().contains(&peer) {
            let _ = tx.send(Ok(0));
            return;
        }
        let request_id = self
            .swarm
            .behaviour_mut()
            .exchange_rr
            .send_request(&peer, direct::build_exchange_request(super::constants::PEER_EXCHANGE_MAX));
        self.pending_exchange.insert(request_id, (peer, tx));
    }

    fn handle_exchange_inbound_request(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        let respond = |behaviour: &mut SparkBehaviour, text: String| {
            let _ = behaviour.exchange_rr.send_response(channel, text);
        };
        let parsed: Option<Value> = serde_json::from_str(&request).ok();
        if parsed
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            != Some("peer-exchange-request")
        {
            respond(self.swarm.behaviour_mut(), direct::build_exchange_response(false, &[], None));
            return;
        }
        if self.exchange_limiter.is_rate_limited(&peer.to_base58(), now) {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_exchange_response(false, &[], Some("rate-limited")),
            );
            return;
        }
        let want = direct::normalize_exchange_want(parsed.as_ref().and_then(|v| v.get("want")));
        let samples = {
            let mut store = OverlayPeerStore::new(&mut self.storage);
            store
                .sample_for_exchange(
                    Some(&peer.to_base58()),
                    want,
                    now,
                    super::constants::PEER_EXCHANGE_MAX_AGE_MS,
                )
                .unwrap_or_default()
        };
        let samples: Vec<direct::PeerExchangeSample> = samples
            .into_iter()
            .map(|r| direct::PeerExchangeSample {
                peer_id: r.peer_id,
                addresses: r.addresses,
                last_seen_at: r.last_seen_at,
            })
            .collect();
        respond(
            self.swarm.behaviour_mut(),
            direct::build_exchange_response(true, &samples, None),
        );
    }

    fn handle_exchange_response(&mut self, request_id: request_response::OutboundRequestId, response: String) {
        let Some((responder, tx)) = self.pending_exchange.remove(&request_id) else {
            return;
        };
        let Some(samples) = direct::parse_exchange_response(&response) else {
            let _ = tx.send(Ok(0));
            return;
        };
        let now = self.now();
        let self_id = self.self_peer_id().to_base58();
        let responder_id = responder.to_base58();
        let mut merged = 0usize;
        {
            let mut store = OverlayPeerStore::new(&mut self.storage);
            for sample in samples.iter().take(super::constants::PEER_EXCHANGE_MAX) {
                if let Some((pid, addrs)) =
                    direct::filter_incoming_sample(sample, &self_id, &responder_id)
                {
                    let _ = store.remember(&pid, &addrs, OverlayPeerSource::Exchange, false, now);
                    merged += 1;
                }
            }
        }
        self.emit(P2pEvent::PeerExchangeCompleted {
            responder: responder_id,
            merged,
        });
        let _ = tx.send(Ok(merged));
    }

    // ------------------------------------------------------------------
    // org-recovery
    // ------------------------------------------------------------------

    fn begin_recovery_query(
        &mut self,
        token: &str,
        neighbors: &[String],
        want: usize,
        tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
    ) {
        let connected = self.connected_peers();
        let mut request_ids = Vec::new();
        for neighbor in neighbors.iter().take(3) {
            let Ok(peer) = neighbor.parse::<PeerId>() else {
                continue;
            };
            if !connected.contains(&peer) {
                continue;
            }
            let request_id = self.swarm.behaviour_mut().recovery_rr.send_request(
                &peer,
                direct::build_recovery_request(token, super::constants::RECOVERY_TTL, want),
            );
            request_ids.push(request_id);
        }
        if request_ids.is_empty() {
            let _ = tx.send(Ok(Vec::new()));
            return;
        }
        // 首个请求挂 session，其余请求经 extra 映射指向它；最后完成者汇总
        let first = request_ids[0];
        self.pending_recovery.insert(
            first,
            RecoverySession {
                remaining: request_ids.len(),
                collected: Vec::new(),
                tx,
            },
        );
        for id in request_ids.iter().skip(1) {
            self.pending_recovery_extra.insert(*id, first);
        }
    }

    fn answer_recovery(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        let Some(query) = direct::parse_recovery_request(&request) else {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(false, &[], None));
            return;
        };
        if self.recovery_limiter.is_rate_limited(&peer.to_base58(), now) {
            let _ = self.swarm.behaviour_mut().recovery_rr.send_response(
                channel,
                direct::build_recovery_response(false, &[], Some("rate-limited")),
            );
            return;
        }
        // 本地命中
        let view = self.host.recovery_view();
        if let Some(peers) = direct::match_recovery_view(&view, &query.token, query.want, now) {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(true, &peers, None));
            return;
        }
        // 转发：ttl>0 时向除请求方外的已连接邻居取前 2 个
        let ttl = direct::normalize_recovery_ttl(query.ttl);
        let connected: Vec<PeerId> = self
            .connected_peers()
            .into_iter()
            .filter(|p| *p != peer)
            .take(2)
            .collect();
        if ttl == 0 || connected.is_empty() {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(true, &[], None));
            return;
        }
        let mut ids = Vec::new();
        for neighbor in &connected {
            let request_id = self.swarm.behaviour_mut().recovery_rr.send_request(
                neighbor,
                direct::build_recovery_request(&query.token, ttl - 1, query.want),
            );
            ids.push(request_id);
        }
        let ctx = ForwardCtx {
            channel,
            remaining: ids.len(),
            collected: Vec::new(),
            want: query.want,
        };
        let first = ids[0];
        self.pending_forward.insert(first, ctx);
        for id in ids.iter().skip(1) {
            self.pending_forward_extra.insert(*id, first);
        }
    }

    // ------------------------------------------------------------------
    // org-share / org-pull 直连
    // ------------------------------------------------------------------

    fn begin_org_attempt(
        &mut self,
        node_info: PeerNodeInfo,
        payload: Value,
        tx: OrgTx,
        is_share: bool,
    ) {
        let targets = match super::peer_targets::build_dial_targets(&node_info) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                match tx {
                    OrgTx::Share(tx) => {
                        let _ = tx.send(Err(e));
                    }
                    OrgTx::Pull(tx) => {
                        let _ = tx.send(Err(e));
                    }
                }
                return;
            }
        };
        let (kind, request_json) = if is_share {
            let sync_id = payload
                .get("syncId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (
                OrgAttemptKind::Share {
                    expected_sync_id: sync_id,
                },
                direct::build_org_share_request(payload),
            )
        } else {
            let text = match payload {
                Value::String(s) => s,
                _ => String::new(),
            };
            (OrgAttemptKind::Pull, text)
        };
        let mut attempt = OrgAttempt {
            kind,
            targets,
            current_target: None,
            current_peer: None,
            request_json,
            in_flight: None,
            tx,
        };
        // 已连接则直接在现有连接上发请求：重拨同一地址会因 TCP 端口复用的
        // 四元组冲突（EADDRINUSE）失败，也无必要。
        let connected_peer = extract_peer_id(&node_info)
            .and_then(|s| s.parse::<PeerId>().ok())
            .filter(|p| self.swarm.is_connected(p));
        if let Some(peer) = connected_peer {
            let request_id = self
                .swarm
                .behaviour_mut()
                .org_share_rr
                .send_request(&peer, attempt.request_json.clone());
            attempt.in_flight = Some(request_id);
            attempt.current_peer = Some(peer);
            self.pending_org_attempts.push(attempt);
            return;
        }
        self.dial_next_org_target(&mut attempt);
        if attempt.current_target.is_some() {
            self.pending_org_attempts.push(attempt);
        } else {
            attempt.finish_exhausted();
        }
    }

    fn dial_next_org_target(&mut self, attempt: &mut OrgAttempt) {
        while let Some(target) = attempt.targets.pop_front() {
            match target.parse::<Multiaddr>() {
                Ok(ma) => {
                    let opts = if target.contains("/p2p/") {
                        DialOpts::from(ma)
                    } else {
                        DialOpts::unknown_peer_id().address(ma).build()
                    };
                    if self.swarm.dial(opts).is_ok() {
                        attempt.current_target = Some(target);
                        return;
                    }
                }
                Err(_) => continue,
            }
        }
    }

    fn handle_org_share_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let response = match direct::parse_org_share_request(&request) {
            Err(_) => direct::build_org_share_error_response("empty or invalid json"),
            Ok(None) => direct::build_org_share_error_response("invalid type"),
            Ok(Some((direct::OrgShareRequestKind::OrgShare, payload))) => {
                match self.host.apply_incoming_org_share(payload.clone(), "direct") {
                    Ok(Some(ack)) => {
                        self.emit(P2pEvent::OrgShareAccepted {
                            org_id: ack.org_id.clone(),
                            sync_id: ack.sync_id.clone(),
                            source: "direct",
                        });
                        direct::build_org_share_ack_response(
                            ack.sync_id.as_deref(),
                            &ack.org_id,
                            &ack.receiver_root_id,
                        )
                    }
                    _ => direct::build_org_share_error_response("not accepted"),
                }
            }
            Ok(Some((direct::OrgShareRequestKind::OrgPullList, payload))) => {
                match self.host.handle_org_pull_list(payload, Some(peer.to_base58())) {
                    Ok(value) => value.to_string(),
                    Err(e) => serde_json::json!({"ok": false, "type": "org-pull-list-response", "reason": e}).to_string(),
                }
            }
            Ok(Some((direct::OrgShareRequestKind::OrgPullOrg, payload))) => {
                match self.host.handle_org_pull_org(payload, Some(peer.to_base58())) {
                    Ok(value) => value.to_string(),
                    Err(e) => serde_json::json!({"ok": false, "type": "org-pull-org-response", "orgId": "", "reason": e}).to_string(),
                }
            }
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .org_share_rr
            .send_response(channel, response);
    }

    // ------------------------------------------------------------------
    // keepalive
    // ------------------------------------------------------------------

    fn run_keepalive_tick(&mut self) -> KeepaliveStats {
        let mut stats = KeepaliveStats::default();
        let now = self.now();

        // 1) 覆盖网拨号：活跃连接不足时从邻居池补拨
        let connected = self.connected_peers();
        let budget = keepalive::overlay_dial_budget(connected.len());
        if budget > 0 {
            let self_id = self.self_peer_id().to_base58();
            let mut exclude: HashSet<String> = connected.iter().map(ToString::to_string).collect();
            exclude.insert(self_id);
            let candidates = {
                let mut store = OverlayPeerStore::new(&mut self.storage);
                store.sample_dial_candidates(&exclude, budget).unwrap_or_default()
            };
            for candidate in candidates {
                let Ok(peer) = candidate.peer_id.parse::<PeerId>() else {
                    continue;
                };
                let addrs: Vec<Multiaddr> = candidate
                    .addresses
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .collect();
                if addrs.is_empty() {
                    continue;
                }
                let opts = DialOpts::peer_id(peer).addresses(addrs).build();
                if self.swarm.dial(opts).is_ok() {
                    self.pending_overlay_dials.insert(peer, ());
                    stats.overlay_dialed += 1;
                }
            }
        }

        // 2) peer-exchange：游标轮选一个已连接邻居
        let connected_strs: HashSet<String> = connected.iter().map(ToString::to_string).collect();
        if let Some(target) = keepalive::pick_exchange_target(
            &connected_strs,
            &self.self_peer_id().to_base58(),
            self.overlay_exchange_cursor,
        ) {
            self.overlay_exchange_cursor += 1;
            if let Ok(peer) = target.parse::<PeerId>() {
                let request_id = self
                    .swarm
                    .behaviour_mut()
                    .exchange_rr
                    .send_request(&peer, direct::build_exchange_request(super::constants::PEER_EXCHANGE_MAX));
                // tick 内发起的交换不带调用方等待器：完成后经事件上报
                let (tx, _rx) = oneshot::channel();
                self.pending_exchange.insert(request_id, (peer, tx));
                stats.exchanged = 1;
            }
        }

        // 3) node-announce 周期发布
        if now - self.last_announced_at >= NODE_ANNOUNCE_INTERVAL_MS
            && let Ok(true) = self.publish_announce()
        {
            stats.announced = true;
        }

        stats
    }

    // ------------------------------------------------------------------
    // swarm 事件
    // ------------------------------------------------------------------

    fn handle_swarm_event(&mut self, event: SwarmEvent<SparkBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { .. } => {
                if !self.port_persisted {
                    let addrs = self.listen_addr_strings();
                    if let Some(port) = listen_port::parse_ws_listen_port(&addrs)
                        && self.storage.put(P2P_LISTEN_WS_PORT, &port.to_string()).is_ok()
                    {
                        self.port_persisted = true;
                        self.emit(P2pEvent::ListenPortPersisted { port });
                    }
                }
                if !self.started_emitted {
                    self.started_emitted = true;
                    self.emit(P2pEvent::Started {
                        peer_id: self.self_peer_id().to_base58(),
                        listen_addresses: self.listen_addr_strings(),
                    });
                }
            }
            SwarmEvent::ExternalAddrConfirmed { .. } => {
                // 地址变化（UPnP 映射、relay 预约）→ 立即补发通告
                let _ = self.publish_announce();
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, num_established, ..
            } => {
                let now = self.now();
                {
                    let mut store = PeerActivityStore::new(&mut self.storage);
                    let _ = store.mark_connected(&peer_id.to_base58(), now);
                }
                // 连接沉淀进覆盖网邻居池
                let remote_addr = endpoint.get_remote_address().to_string();
                {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        std::slice::from_ref(&remote_addr),
                        OverlayPeerSource::Connect,
                        false,
                        now,
                    );
                }
                // 覆盖网补拨结果记账
                if self.pending_overlay_dials.remove(&peer_id).is_some() {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.mark_dial_result(&peer_id.to_base58(), true);
                }
                // connect 命令匹配
                let remote = remote_addr.clone();
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        let expected = extract_peer_id(&p.node_info)
                            .and_then(|s| s.parse::<PeerId>().ok());
                        expected == Some(peer_id)
                            || p.current.as_deref().is_some_and(|t| {
                                remote == t || remote.starts_with(&format!("{t}/")) || t.starts_with(&remote)
                            })
                    };
                    if matched {
                        let done = self.pending_connects.remove(i);
                        let info = done.node_info.clone();
                        self.remember_node_observation(&info, NodeObservation::Success, None);
                        let _ = done.tx.send(Ok(()));
                    } else {
                        i += 1;
                    }
                }
                // org 直连尝试匹配：连接成功即发请求
                let mut j = 0;
                while j < self.pending_org_attempts.len() {
                    let matched = {
                        let a = &self.pending_org_attempts[j];
                        a.current_target.as_deref().is_some_and(|t| {
                            remote == t || remote.starts_with(&format!("{t}/")) || t.starts_with(&remote)
                        }) || a.current_peer == Some(peer_id)
                    };
                    if matched {
                        let attempt = &mut self.pending_org_attempts[j];
                        let request_id = self
                            .swarm
                            .behaviour_mut()
                            .org_share_rr
                            .send_request(&peer_id, attempt.request_json.clone());
                        attempt.in_flight = Some(request_id);
                        attempt.current_peer = Some(peer_id);
                        break;
                    }
                    j += 1;
                }
                if num_established.get() == 1 {
                    self.emit(P2pEvent::PeerConnected {
                        peer_id: peer_id.to_base58(),
                    });
                }
                // 版本探测（in-flight 去重）
                if !self.version_probe_in_flight.contains(&peer_id) {
                    self.version_probe_in_flight.insert(peer_id);
                    let request_id = self
                        .swarm
                        .behaviour_mut()
                        .version_rr
                        .send_request(&peer_id, String::new());
                    self.pending_version.insert(request_id, peer_id);
                }
            }
            SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                if num_established == 0 {
                    let now = self.now();
                    let mut store = PeerActivityStore::new(&mut self.storage);
                    let _ = store.mark_disconnected(&peer_id.to_base58(), now);
                    self.emit(P2pEvent::PeerDisconnected {
                        peer_id: peer_id.to_base58(),
                    });
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                // connect 命令：失败则试下一目标
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        match (peer_id, extract_peer_id(&p.node_info).and_then(|s| s.parse::<PeerId>().ok())) {
                            (Some(actual), Some(expected)) => actual == expected,
                            (None, None) => true,
                            _ => false,
                        }
                    };
                    if matched {
                        let mut p = self.pending_connects.remove(i);
                        p.last_error = Some(error.to_string());
                        p.current = None;
                        match self.dial_next_connect_target(&mut p) {
                            None => self.pending_connects.push(p),
                            Some(err) => {
                                let info = p.node_info.clone();
                                self.remember_node_observation(&info, NodeObservation::Failure, Some(&err));
                                let _ = p.tx.send(Err(P2pError::Dial(format!(
                                    "Failed to connect peer by provided addresses: {err}"
                                ))));
                            }
                        }
                    } else {
                        i += 1;
                    }
                }
                // org 尝试：失败试下一目标
                let mut j = 0;
                while j < self.pending_org_attempts.len() {
                    let should_retry = {
                        let a = &self.pending_org_attempts[j];
                        a.in_flight.is_none()
                            && a.current_target.is_some()
                            && match (peer_id, a.current_peer) {
                                (Some(actual), Some(expected)) => actual == expected,
                                (None, None) => true,
                                (Some(_), None) => true,
                                _ => false,
                            }
                    };
                    if should_retry {
                        let mut a = self.pending_org_attempts.remove(j);
                        a.current_target = None;
                        self.dial_next_org_target(&mut a);
                        if a.current_target.is_some() {
                            self.pending_org_attempts.push(a);
                        } else {
                            a.finish_exhausted();
                        }
                    } else {
                        j += 1;
                    }
                }
                // 覆盖网补拨失败记账
                if let Some(peer) = peer_id
                    && self.pending_overlay_dials.remove(&peer).is_some()
                {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.mark_dial_result(&peer.to_base58(), false);
                }
            }
            SwarmEvent::Behaviour(behaviour_event) => self.handle_behaviour_event(behaviour_event),
            _ => {}
        }
    }

    fn handle_behaviour_event(&mut self, event: SparkBehaviourEvent) {
        match event {
            SparkBehaviourEvent::Gossipsub(gossipsub::Event::Message { message, .. }) => {
                let Ok(text) = String::from_utf8(message.data) else {
                    return;
                };
                if message.topic == gossipsub::IdentTopic::new(OVERLAY_TOPIC).hash() {
                    self.handle_inbound_announce(&text);
                } else {
                    self.handle_sync_message(&text);
                }
            }
            SparkBehaviourEvent::Mdns(mdns::Event::Discovered(peers)) => {
                let now = self.now();
                let mut store = OverlayPeerStore::new(&mut self.storage);
                for (peer_id, addr) in peers {
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        &[addr.to_string()],
                        OverlayPeerSource::Mdns,
                        false,
                        now,
                    );
                }
            }
            SparkBehaviourEvent::VersionRr(request_response::Event::Message { peer, message, .. }) => {
                match message {
                    request_response::Message::Request { channel, .. } => {
                        // 响应侧：连接打开即写版本帧（请求体为空）
                        let frame = direct::build_peer_version_response(
                            &self.app_version,
                            &self.self_peer_id().to_base58(),
                            self.now(),
                        );
                        let text = serde_json::to_string(&frame)
                            .unwrap_or_else(|_| "{}".to_string());
                        let _ = self.swarm.behaviour_mut().version_rr.send_response(channel, text);
                    }
                    request_response::Message::Response { request_id, response, .. } => {
                        if let Some(probed) = self.pending_version.remove(&request_id) {
                            self.version_probe_in_flight.remove(&probed);
                            if let Some(version) = direct::parse_peer_version_response(&response) {
                                self.host.on_peer_version(&version, &peer.to_base58());
                                self.emit(P2pEvent::PeerVersion {
                                    peer_id: peer.to_base58(),
                                    app_version: version,
                                });
                            }
                        }
                    }
                }
            }
            SparkBehaviourEvent::VersionRr(request_response::Event::OutboundFailure { request_id, .. }) => {
                if let Some(probed) = self.pending_version.remove(&request_id) {
                    self.version_probe_in_flight.remove(&probed);
                }
            }
            SparkBehaviourEvent::ExchangeRr(request_response::Event::Message { peer, message, .. }) => {
                match message {
                    request_response::Message::Request { request, channel, .. } => {
                        self.handle_exchange_inbound_request(peer, request, channel);
                    }
                    request_response::Message::Response { request_id, response, .. } => {
                        self.handle_exchange_response(request_id, response);
                    }
                }
            }
            SparkBehaviourEvent::ExchangeRr(request_response::Event::OutboundFailure { request_id, .. }) => {
                if let Some((responder, tx)) = self.pending_exchange.remove(&request_id) {
                    self.emit(P2pEvent::PeerExchangeCompleted {
                        responder: responder.to_base58(),
                        merged: 0,
                    });
                    let _ = tx.send(Ok(0));
                }
            }
            SparkBehaviourEvent::RecoveryRr(request_response::Event::Message { peer, message, .. }) => {
                match message {
                    request_response::Message::Request { request, channel, .. } => {
                        self.answer_recovery(peer, request, channel);
                    }
                    request_response::Message::Response { request_id, response, .. } => {
                        self.resolve_recovery_outbound(request_id, Some(response));
                    }
                }
            }
            SparkBehaviourEvent::RecoveryRr(request_response::Event::OutboundFailure { request_id, .. }) => {
                self.resolve_recovery_outbound(request_id, None);
            }
            SparkBehaviourEvent::OrgShareRr(request_response::Event::Message { peer, message, .. }) => {
                match message {
                    request_response::Message::Request { request, channel, .. } => {
                        self.handle_org_share_inbound(peer, request, channel);
                    }
                    request_response::Message::Response { request_id, response, .. } => {
                        self.resolve_org_response(request_id, response);
                    }
                }
            }
            SparkBehaviourEvent::OrgShareRr(request_response::Event::OutboundFailure { request_id, .. }) => {
                self.resolve_org_failure(request_id);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // recovery outbound 汇总
    // ------------------------------------------------------------------

    fn resolve_recovery_outbound(&mut self, request_id: request_response::OutboundRequestId, response: Option<String>) {
        // 转发上下文
        if let Some(first) = self.pending_forward_extra.remove(&request_id) {
            let mut respond_now = None;
            if let Some(ctx) = self.pending_forward.get_mut(&first) {
                if let Some(text) = response
                    && let Some(peers) = direct::parse_recovery_response(&text)
                {
                    ctx.collected.extend(peers);
                }
                ctx.remaining = ctx.remaining.saturating_sub(1);
                if ctx.remaining == 0
                    && let Some(ctx) = self.pending_forward.remove(&first)
                {
                    let merged = direct::dedupe_recovery_peers(ctx.collected, ctx.want);
                    respond_now = Some((ctx.channel, merged));
                }
            }
            if let Some((channel, merged)) = respond_now {
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .recovery_rr
                    .send_response(channel, direct::build_recovery_response(true, &merged, None));
            }
            return;
        }
        // 主查询 session
        if let Some(first) = self.pending_recovery_extra.remove(&request_id) {
            let mut finish = None;
            if let Some(session) = self.pending_recovery.get_mut(&first) {
                if let Some(text) = response
                    && let Some(peers) = direct::parse_recovery_response(&text)
                {
                    session.collected.extend(peers);
                }
                session.remaining = session.remaining.saturating_sub(1);
                if session.remaining == 0 {
                    finish = self.pending_recovery.remove(&first);
                }
            }
            if let Some(session) = finish {
                let merged = direct::dedupe_recovery_peers(
                    session.collected,
                    super::constants::RECOVERY_QUERY_WANT * 2,
                );
                let _ = session.tx.send(Ok(merged));
            }
            return;
        }
        if let Some(mut session) = self.pending_recovery.remove(&request_id) {
            if let Some(text) = response
                && let Some(peers) = direct::parse_recovery_response(&text)
            {
                session.collected.extend(peers);
            }
            session.remaining = session.remaining.saturating_sub(1);
            if session.remaining == 0 {
                let merged = direct::dedupe_recovery_peers(
                    session.collected,
                    super::constants::RECOVERY_QUERY_WANT * 2,
                );
                let _ = session.tx.send(Ok(merged));
            } else {
                self.pending_recovery.insert(request_id, session);
            }
        }
    }

    // ------------------------------------------------------------------
    // org 直连 outbound 汇总
    // ------------------------------------------------------------------

    fn resolve_org_response(&mut self, request_id: request_response::OutboundRequestId, response: String) {
        let mut i = 0;
        while i < self.pending_org_attempts.len() {
            if self.pending_org_attempts[i].in_flight == Some(request_id) {
                let mut attempt = self.pending_org_attempts.remove(i);
                attempt.in_flight = None;
                let delivered = match &attempt.kind {
                    OrgAttemptKind::Share { expected_sync_id } => {
                        direct::parse_org_share_direct_response(&response, expected_sync_id)
                    }
                    OrgAttemptKind::Pull => {
                        matches!(serde_json::from_str::<Value>(&response), Ok(v) if v.is_object())
                    }
                };
                if delivered {
                    match (&attempt.kind, attempt.tx) {
                        (OrgAttemptKind::Share { .. }, OrgTx::Share(tx)) => {
                            let _ = tx.send(Ok(true));
                        }
                        (OrgAttemptKind::Pull, OrgTx::Pull(tx)) => {
                            let value = serde_json::from_str::<Value>(&response).ok();
                            let _ = tx.send(Ok(value));
                        }
                        // 类别与通道不匹配属内部错误，按耗尽处理
                        (kind, tx) => {
                            let _ = kind;
                            match tx {
                                OrgTx::Share(tx) => {
                                    let _ = tx.send(Ok(false));
                                }
                                OrgTx::Pull(tx) => {
                                    let _ = tx.send(Ok(None));
                                }
                            }
                        }
                    }
                    return;
                }
                // 未送达/不可解析：下一个地址
                attempt.current_target = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.current_target.is_some() {
                    self.pending_org_attempts.push(attempt);
                } else {
                    attempt.finish_exhausted();
                }
                return;
            }
            i += 1;
        }
    }

    fn resolve_org_failure(&mut self, request_id: request_response::OutboundRequestId) {
        let mut i = 0;
        while i < self.pending_org_attempts.len() {
            if self.pending_org_attempts[i].in_flight == Some(request_id) {
                let mut attempt = self.pending_org_attempts.remove(i);
                attempt.in_flight = None;
                attempt.current_target = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.current_target.is_some() {
                    self.pending_org_attempts.push(attempt);
                } else {
                    attempt.finish_exhausted();
                }
                return;
            }
            i += 1;
        }
    }
}
