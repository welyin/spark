//! 消息模块的记录类型与存储键构造（对齐 `app/src/mock/messages.ts` 的
//! `Conversation`/`ChatMessage`，字段按 serde camelCase 落盘）。
//!
//! 存储键（空间隔离由调用方以 `space` 参数体现；存储后端本身已按身份隔离）：
//! - 会话：`msg:conv:{space}:{convId}` → [`ConversationRecord`]
//! - 消息：`msg:item:{space}:{convId}:{createdAt:013}:{msgId}` → [`MessageRecord`]
//! - 消息 id 二级索引：`msg:byid:{space}:{convId}:{msgId}` → 消息存储键
//!
//! 消息键的时间戳段用 13 位零填充，保证 scan 字典序 = 时间序（仅对非负
//! 时间戳成立：负数带符号位填充后字典序与数值序相反，故入站消息拒绝
//! `created_at <= 0`，校验在 kernel 入站编排侧）。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// 会话键前缀。
pub(crate) const CONVERSATION_KEY_PREFIX: &str = "msg:conv:";
/// 消息键前缀。
pub(crate) const MESSAGE_KEY_PREFIX: &str = "msg:item:";
/// 消息 id 二级索引键前缀。
pub(crate) const MESSAGE_ID_INDEX_PREFIX: &str = "msg:byid:";

/// 撤回窗口：发送后 2 分钟内允许撤回（ui-messages.md §9.1）。
pub const RECALL_WINDOW_MS: i64 = 2 * 60_000;

/// 文本消息正文上限（16 KiB，UTF-8 字节；入站/出站两侧都校验）。
/// 与传输层的关系：直连协议单帧上限为 1 MiB（p2p/behaviour.rs
/// `MAX_FRAME_LEN`），信封还包含签名、消息元数据与引用等开销——正文
/// 限制在 16 KiB 可保证整条信封远低于帧上限，同时拦截畸形超大正文。
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

/// 会话类型（`"direct"` 单聊 / `"system"` 系统通知、组织公告）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationKind {
    /// 1:1 单聊。
    Direct,
    /// 系统通知 / 组织公告。
    System,
}

/// 消息类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    /// 文本。
    Text,
    /// 图片。
    Image,
    /// 文件（`content` 为文件名，`file_size` 有值）。
    File,
    /// 链接（随消息携带 [`LinkPreview`]）。
    Link,
    /// 语音（`duration` 为秒）。
    Voice,
    /// 系统提示文案。
    System,
}

/// 对方节点的可寻址信息（随会话保存，发送时供 p2p 直连/网关寻址）。
///
/// 全仓唯一的 PeerRef 定义：contact 模块（朋友/申请记录随附的寻址信息）
/// 直接 `pub use` 本类型，勿再另建同形结构。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRef {
    /// libp2p peerId。
    pub peer_id: String,
    /// multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// 链接预览卡片（ui-messages.md §6），元数据由发送方本地抓取随消息携带。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPreview {
    /// 原始链接。
    pub url: String,
    /// 页面标题。
    pub title: String,
    /// 页面描述。
    pub description: String,
    /// 来源 APP 名（域名白名单映射），未知域名时等于 `domain`。
    pub site_name: String,
    /// 域名（去 `www.`）。
    pub domain: String,
}

/// 引用回复携带的原消息片段（ui-messages.md §9.3）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteRef {
    /// 原消息 id。
    pub message_id: String,
    /// 原消息发送者显示名。
    pub sender_name: String,
    /// 原消息缩略文本。
    pub preview: String,
}

/// 会话记录（键 `msg:conv:{space}:{convId}`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    /// 会话 id（空间内唯一）。
    pub id: String,
    /// direct / system。
    pub kind: ConversationKind,
    /// 显示标题（联系人备注名解析在前端/kernel 视图层完成）。
    pub title: String,
    /// 对方 rootId（system 会话为约定值如 `"system"`）。
    pub peer_root_id: String,
    /// 对方节点寻址信息（可省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerRef>,
    /// 未读数。
    pub unread_count: u32,
    /// 置顶时间戳，0 表示未置顶（ui-messages.md §2.3）。
    pub pinned_at: i64,
    /// 免打扰。
    pub muted: bool,
    /// 草稿文本。
    pub draft: String,
    /// 最后一条消息时间。
    pub updated_at: i64,
}

impl Default for ConversationKind {
    fn default() -> Self {
        Self::Direct
    }
}

impl Default for MessageType {
    fn default() -> Self {
        Self::Text
    }
}

/// 消息记录（键 `msg:item:{space}:{convId}:{createdAt:013}:{msgId}`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRecord {
    /// 消息 id（会话内唯一，见 [`generate_message_id`]）。
    pub id: String,
    /// 发送者真实 rootId（`'me'` 的映射在 kernel 视图层做，本层不感知）。
    pub sender_id: String,
    /// 发送者显示名（冗余落盘，避免展示时回查）。
    pub sender_name: String,
    /// 消息类型。
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    /// 文本内容；文件消息为文件名；系统消息为提示文案。
    pub content: String,
    /// 文件大小（字节），仅文件消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    /// 语音时长（秒），仅语音消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// 链接预览，仅链接消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkPreview>,
    /// 引用回复，可省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<QuoteRef>,
    /// 发送时间（epoch 毫秒）。
    pub created_at: i64,
    /// 发送状态（`"sending"|"sent"|"delivered"|"read"|"failed"`），仅自己发的消息有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 是否已撤回。
    pub recalled: bool,
    /// 本地已读标记（仅对端发来的入站消息使用）：会话 `mark_read` 时批量
    /// 置位，`delete_message` 据此判断「删的是否未读消息」——避免删一条
    /// 早已读的历史消息时把真正未读的角标误清。自己发的消息恒 false。
    #[serde(default, skip_serializing_if = "is_false")]
    pub read: bool,
}

/// `read` 为 false 时跳过序列化（保持存量记录与信封 JSON 形状不变）。
fn is_false(b: &bool) -> bool {
    !b
}

/// 会话存储键。
pub fn conversation_key(space: &str, conv_id: &str) -> String {
    format!("{CONVERSATION_KEY_PREFIX}{space}:{conv_id}")
}

/// 指定空间的会话键前缀（scan 用）。
pub fn conversation_prefix(space: &str) -> String {
    format!("{CONVERSATION_KEY_PREFIX}{space}:")
}

/// 消息存储键（`createdAt` 13 位零填充，scan 字典序 = 时间序）。
pub fn message_key(space: &str, conv_id: &str, created_at: i64, msg_id: &str) -> String {
    format!("{MESSAGE_KEY_PREFIX}{space}:{conv_id}:{created_at:013}:{msg_id}")
}

/// 指定会话的消息键前缀（scan 用；范围语义走 [`crate::storage::ScanOptions::prefix`] 默认上界）。
pub fn message_prefix(space: &str, conv_id: &str) -> String {
    format!("{MESSAGE_KEY_PREFIX}{space}:{conv_id}:")
}

/// 消息 id 二级索引键（值 = 消息存储键 `msg:item:...`）：入站去重、撤回、
/// 状态回写按 id 定位走索引直取，避免全量扫描会话消息。
pub fn message_id_index_key(space: &str, conv_id: &str, msg_id: &str) -> String {
    format!("{MESSAGE_ID_INDEX_PREFIX}{space}:{conv_id}:{msg_id}")
}

/// 指定会话的消息 id 索引前缀（清空/删除会话时批量清理索引用）。
pub fn message_id_index_prefix(space: &str, conv_id: &str) -> String {
    format!("{MESSAGE_ID_INDEX_PREFIX}{space}:{conv_id}:")
}

/// 进程内 id 序号（避免同毫秒冲突）。
static ID_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成消息 id：`m{now_ms}-{seq}`（同毫秒递增序号防冲突）。
pub fn generate_message_id(now_ms: i64) -> String {
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("m{now_ms}-{seq}")
}

/// 生成会话 id：`c{now_ms}-{seq}`。
pub(crate) fn generate_conversation_id(now_ms: i64) -> String {
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("c{now_ms}-{seq}")
}
