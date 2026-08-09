//! 消息门面（`Kernel` 的消息 API）：会话/消息视图查询与 dm 出站投递编排。
//!
//! - 视图层完成前端契约的 `'me'` 映射：自己发的消息 `senderId = "me"`、
//!   `senderName = "我"`（存储层一律真实 rootId，见 message 模块文档）；
//! - direct 会话 id 约定为 `dm:{peerRootId}`（确定性，前端依赖此约定）；
//! - 出站消息构造 dm 信封（[`super::dm_envelope`]）经 p2p 直连投递：同步解析
//!   会话/构造信封后 spawn 异步投递（[`super::dm_delivery`]），命令立即返回
//!   `sending` 视图，终态（`delivered`/`failed`）经 `P2pEvent::ChatStatus`
//!   事件回写（可经 [`Kernel::message_resend`] 重发）；
//! - 查询类方法同步执行，p2p 调用以 `Handle::block_on` 驱动（线程模型见 kernel/mod.rs）。
//!
//! 代码组织：本文件为入口——视图类型、会话 id 约定、链接预览守卫与共享
//! 内部辅助；记录→视图映射与会话查询/建立在 [`views`]，发送/重发/撤回与
//! bot 回复在 [`send`]，会话/消息本地状态变更在 [`conv_ops`]，应用消息
//! （服务号模型，p2p-messages.md §20）在 [`app_ops`]。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{Kernel, Result};
use crate::contact::ContactService;
use crate::message::types::ConversationKind;
use crate::message::{AppMessageCard, LinkPreview, MessageType, QuoteRef};

mod app_ops;
mod conv_ops;
mod send;
mod views;

pub(crate) use send::bot_reply_shared;
pub(crate) use views::{app_message_view, conversation_view, message_view};

/// direct 会话 id 前缀（`dm:{peerRootId}`）。
pub const DIRECT_CONV_PREFIX: &str = "dm:";

/// 应用会话 id（`app:{pluginId}`，确定性；p2p-messages.md §20.1）。
pub use crate::message::app_conversation_id;

/// 链接预览各字段入库上限（字符数，trim 后超限截断而非报错，ui-messages.md §6）。
/// 仅 `sanitize_link_preview` 内部使用（测试断言字面量），不导出。
const LINK_URL_MAX_CHARS: usize = 2048;
/// 标题上限。
const LINK_TITLE_MAX_CHARS: usize = 256;
/// 描述上限。
const LINK_DESCRIPTION_MAX_CHARS: usize = 512;
/// 来源 APP 名上限。
const LINK_SITE_NAME_MAX_CHARS: usize = 64;
/// 域名上限（DNS 标签全长上限 253）。
const LINK_DOMAIN_MAX_CHARS: usize = 253;

/// 按字符数截断（`chars` 计数，避免按字节截断出半个 UTF-8 序列）。
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 链接预览入库守卫：五字段各自 trim 后限长截断（超限截断而非报错）；
/// url 为空或非 http(s) scheme 则整条不落（`None`——`javascript:`/`data:` 等
/// scheme 在卡片点击/渲染面是注入向量，对端自报的 link 字段同样过此守卫）。
/// 抓取在 src-tauri 壳层完成，内核只认入参形状、不信任其内容，故入库前
/// 统一收敛。
pub fn sanitize_link_preview(link: LinkPreview) -> Option<LinkPreview> {
    let url = truncate_chars(link.url.trim(), LINK_URL_MAX_CHARS);
    let lower = url.to_ascii_lowercase();
    if url.is_empty() || !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    Some(LinkPreview {
        url,
        title: truncate_chars(link.title.trim(), LINK_TITLE_MAX_CHARS),
        description: truncate_chars(link.description.trim(), LINK_DESCRIPTION_MAX_CHARS),
        site_name: truncate_chars(link.site_name.trim(), LINK_SITE_NAME_MAX_CHARS),
        domain: truncate_chars(link.domain.trim(), LINK_DOMAIN_MAX_CHARS),
    })
}

/// direct 会话 id（前端契约：确定性 id）。
pub fn direct_conversation_id(peer_root_id: &str) -> String {
    format!("{DIRECT_CONV_PREFIX}{peer_root_id}")
}

/// 会话视图（serde camelCase，Tauri 命令直接返回）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    /// 对方 rootId（前端契约字段名 peerId；= 存储层 peerRootId，
    /// 与 libp2p peerId 无关）。
    pub peer_id: String,
    pub unread_count: u32,
    pub pinned_at: i64,
    pub muted: bool,
    /// 对方 peerId 当前是否在线（p2p 未启动时恒 false）。
    pub online: bool,
    pub draft: String,
    pub updated_at: i64,
}

/// 消息视图（serde camelCase；`type` 为前端契约字段名）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageView {
    pub id: String,
    /// 发送者：自己发的消息映射为 `"me"`，否则为真实 rootId。
    pub sender_id: String,
    /// 自己发的消息固定为 `"我"`（前端契约）。
    pub sender_name: String,
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkPreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<QuoteRef>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub recalled: bool,
}

/// 应用消息视图（serde camelCase；= §20.2 记录线形原样，Tauri 命令直接返回）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppMessageView {
    pub id: String,
    pub plugin_id: String,
    /// 纯文本摘要（trim 后的 payload.summary；未装插件时壳层原生渲染此字段）。
    pub summary: String,
    /// 插件自描述 JSON（含 summary 字段）。
    pub payload: serde_json::Value,
    /// 可选卡片（message-card 富渲染视图）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<AppMessageCard>,
    pub created_at: i64,
    /// 本地状态集：恒 `"local"`（§20.3，无 delivered 语义）。
    pub status: String,
    pub read: bool,
}

impl Kernel {
    // ------------------------------------------------------------------
    // 共享内部辅助（contact_ops 复用投递/信封）
    // ------------------------------------------------------------------

    /// 当前已解锁身份的显示昵称（拿不到用 rootId 前 8 位）。
    pub(crate) fn my_nickname(&self, root_id: &str) -> String {
        self.read_identity_file(root_id)
            .ok()
            .flatten()
            .and_then(|f| f.nickname)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| root_id.chars().take(8).collect())
    }

    /// 当前已解锁身份的头像（data URL；无头像/空串归一为 None）。
    pub(crate) fn my_avatar(&self, root_id: &str) -> Option<String> {
        self.read_identity_file(root_id)
            .ok()
            .flatten()
            .and_then(|f| f.avatar)
            .filter(|a| !a.trim().is_empty())
    }

    /// rootId → libp2p peerId 映射（朋友记录的寻址回退，`conv.peer` 缺失时
    /// 的 online 判定依据，与 `resolve_conv_peer` 的朋友回退口径对齐）。
    pub(crate) fn friend_peer_map(&self) -> HashMap<String, String> {
        let Ok(storage) = self.require_storage() else {
            return HashMap::new();
        };
        ContactService::overview(storage, "personal")
            .map(|view| view.friends)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|f| {
                f.peer
                    .and_then(|p| (!p.peer_id.is_empty()).then_some((f.root_id, p.peer_id)))
            })
            .collect()
    }

    /// 当前在线的 libp2p peerId 集合（p2p 未启动为空集）。
    pub(crate) fn online_peer_ids(&self) -> HashSet<String> {
        self.p2p_status()
            .ok()
            .flatten()
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default()
    }
}
