//! p2p 模块协议常量（逐一对齐 desktop/src/main/p2p/constants.ts 与
//! core/spec/p2p-messages.md §13 速查表）。

/// org-share 直连协议名。
pub const DIRECT_ORG_SHARE_PROTOCOL: &str = "/spark/org-share/1.0.0";

/// 对端版本探测协议名。
pub const DIRECT_VERSION_PROTOCOL: &str = "/spark/version/1.0.0";

/// peer-exchange 直连协议名。
pub const DIRECT_PEER_EXCHANGE_PROTOCOL: &str = "/spark/peer-exchange/1.0.0";

/// org-recovery 直连协议名。
pub const DIRECT_ORG_RECOVERY_PROTOCOL: &str = "/spark/org-recovery/1.0.0";

/// 本地持久化 libp2p 私钥的存储键（值 = protobuf 序列化的 base64）。
pub const P2P_IDENTITY_PRIVATE_KEY: &str = "p2p:identity:privateKey";

/// 本地持久化监听端口的存储键（十进制字符串）。
pub const P2P_LISTEN_WS_PORT: &str = "p2p:listen:wsPort";

/// 默认首选监听端口。
pub const P2P_DEFAULT_LISTEN_WS_PORT: u16 = 15002;

/// 端口扫描范围：从首选端口起向后扫描的端口个数。
pub const LISTEN_PORT_SCAN_RANGE: u16 = 50;

/// 节点活跃度记录前缀。
pub const P2P_PEER_RECORD_PREFIX: &str = "p2p:peer:record:";

/// 覆盖网邻居池记录前缀。
pub const P2P_OVERLAY_PEER_PREFIX: &str = "p2p:overlay:peer:";

/// 覆盖网邻居池容量上限。
pub const OVERLAY_POOL_MAX: usize = 200;

/// 单个 peer 最多保留的地址条数。
pub const MAX_ADDRESSES_PER_PEER: usize = 20;

/// 活跃覆盖网连接目标数。
pub const OVERLAY_DIAL_TARGET: usize = 4;

/// 每个 keepalive tick 允许的最大覆盖网拨号次数。
pub const OVERLAY_TICK_DIAL_BUDGET: usize = 2;

/// peer-exchange 单次的最大条目数。
pub const PEER_EXCHANGE_MAX: usize = 16;

/// 响应侧只分享该时间窗内见过的邻居（14 天）。
pub const PEER_EXCHANGE_MAX_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// 响应侧限流：同一请求方两次服务的最小间隔（60s）。
pub const PEER_EXCHANGE_MIN_INTERVAL_MS: i64 = 60_000;

/// 覆盖网控制面主题。
pub const OVERLAY_TOPIC: &str = "spark-overlay";

/// 业务数据主题。
pub const SYNC_TOPIC: &str = "spark-sync";

/// node-announce 周期发送间隔（5 分钟）。
pub const NODE_ANNOUNCE_INTERVAL_MS: i64 = 5 * 60_000;

/// 接收侧限流：同一 peerId 两次接受的最小间隔（60s）。
pub const NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_MS: i64 = 60_000;

/// 携带未知新地址时的接收侧限流下限（5s）。
pub const NODE_ANNOUNCE_ACCEPT_MIN_INTERVAL_ON_CHANGE_MS: i64 = 5_000;

/// announce 时间戳新鲜度窗口（±10 min）。
pub const NODE_ANNOUNCE_MAX_AGE_MS: i64 = 10 * 60_000;

/// 单条 announce 允许的地址数上限。
pub const MAX_ANNOUNCE_ADDRESSES: usize = 20;

/// 单条地址长度上限。
pub const MAX_ANNOUNCE_ADDRESS_LENGTH: usize = 512;

/// 恢复查询最大转发跳数。
pub const RECOVERY_TTL: u32 = 2;

/// 恢复查询冷却（全局单值，10 min）。
pub const RECOVERY_COOLDOWN_MS: i64 = 10 * 60_000;

/// 触发恢复查询前，组织侧"全员失联"需持续的 tick 数。
pub const RECOVERY_TRIGGER_CONSECUTIVE_TICKS: u32 = 3;

/// 单次恢复查询请求的成员条目上限。
pub const RECOVERY_QUERY_WANT: usize = 8;

/// 应答侧限流：同一请求方两次恢复查询服务的最小间隔（30s）。
pub const RECOVERY_QUERY_MIN_INTERVAL_MS: i64 = 30_000;

/// 组织同步记账前缀。
pub const ORG_SYNC_STATE_PREFIX: &str = "p2p:org-sync-state:";

/// 组织副本目标数（K，含本机）。
pub const ORG_REPLICA_TARGET: usize = 3;

/// 副本"新鲜"窗口（30 天）。
pub const ORG_REPLICA_FRESH_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// keepalive 保活周期（60s）。
pub const ORG_KEEPALIVE_INTERVAL_MS: i64 = 60_000;

/// 直连协议读超时：version 探测（2500ms）。
pub const VERSION_PROTOCOL_READ_TIMEOUT_MS: u64 = 2_500;

/// 直连协议读超时：peer-exchange 响应侧读请求（3000ms）。
pub const PEER_EXCHANGE_READ_REQUEST_TIMEOUT_MS: u64 = 3_000;

/// 直连协议读超时：peer-exchange 请求侧读响应（4000ms）。
pub const PEER_EXCHANGE_READ_RESPONSE_TIMEOUT_MS: u64 = 4_000;

/// 直连协议读超时：org-recovery（3000ms）。
pub const ORG_RECOVERY_READ_TIMEOUT_MS: u64 = 3_000;

/// 直连协议读超时：org-share / org-pull（4000ms）。
pub const ORG_SHARE_READ_TIMEOUT_MS: u64 = 4_000;

/// relay server 预约参数（对齐 TS circuitRelayServer 配置）。
pub const RELAY_MAX_RESERVATIONS: usize = 15;
/// relay server 默认预约时长（2 小时）。
pub const RELAY_DEFAULT_DURATION_LIMIT_SECS: u64 = 2 * 60 * 60;
/// relay server 默认流量上限（256 MiB）。
pub const RELAY_DEFAULT_DATA_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

/// peer-activity 清除阈值：连续失败次数。
pub const PEER_ACTIVITY_FAILURE_PURGE_THRESHOLD: u32 = 10;

/// 打分公式系数：成功一次 +60s 等效在线时长。
pub const PEER_ACTIVITY_SUCCESS_WEIGHT_MS: i64 = 60_000;

/// 打分公式系数：失败一次 -30s。
pub const PEER_ACTIVITY_FAILURE_WEIGHT_MS: i64 = 30_000;
