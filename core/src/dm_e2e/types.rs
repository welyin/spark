//! dm_e2e 类型与常量（p2p-dm §19.1.1 线形 + 会话密钥表存储键）。

/// 会话密钥表存储键前缀 `dm:e2e:key:`（personal 域，pdsync `dm:e2e` category）。
/// 键 = `{prefix}{peerRootId}`，**方向无关**（2026-08-11 架构师裁决）：A↔B
/// 共用一份会话密钥，A 端存 `dm:e2e:key:{B}`、B 端存 `dm:e2e:key:{A}`（值相同，
/// 各自经 pdsync 扩散到本方自设备；feed/chat 通道共用这份）。
pub const E2E_KEY_PREFIX: &str = "dm:e2e:key:";

/// X25519 域身份派生域串。**已废弃**（2026-08-11 架构师裁决：E2E 密钥协商
/// 改为 root 密钥直接转换，不再用域身份派生——dm-e2e 域身份导致对端公钥
/// 不可从 root 公钥推导）。保留常量仅作历史记录，生产接线不再使用。
pub const DM_E2E_DOMAIN: &str = "dm-e2e";

/// HKDF info 前缀；完整 info = `{HKDF_INFO_PREFIX}{lo}:{hi}`，其中 lo/hi 为
/// 字典序排序后的 from/to（**方向无关**，A→B 与 B→A 共用同一会话密钥）。
pub const HKDF_INFO_PREFIX: &str = "spark-dm-e2e-v1:";

/// 历史密钥容量上限：保留最近 `HISTORY_KEY_CAP` 个历史密钥供离线密文解密，
/// 超出淘汰最旧（牺牲最早时段的离线解密能力，换存储有界）。淘汰按 since
/// 升序丢最旧。**长期密钥恒等（2026-08-11 二轮修订）**：不再新增 history 条目
/// （轮换已去除），该容量仅约束历史存量记录，离线密文始终可用任一 current/
/// 历史条目解密。
pub const HISTORY_KEY_CAP: usize = 8;

/// 会话密钥表记录（`dm:e2e:key:{peerRootId}` 的值，JSON 线形）。
///
/// **长期密钥恒等（2026-08-11 二轮修订）**：root DH 派生确定性，长期会话密钥
/// 恒等、不轮换；前向保密由 per-message 临时密钥（信封 `ephPub`）承担。故
/// 本记录 `currentKey` 协商一次后原样复用，不再新增 `historyKeys` 条目——
/// 历史字段仅作**存量离线密文时段簿记**保留（`#[serde(default)]` 兼容旧记录
/// 反序列化），解密仍可回退读取。
///
/// `lastSeen` 记录**上次写入会话密钥表的时间**（密钥协商 / 对端 root 公钥
/// 积累 / 记录刷新等写盘动作），**不代表**「上次成功加解密」——加解密路径不
/// 刷新它（避免每次通讯高频写盘）。协议**不再**以 `lastSeen` 触发任何安全
/// 相关的密钥轮换（长期密钥恒等，轮换无意义）。
///
/// `peerRootPub`（2026-08-11 架构师裁决）记录**对端 root Ed25519 公钥**
/// （base64，= 对端信封 `pubKey` 字段）——E2E 密钥协商由域身份派生改为
/// **root 密钥直接转换**：发送方用接收方 root 公钥（`ed_pk_to_x25519`）+
/// 自己 root 私钥派生共享密钥，故需在密钥表记录对端 root 公钥供出站读取。
/// 入站验签通过时顺手写入（仅当与已存值不同才写，避免每次通讯都写盘）；
/// 出站从本字段读对端 root 公钥做 X25519 转换，无记录视为内部错误（不静默
/// 降级明文）。旧记录缺省该字段（`#[serde(default)]` 得 None）仍可正常解密
/// 既有密文，仅出站加密前需先经入站积累对端公钥。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionKeyRecord {
    /// 当前 AES-256 会话密钥（base64，32B）。
    #[serde(rename = "currentKey")]
    pub current_key: String,
    /// 当前密钥生效时间（毫秒）；`ts >= currentSince` 的密文用 current 解。
    #[serde(rename = "currentSince")]
    pub current_since: i64,
    /// 上次**写入会话密钥表**的时间（毫秒；密钥协商/对端公钥积累等写盘动作），
    /// 不代表成功加解密。缺省 0（旧记录兼容，不触发任何轮换逻辑）。
    #[serde(rename = "lastSeen", default)]
    pub last_seen: i64,
    /// 历史密钥（since 升序，最新在尾）；`ts` 落在某历史密钥时段内用它解。
    /// **存量**簿记：长期密钥恒等不再新增条目，仅兼容旧记录与旧离线密文解密。
    #[serde(rename = "historyKeys", default)]
    pub history_keys: Vec<HistoryKey>,
    /// 对端 root Ed25519 公钥（base64，= 对端信封 `pubKey`）。入站验签时
    /// 写入、出站读取做 X25519 转换。缺省 None（旧记录兼容）。
    #[serde(rename = "peerRootPub", default)]
    pub peer_root_pub: Option<String>,
}

/// 一段历史会话密钥（轮换前签发的密文在该时段内用此密钥解）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryKey {
    /// AES-256 会话密钥（base64，32B）。
    pub key: String,
    /// 该密钥生效时间（毫秒）。
    pub since: i64,
}
