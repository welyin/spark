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
mod rediscovery;
mod relay_manager;
mod rr_protocols;
mod swarm_events;
#[cfg(test)]
mod tests;
mod tick;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::mpsc;

use crate::storage::StorageBackend;

use super::announce::NodeAnnounceValidator;
use super::behaviour::{BehaviourOptions, DhtMode, SparkBehaviour, build_behaviour};
use super::constants::{
    CHALLENGE_MIN_INTERVAL_MS, DM_MIN_INTERVAL_MS, ORG_KEEPALIVE_INTERVAL_MS, P2P_LISTEN_WS_PORT,
    PEER_EXCHANGE_MIN_INTERVAL_MS, PLUGIN_ANNOUNCE_MIN_POW_BITS, PLUGIN_ANNOUNCE_RELAY_TENURE_MS,
    RECOVERY_QUERY_MIN_INTERVAL_MS,
};
use super::direct::MinIntervalRateLimiter;
use super::envelope::EnvelopeSigner;
use super::host::P2pHost;
use super::identity_store::get_or_create_libp2p_keypair;
use super::listen_port;
use super::plugin_announce::PluginAnnounceValidator;
use super::{P2pError, Result};

// pub(crate) 再导出：F6 故障注入单测（kernel org-sync stall_tests）经
// `P2pNode::stub_for_test` 持有命令接收端，需命名 Command 做选择性应答。
pub(crate) use api::Command;
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
    /// plugin-announce PoW 最低难度覆盖（§8.4；None = 网络常量 20，测试调低）。
    pub plugin_announce_pow_bits: Option<u32>,
    /// plugin-announce relay 资历阈值覆盖（§8.6；None = 72h，测试调 0/调大）。
    pub plugin_announce_relay_tenure_ms: Option<i64>,
    /// DHT 周期重发间隔覆盖（tick 计数；None = 默认 240 ≈ 4h，移动端传 120 ≈ 2h，
    /// peer-rediscovery §4.2）。
    pub dht_republish_ticks: Option<u64>,
    /// 是否启用 relay server（接受他人预约）。桌面默认 true；移动端强制 false
    /// （节省流量与电量，移动端只作 relay client，peer-rediscovery §7.2）。
    pub enable_relay_server: bool,
    /// 叶子模式（mobile-leaf-mode §3）：只消费不服务——kad 仅一次性查询客户端，
    /// gossipsub announce 发布 / peer-exchange / overlay 邻居池维护全关；
    /// mDNS、relay client、监听、直连协议全部保留。桌面默认 false，移动端注入 true。
    pub leaf_mode: bool,
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
            plugin_announce_pow_bits: None,
            plugin_announce_relay_tenure_ms: None,
            dht_republish_ticks: None,
            enable_relay_server: true,
            leaf_mode: false,
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

/// AutoNAT 公网判定快照（U1 relay 状态页，relay-implementation §3）。
/// 事件循环只在 `StatusChanged` 时更新；启动初值 Unknown。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NatStatusLabel {
    Public,
    Private,
    #[default]
    Unknown,
}

impl NatStatusLabel {
    /// 线形字符串（public/private/unknown）。
    pub fn as_str(&self) -> &'static str {
        match self {
            NatStatusLabel::Public => "public",
            NatStatusLabel::Private => "private",
            NatStatusLabel::Unknown => "unknown",
        }
    }
}

/// relay 预约状态条目（U1 状态页；serde 线形 camelCase）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayReservationInfo {
    /// relay 节点 peerId。
    pub peer: String,
    /// 到期剩余（ms；近似值：预约建立时刻 + 默认预约时长 2h——libp2p relay
    /// client 不回传实际授予时长，且存活期间自动续期，该值仅作展示下界）。
    pub expires_in_ms: i64,
    /// 流量配额（字节，relay server 默认上限常量；客户端侧实际授予值不回传）。
    pub limit_bytes: u64,
    /// 已用流量（libp2p relay client 不暴露用量统计，恒 None——U1 展示「未统计」）。
    pub used_bytes: Option<u64>,
}

/// 本机 relay 状态快照（U1 状态页 + U2 托管向导自检，relay-implementation §3；
/// 只读 facade，无写路径）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalRelayStatus {
    /// AutoNAT 公网判定（public/private/unknown）。
    pub autonat: String,
    /// relay server 角色（serving=服务中 / off=关闭）。
    pub relay_role: String,
    /// 当前活跃预约。
    pub reservations: Vec<RelayReservationInfo>,
    /// spark:relay 共享池候选数（R2 get_providers 落点）。
    pub pool_size: usize,
    /// R1 动态 IP 降权标记（true = stability low）。
    pub stability_low: bool,
    /// UPnP 端口映射状态（U2 向导三态）：mapped=存在有效外部映射 /
    /// failed=映射过期或网关探测失败 / unknown=尚无 UPnP 事件。
    pub upnp: String,
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
    /// 应用层就绪：版本探测成功（对端响应 `/spark/version/1.0.0`），同一连接
    /// 上 Spark 应用层协议可通信。作为 profile-sync / flush_pending /
    /// device-notice 等业务投递的唯一触发信号（peer-app-ready-event §3.3）。
    /// 与 `PeerConnected`（transport 层首个连接建立）语义区分。serde 线形为
    /// `{kind:"peerAppReady", data:{peerId, appVersion}}`，旧壳层忽略未知 kind。
    PeerAppReady {
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
    /// 本机资料被自设备同步更新（自设备 profile-sync 全量快照应用身份文件
    /// 后发出；data 为 `{"nickname", "avatar"?}`，前端刷新当前用户展示）。
    SelfProfileSynced(serde_json::Value),
    /// 设备清单记录更新（device-sync 入站落库或本机采集刷新后发出；data 为
    /// `DeviceRecord` JSON，前端设备管理页按 peerId upsert）。
    DeviceUpdated(serde_json::Value),
    /// 自设备设备加入/变更通知入站生效（from==to==自己；data 为
    /// `{"kind","deviceId","deviceName","ts"}`）。
    DeviceNoticeReceived(serde_json::Value),
    /// 通讯录被自设备快照更新（contact-sync 入站合入后发出；data 为
    /// `{"applied": n}`，前端整页刷新个人空间通讯录）。
    ContactsSynced(serde_json::Value),
    /// 组织域数据被自设备 pdsync 更新（`org:meta:*` 组织记录 / `ct:org:*`
    /// 组织空间成员附加资料·标签·分组树等合入后发出；data 为
    /// `{"orgMeta": n, "orgContacts": n}`，前端刷新组织列表与组织空间通讯录、
    /// 组织身份扩展字段）。与 `ContactsSynced`（个人空间）分立：作用域不同。
    OrgSynced(serde_json::Value),
    /// 远端合入了插件声明式数据（P6）：`pdsync-data` 中 `pdoc:`/`pdecl:` 键
    /// 新合入时按集合名聚合发出。本地写不触发（插件本地路径即时可见）。
    /// data 为 `{"pluginId", "name", "keys"}`（keys = 完整存储键）；壳层转
    /// iframe 桥订阅，内核插件路由任务投递后台运行时 onChange。
    PluginDataChanged(serde_json::Value),
    /// 会话元数据被自设备快照更新（conv-sync 入站合入后发出；data 为
    /// `{"applied": n}`，前端刷新个人空间会话列表）。
    ConversationsSynced(serde_json::Value),
    /// 组织邀请投递通知（入站 org-invite 落库后发出；data 为落库后的
    /// `OrgInviteRecord` JSON，前端按 id upsert）。
    OrgInviteReceived(serde_json::Value),
    /// 组织邀请状态更新（入站 org-invite-reply 校验通过并落库后发出；
    /// data 为更新后的 `OrgInviteRecord` JSON，前端按 id upsert）。
    OrgInviteUpdated(serde_json::Value),
    /// 社交定向投递入站（feed 信封验签/解密/落收件箱后发出，social-feed §8）。
    /// data 为 `{"topic", "from", "feedId", "payload", "ts", "replyTo"?}`，
    /// 前端/插件按 topic 前缀路由到对应插件实时刷新（pull 补读兜底）。
    FeedReceived(serde_json::Value),
    /// 消息被丢弃（验签失败/强制签名缺失/形状非法）。
    MessageDropped {
        reason: String,
    },
    /// plugin-announce 入站声明校验通过并入索引（plugin-dist §8.9）。
    PluginAnnounceReceived {
        id: String,
        publisher: String,
    },
    /// plugin-announce 懒惰核查落终态（§8.8/§8.9；verified=false 时 error 记原因）。
    PluginAnnounceVerified {
        id: String,
        verified: bool,
        error: Option<String>,
    },
    /// keepalive tick 完成（宿主应执行组织层保活）。
    KeepaliveTick(KeepaliveStats),
    /// M5 延迟恢复请求状态更新（initiated / vetoed / committed）。`from_device`
    /// 为发送方 peerId 标注（展示/日志用，如「设备 X 正在发起密码重置」）；
    /// `deadline` 为**本机关心的截止时间**——发起方=pending.deadline，接收方
    /// initiated=本地到达+否决窗（§4.4 时钟安全，不信对端单一时钟）。
    RecoveryUpdated {
        request_id: String,
        state: String,
        op: Option<String>,
        deadline: Option<i64>,
        from_device: String,
    },
    /// M3 乙+校验器：检测到口令变更标记（改密/重置），UI 触发器（仅提示，
    /// 数据面权威是 `pwv:self`/epoch:state）。`reason ∈ password_change|password_reset`。
    PasswordChangeObserved {
        rotated_at: u64,
        rotated_by: String,
        rotated_by_device: String,
        reason: String,
    },
    /// M3 乙+校验器：本机完成口令统一（unify 重封），其他设备撤提示。
    PasswordUnificationDone {
        rotated_at: u64,
    },
    /// M3 D′：本机已超出统一密码宽限期（本机判定，非广播）。
    DeviceOutOfGrace {
        password_changed_at: u64,
        grace_ms: u64,
    },
    /// 非致命告警。
    Warning(String),
    /// 节点已停止。
    Stopped,
}

/// P2P 节点句柄。
pub struct P2pNode {
    peer_id: String,
    /// Ed25519 公钥 base64（32B 原始字节），M3 epoch 包裹采集用。
    device_pub_key: String,
    /// 叶子模式快照（mobile-leaf-mode §3；内核组织单连接/寻址链按它分支）。
    leaf_mode: bool,
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
        let device_pub_key = keypair
            .public()
            .try_into_ed25519()
            .ok()
            .map(|pk| base64::engine::general_purpose::STANDARD.encode(pk.to_bytes()))
            .unwrap_or_default();

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
            enable_relay_server: config.enable_relay_server,
            // leaf 模式 §3：kad 仅一次性查询客户端（只发查询、不应答、不入他人
            // 路由表）；显式 Off（隐私开关）优先，不被 leaf 覆盖
            dht_mode: match (config.leaf_mode, config.dht_mode) {
                (true, DhtMode::Server) => DhtMode::Client,
                (_, mode) => mode,
            },
            leaf_mode: config.leaf_mode,
        };
        let mut swarm = build_swarm(&keypair, &behaviour_options).await?;

        // Android 无 ws 传输层（见 build_swarm 门控注释），监听地址同步禁用 ws
        let enable_ws = config.enable_ws && !cfg!(target_os = "android");
        let addrs = build_listen_addrs(port, ipv6, config.enable_tcp, enable_ws);
        // 逐地址尽力监听：任一地址绑定失败仅告警降级，**不整体回退**——此前
        // 双栈集合任一失败即整体重建为 IPv4 单栈（Windows 上 IPv6 ws 绑定
        // 失败 → PC 失去全部 tcp6 监听 → 蜂窝场景手机拨 PC 全局 IPv6 必败
        // ConnectionRefused（真机实测：ping6 可达但端口无人监听）。只有全部失败才报错。
        let mut bound = 0usize;
        for addr in &addrs {
            let ma: Multiaddr = addr
                .parse()
                .map_err(|e| P2pError::Swarm(format!("invalid listen addr {addr}: {e}")))?;
            match swarm.listen_on(ma) {
                Ok(_) => bound += 1,
                Err(e) => log::warn!("[p2p] listen failed on {addr}（尽力监听，跳过）: {e}"),
            }
        }
        if bound == 0 {
            return Err(P2pError::Swarm(
                "listen failed on all addresses".to_string(),
            ));
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cmd_tx_loop = cmd_tx.clone();
        let (dm_completion_tx, dm_completion_rx) = mpsc::unbounded_channel();
        let (dial_timeout_tx, dial_timeout_rx) = mpsc::unbounded_channel();
        let event_loop = EventLoop {
            swarm,
            storage,
            host,
            keypair,
            signer: EnvelopeSigner::generate(),
            now_fn: config.now_fn.clone(),
            app_version: config.app_version.clone(),
            leaf_mode: config.leaf_mode,
            cmd_rx,
            cmd_tx: cmd_tx_loop,
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
            dht_republish_ticks: config
                .dht_republish_ticks
                .unwrap_or(crate::p2p::constants::DHT_REPUBLISH_TICKS),
            pending_network_change: None,
            last_network_change_fired_at: None,
            last_network_snapshot: None,
            rediscovery_states: HashMap::new(),
            rediscovery_dht_queries: HashMap::new(),
            pending_rediscovery_confirm: HashMap::new(),
            relay_reservations: Vec::new(),
            relay_reservations_inflight: std::collections::HashSet::new(),
            enable_relay_server: config.enable_relay_server,
            relay_pool_queries: std::collections::HashSet::new(),
            relay_pool_candidates: Vec::new(),
            relay_pool_stability: std::collections::HashMap::new(),
            relay_pool_record_queries: std::collections::HashSet::new(),
            last_relay_pool_query_at: 0,
            network_change_log: Vec::new(),
            relay_stability_low: false,
            nat_status: NatStatusLabel::default(),
            upnp_mapping: None,
            upnp_failed: false,
            dm_completion_tx,
            dm_completion_rx,
            dial_timeout_tx,
            dial_timeout_rx,
            pending_dm_inbound: HashMap::new(),
            next_dm_task_id: 0,
            plugin_announce_validator: PluginAnnounceValidator::new(
                config
                    .plugin_announce_pow_bits
                    .unwrap_or(PLUGIN_ANNOUNCE_MIN_POW_BITS),
            ),
            plugin_announce_tenure_ms: config
                .plugin_announce_relay_tenure_ms
                .unwrap_or(PLUGIN_ANNOUNCE_RELAY_TENURE_MS),
            peer_connected_since: HashMap::new(),
            topic_cache: HashMap::new(),
            org_pull_blackhole: false,
            stalled_pull_channels: Vec::new(),
        };
        let keepalive_interval = config.keepalive_interval;
        let task = tokio::spawn(async move {
            event_loop.run(keepalive_interval).await;
        });

        Ok(Self {
            peer_id: peer_id_str,
            device_pub_key,
            leaf_mode: config.leaf_mode,
            cmd_tx,
            event_rx,
            task: std::sync::Mutex::new(Some(task)),
        })
    }

    /// 本机 PeerId 字符串。
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// 是否叶子模式（mobile-leaf-mode §3：只消费不服务；内核侧组织单连接
    /// 目标排序与自设备寻址链按它分支）。
    pub fn leaf_mode(&self) -> bool {
        self.leaf_mode
    }

    /// 本机 Ed25519 公钥 base64（32B 原始字节）。
    pub fn device_pub_key(&self) -> &str {
        &self.device_pub_key
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

    /// 测试用桩节点（F6 故障注入单测）：命令通道接收端交还测试持有——
    /// 测试扮演「假事件循环」，选择性应答/挂起命令（如 org_pull_request
    /// 挂起复现对端长超时）。无真实网络与事件循环。
    #[cfg(test)]
    pub(crate) fn stub_for_test() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        (
            Self {
                peer_id: "stub-peer".to_string(),
                device_pub_key: String::new(),
                leaf_mode: false,
                cmd_tx,
                event_rx,
                task: std::sync::Mutex::new(None),
            },
            cmd_rx,
        )
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
    let builder = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .map_err(|e| P2pError::Swarm(format!("tcp security: {e}")))?;

    // websocket 传输层仅桌面端启用：libp2p 默认 DNS 解析器（hickory）依赖
    // /etc/resolv.conf，Android 上不存在该文件，with_websocket 初始化即失败
    // （swarm error: websocket: Dns）导致整个 swarm 起不来。原生节点间走 TCP，
    // WS 只有浏览器节点/WS 中继才需要，移动端暂不需要。
    #[cfg(target_os = "android")]
    let builder = builder;
    #[cfg(not(target_os = "android"))]
    let builder = builder
        .with_websocket(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .await
        .map_err(|e| P2pError::Swarm(format!("websocket: {e}")))?;

    let swarm = builder
        .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .map_err(|e| P2pError::Swarm(format!("relay client: {e}")))?
        .with_behaviour(|key, relay_client| {
            build_behaviour(key, relay_client, &options)
                .expect("behaviour construction is infallible for valid keypair")
        })
        .expect("behaviour constructor is infallible")
        // idle 连接 600s 超时（V2）：60s 会让 kad 路由表 peer 恒处于未连接态，
        // kad 行为层随即对其用 PortUse::Reuse 自动重拨（刷屏首因），延长后大幅
        // 减少这类无效自动拨号
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();
    Ok(swarm)
}
