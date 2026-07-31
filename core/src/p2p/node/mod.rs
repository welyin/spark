//! P2pNode：节点生命周期、事件循环、命令接口与事件流。
//!
//! - `P2pNode::start(config, storage, host)` 装配 libp2p（TCP+WS 双栈、双协议栈同端口）、
//!   持久化 Ed25519 身份、端口扫描与写回、订阅两个主题、注册五个直连协议，
//!   并按 `dht_mode` 挂载 Kad（公共 DHT：启动时从邻居池灌路由表并 bootstrap，
//!   keepalive tick 按间隔重发节点存在记录）；
//! - 事件循环在独立 tokio 任务内运行，宿主经 [`P2pEvent`] 流接收通知、经命令方法驱动；
//! - keepalive 60s tick：覆盖网维护（补拨/peer-exchange/node-announce）由循环内完成，
//!   组织层保活（候选拨号/反熵拉取/补副本/恢复触发）经 `P2pEvent::KeepaliveTick`
//!   交由宿主执行（宿主以 [`P2pNode`] 命令完成拨号与拉取）。
//!
//! 代码组织：本文件为节点句柄（启动/停止/事件流）与公开类型；对外命令方法在
//! `api`，事件循环主体在 `event_loop`，swarm 事件分发在 `swarm_events`，gossip
//! 入站与信封发布在 `gossip`，version/peer-exchange/org-recovery 三个
//! request-response 协议在 `rr_protocols`，org-share/org-pull 直连在
//! `org_direct`，dm（1:1 聊天/好友请求）直连在 `dm`，Kad DHT 与 node-challenge
//! 三层确认在 `dht`，keepalive tick 编排在 `tick`。

mod api;
mod dht;
mod dm;
mod event_loop;
mod gossip;
mod org_direct;
mod rr_protocols;
mod swarm_events;
#[cfg(test)]
mod tests;
mod tick;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::mpsc;

use crate::storage::StorageBackend;

use super::announce::NodeAnnounceValidator;
use super::behaviour::{BehaviourOptions, DhtMode, SparkBehaviour, build_behaviour};
use super::constants::{
    CHALLENGE_MIN_INTERVAL_MS, DM_MIN_INTERVAL_MS, ORG_KEEPALIVE_INTERVAL_MS, P2P_LISTEN_WS_PORT,
    PEER_EXCHANGE_MIN_INTERVAL_MS, RECOVERY_QUERY_MIN_INTERVAL_MS,
};
use super::direct::MinIntervalRateLimiter;
use super::envelope::EnvelopeSigner;
use super::host::P2pHost;
use super::identity_store::get_or_create_libp2p_keypair;
use super::listen_port;
use super::{P2pError, Result};

use api::Command;
use event_loop::EventLoop;

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
    /// DHT（Kad）运行模式；默认 Server。
    pub dht_mode: DhtMode,
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
            dht_mode: DhtMode::default(),
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
    Started {
        peer_id: String,
        listen_addresses: Vec<String>,
    },
    /// 实际监听端口写回持久化。
    ListenPortPersisted {
        port: u16,
    },
    PeerConnected {
        peer_id: String,
    },
    PeerDisconnected {
        peer_id: String,
    },
    /// 对端版本观察。
    PeerVersion {
        peer_id: String,
        app_version: String,
    },
    /// node-announce 已发布。
    AnnouncePublished {
        addresses: usize,
    },
    /// 入站 announce 验签通过并入池。
    AnnounceAccepted {
        peer_id: String,
    },
    /// peer-exchange 完成（合并条目数）。
    PeerExchangeCompleted {
        responder: String,
        merged: usize,
    },
    /// org-share 推送被接受（pubsub/直连）。
    OrgShareAccepted {
        org_id: String,
        sync_id: Option<String>,
        source: &'static str,
    },
    /// 数据类消息已交宿主落库。
    SyncMessageApplied {
        msg_type: String,
        domain: String,
    },
    /// dm 聊天消息投递通知（kernel 层 handle_dm 验签落库后发出；
    /// newtype 变体以序列化为 `{kind, data}` 形状）。
    ChatReceived(serde_json::Value),
    /// dm 消息状态（已读/撤回）通知。
    ChatStatus(serde_json::Value),
    /// 好友请求投递通知。
    FriendRequestReceived(serde_json::Value),
    /// 好友请求被接受通知。
    FriendRequestAccepted(serde_json::Value),
    /// 我发出的好友申请投递终态（pending=已送达等对方确认 / failed=投递失败
    /// 可重试；data 为 `{"request": <outbox 记录>}`，前端按 id upsert）。
    FriendRequestSent(serde_json::Value),
    /// 朋友资料更新通知（profile-sync 入站落库后发出；data 为
    /// `{"rootId", "nickname", "avatar"?}`，前端按 rootId 更新朋友资料）。
    FriendProfileUpdated(serde_json::Value),
    /// 组织邀请投递通知（入站 org-invite 落库后发出；data 为落库后的
    /// `OrgInviteRecord` JSON，前端按 id upsert）。
    OrgInviteReceived(serde_json::Value),
    /// 组织邀请状态更新（入站 org-invite-reply 校验通过并落库后发出；
    /// data 为更新后的 `OrgInviteRecord` JSON，前端按 id upsert）。
    OrgInviteUpdated(serde_json::Value),
    /// 消息被丢弃（验签失败/强制签名缺失/形状非法）。
    MessageDropped {
        reason: String,
    },
    /// keepalive tick 完成（宿主应执行组织层保活）。
    KeepaliveTick(KeepaliveStats),
    /// 非致命告警。
    Warning(String),
    /// 节点已停止。
    Stopped,
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
            dht_mode: config.dht_mode,
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
        let (dm_completion_tx, dm_completion_rx) = mpsc::unbounded_channel();
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
            challenge_limiter: MinIntervalRateLimiter::new(CHALLENGE_MIN_INTERVAL_MS),
            dm_limiter: MinIntervalRateLimiter::new(DM_MIN_INTERVAL_MS),
            peer_protocols: HashMap::new(),
            pending_challenge: HashMap::new(),
            pending_challenge_confirm: HashMap::new(),
            pending_dht_put: HashMap::new(),
            pending_dht_get: HashMap::new(),
            pending_dht_providers: HashMap::new(),
            provided_records: HashMap::new(),
            dht_tick_counter: 0,
            dm_completion_tx,
            dm_completion_rx,
            pending_dm_inbound: HashMap::new(),
            next_dm_task_id: 0,
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
