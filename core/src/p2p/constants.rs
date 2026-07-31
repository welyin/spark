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

/// dm（direct message：1:1 聊天消息与好友请求投递）直连协议名。
pub const DIRECT_DM_PROTOCOL: &str = "/spark/dm/1.0.0";

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

/// 「恢复中」状态的限时显示窗口（与冷却同周期：每轮恢复查询会刷新
/// `last_query_at`；超过一个周期未再发起查询，视为自动恢复无果转 failed）。
pub const RECOVERY_SEARCH_DISPLAY_MS: i64 = RECOVERY_COOLDOWN_MS;

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

/// 直连协议读超时：dm（10000ms，对齐 dm 单地址尝试量级；命令侧外层超时 15s）。
pub const DM_READ_TIMEOUT_MS: u64 = 10_000;

/// dm 应答侧限流：同一请求方两次服务的最小间隔（1s）。
pub const DM_MIN_INTERVAL_MS: i64 = 1_000;

/// Kad（Kademlia DHT）协议名。
pub const KAD_PROTOCOL_NAME: &str = "/spark/kad/1.0.0";

/// DHT 记录 TTL（8 小时；本地周期重发保活）。
pub const DHT_RECORD_TTL_SECS: u64 = 8 * 60 * 60;

/// 节点存在记录重发间隔：keepalive tick（60s）计数，每 240 tick ≈ 4 小时重发一次。
pub const DHT_REPUBLISH_TICKS: u64 = 240;

/// DHT 路由表目标规模参考（5–10，预留：后续用于健康度判定）。
pub const DHT_MIN_PEERS: usize = 5;

/// 节点存在记录的 DHT key 前缀（key = sha256 前缀 + peerId 的 hex，见 announce.rs）。
pub const DHT_NODE_RECORD_KEY_PREFIX: &str = "spark:node:";

/// node-challenge 直连协议名。
pub const NODE_CHALLENGE_PROTOCOL: &str = "/spark/node-challenge/1.0.0";

/// 直连协议读超时：node-challenge（3000ms）。
pub const NODE_CHALLENGE_READ_TIMEOUT_MS: u64 = 3_000;

/// challenge 回执时间戳新鲜度窗口（±60s）。
pub const CHALLENGE_MAX_AGE_MS: i64 = 60_000;

/// challenge 应答侧限流：同一请求方两次服务的最小间隔（2s）。
pub const CHALLENGE_MIN_INTERVAL_MS: i64 = 2_000;

/// DHT 模式配置的存储键（值 = off/client/server，缺省 server）。
pub const P2P_DHT_MODE_KEY: &str = "p2p:dht:mode";

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

// ------------------------------------------------------------------
// plugin-announce（插件市场广播索引，plugin-dist.md §8）
// ------------------------------------------------------------------

/// 插件声明广播 topic（§8.1）。
pub const PLUGIN_ANNOUNCE_TOPIC: &str = "/spark/plugin-announce/1.0.0";

/// 消息总大小上限 48 KiB（§8.1）。
pub const PLUGIN_ANNOUNCE_MAX_BYTES: usize = 48 * 1024;

/// 声明 TTL：30 天（§8.2/§8.5，本版固定值）。
pub const PLUGIN_ANNOUNCE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// timestamp 远未来容忍窗口（±10 min，对齐 node-announce，§8.5）。
pub const PLUGIN_ANNOUNCE_MAX_FUTURE_MS: i64 = 10 * 60_000;

/// PoW 最低难度（前导零 bit 数；§8.4，中档手机秒级）。
pub const PLUGIN_ANNOUNCE_MIN_POW_BITS: u32 = 20;

/// 逐 peer 限流：每小时接受条数上限（§8.6-2）。
pub const PLUGIN_ANNOUNCE_RATE_LIMIT_PER_HOUR: usize = 10;

/// 限流器跟踪 peer 数上限（满时回收过期、仍满清空，§8.6-2）。
pub const PLUGIN_ANNOUNCE_RATE_LIMIT_TRACKED_PEERS: usize = 1024;

/// relay 资历制阈值：传播源连续接入时长下限（72 小时，§8.6）。
pub const PLUGIN_ANNOUNCE_RELAY_TENURE_MS: i64 = 72 * 60 * 60 * 1000;

/// icon data:base64 字符数上限（20 KB 二进制，§8.2）。
pub const PLUGIN_ANNOUNCE_ICON_MAX_CHARS: usize = 28 * 1024;

/// releaseUrl / icon URL 长度上限（§8.2）。
pub const PLUGIN_ANNOUNCE_URL_MAX_CHARS: usize = 512;

/// version 字段长度上限（§8.2）。
pub const PLUGIN_ANNOUNCE_VERSION_MAX_CHARS: usize = 32;

/// 本地索引 sled 键前缀（§8.7）。
pub const PLUGIN_MARKET_INDEX_PREFIX: &str = "mkt:ann:";

/// 本地索引容量上限（§8.7，超限按 updatedAt 最旧 LRU 逐出）。
pub const PLUGIN_MARKET_INDEX_MAX: usize = 10_000;

/// 索引条目计数键（近似值，逐出时以全量扫描重写；注意避开 `mkt:ann:` 前缀）。
pub const PLUGIN_MARKET_INDEX_COUNT_KEY: &str = "mkt:ann-count";
