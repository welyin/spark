//! dm_offline 类型与常量（social-feed §6，纯逻辑层）。
//!
//! 统一密文暂存补投的存储语义：个人空间 `dm:pending:{toRootId}:{messageId}`，
//! 组织空间 `org:dm:pending:{orgId}:{toRootId}:{messageId}`。feed 复用个人空间键。

use serde::{Deserialize, Serialize};

/// 个人空间 pending 记录键前缀（`dm:pending:{toRootId}:{messageId}`）。
/// 经 pdsync `dm:pending` category 自设备扩散。
pub const PENDING_PREFIX: &str = "dm:pending:";

/// 组织空间 pending 记录键前缀（`org:dm:pending:{orgId}:{toRootId}:{messageId}`）。
/// 经 org-sync 网关同步（通道未就绪，先按个人空间同构落地，见模块注释）。
pub const ORG_PENDING_PREFIX: &str = "org:dm:pending:";

/// 离线暂存 TTL：7 天（过期丢弃，不补投）。
pub const PENDING_TTL_MS: i64 = 7 * 24 * 3600 * 1000;

/// 单 recipient 的 pending 上限（超出淘汰最旧）。
pub const PER_RECIPIENT_PENDING_CAP: usize = 100;

/// 全局 pending 上限（超出淘汰最旧）。
pub const GLOBAL_PENDING_CAP: usize = 1000;

/// 一条待补投的密文消息记录（`dm:pending:{to}:{messageId}` 的值）。
///
/// 存储的是**已加密的完整 dm 信封**（`envelope`）——暂存方只负责持有 + 补投，
/// 解密是接收方本地属性。附 `spaceKey`/`convId`/`messageId` 供补投成功后
/// compare-and-set 回写消息终态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRecord {
    /// 收件人 rootId。
    pub to: String,
    /// 消息 id（存储键用）。
    pub message_id: String,
    /// 信封 kind（`chat` / `feed` / `friend-request` 等）。
    pub kind: String,
    /// 消息所属空间键（`personal` 或 `org:<orgId>`），补投成功后回写用。
    #[serde(rename = "spaceKey")]
    pub space_key: String,
    /// 会话 id（chat 通道补投成功后回写消息状态用；非 chat 为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv_id: Option<String>,
    /// 已加密的完整 dm 信封（含签名/密文），补投时直接 `dm_direct` 重发。
    pub envelope: serde_json::Value,
    /// 入队时间（毫秒）；TTL 与淘汰都以它为准。
    pub created_at: i64,
}
