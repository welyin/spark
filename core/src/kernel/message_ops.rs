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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::dm_envelope::{KIND_CHAT, KIND_READ, KIND_RECALL};
use super::{Kernel, KernelError, Result};
use crate::contact::ContactService;
use crate::message::types::ConversationKind;
use crate::message::{
    AppMessageCard, AppMessageRecord, AppMessageService, ConversationRecord, LinkPreview,
    MAX_TEXT_BYTES, MessageError, MessageRecord, MessageService, MessageType, QuoteRef,
    generate_message_id,
};
use crate::p2p::node::system_now_ms;

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

/// 应用消息记录 → 视图（字段一一对应，无 `'me'` 映射——应用消息无发送者概念）。
pub(crate) fn app_message_view(record: &AppMessageRecord) -> AppMessageView {
    AppMessageView {
        id: record.id.clone(),
        plugin_id: record.plugin_id.clone(),
        summary: record.summary.clone(),
        payload: record.payload.clone(),
        card: record.card.clone(),
        created_at: record.created_at,
        status: record.status.clone(),
        read: record.read,
    }
}

/// 会话记录 → 视图（`online_peers` 为当前连接的 libp2p peerId 集合；
/// `my_root_id` 命中的会话是自己的会话，online 恒 true——自己永远在线）。
///
/// `fallback_peer_id`：`conv.peer` 缺失时（`message_ensure_direct` 从通讯录
/// 先建的会话没有寻址信息，peer 要等对方入站消息才回填）的寻址回退——朋友
/// 记录（`ct:friend:{peerRootId}`）里的 peerId，与 `resolve_conv_peer` 的
/// 回退口径对齐；回退也没有就不在线。
pub(crate) fn conversation_view(
    conv: &ConversationRecord,
    online_peers: &HashSet<String>,
    my_root_id: Option<&str>,
    fallback_peer_id: Option<&str>,
) -> ConversationView {
    let peer_id = conv
        .peer
        .as_ref()
        .map(|p| p.peer_id.as_str())
        .or(fallback_peer_id);
    let online = my_root_id.is_some_and(|me| me == conv.peer_root_id)
        || peer_id.is_some_and(|p| online_peers.contains(p));
    ConversationView {
        id: conv.id.clone(),
        kind: conv.kind,
        title: conv.title.clone(),
        peer_id: conv.peer_root_id.clone(),
        unread_count: conv.unread_count,
        pinned_at: conv.pinned_at,
        muted: conv.muted,
        online,
        draft: conv.draft.clone(),
        updated_at: conv.updated_at,
    }
}

/// 消息记录 → 视图（`my_root_id` 命中时做 `'me'` 映射；列表水合与入站
/// 事件两侧都传 `Some`，口径一致——自己设备同步来的消息同样渲染到自己侧）。
pub(crate) fn message_view(record: &MessageRecord, my_root_id: Option<&str>) -> ChatMessageView {
    let is_mine = my_root_id.is_some_and(|me| me == record.sender_id);
    ChatMessageView {
        id: record.id.clone(),
        sender_id: if is_mine {
            "me".to_string()
        } else {
            record.sender_id.clone()
        },
        sender_name: if is_mine {
            "我".to_string()
        } else {
            record.sender_name.clone()
        },
        msg_type: record.msg_type,
        content: record.content.clone(),
        file_size: record.file_size,
        duration: record.duration,
        link: record.link.clone(),
        quote: record.quote.clone(),
        created_at: record.created_at,
        status: record.status.clone(),
        recalled: record.recalled,
    }
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

    // ------------------------------------------------------------------
    // 会话/消息视图
    // ------------------------------------------------------------------

    /// 会话列表（置顶优先，其余按最后消息时间倒序）。
    pub fn message_list_conversations(&self, space: &str) -> Result<Vec<ConversationView>> {
        let mut convs = MessageService::list_conversations(self.require_storage()?, space)?;
        convs.sort_by(|a, b| {
            (b.pinned_at > 0)
                .cmp(&(a.pinned_at > 0))
                .then(b.pinned_at.cmp(&a.pinned_at))
                .then(b.updated_at.cmp(&a.updated_at))
        });
        let online = self.online_peer_ids();
        let my_root_id = self.current_root_id().ok().flatten();
        let friend_peers = self.friend_peer_map();
        Ok(convs
            .iter()
            .map(|c| {
                conversation_view(
                    c,
                    &online,
                    my_root_id.as_deref(),
                    friend_peers.get(&c.peer_root_id).map(String::as_str),
                )
            })
            .collect())
    }

    /// 会话消息列表（时间升序；自己发的消息 `senderId`/`senderName` 映射为
    /// `"me"`/`"我"`）。
    pub fn message_list_messages(&self, space: &str, conv_id: &str) -> Result<Vec<ChatMessageView>> {
        let my_root_id = self.require_current_root_id()?;
        let messages = MessageService::get_messages(self.require_storage()?, space, conv_id)?;
        Ok(messages
            .iter()
            .map(|m| message_view(m, Some(&my_root_id)))
            .collect())
    }

    /// 找到或创建与 `peer_root_id` 的 1:1 会话（幂等；id 为 `dm:{peerRootId}`）。
    pub fn message_ensure_direct(
        &mut self,
        space: &str,
        peer_root_id: &str,
        title: &str,
    ) -> Result<ConversationView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now = system_now_ms();
        let conv = match MessageService::find_direct_conversation(
            self.require_storage()?,
            space,
            peer_root_id,
        )? {
            Some(existing) => existing,
            None => {
                let record = ConversationRecord {
                    id: direct_conversation_id(peer_root_id),
                    kind: ConversationKind::Direct,
                    title: title.to_string(),
                    peer_root_id: peer_root_id.to_string(),
                    peer: None,
                    unread_count: 0,
                    pinned_at: 0,
                    muted: false,
                    draft: String::new(),
                    updated_at: now,
                };
                MessageService::upsert_conversation(self.require_storage_mut()?, space, &record)?;
                record
            }
        };
        let online = self.online_peer_ids();
        let my_root_id = self.current_root_id().ok().flatten();
        let fallback = self.friend_peer_map().get(peer_root_id).cloned();
        Ok(conversation_view(
            &conv,
            &online,
            my_root_id.as_deref(),
            fallback.as_deref(),
        ))
    }

    // ------------------------------------------------------------------
    // 发送/重发/撤回
    // ------------------------------------------------------------------

    /// 发送文本消息：先落库（`sending`），信封构造与对端解析同步完成，
    /// dm_direct 投递 spawn 到 kernel runtime——命令立即返回 `sending` 态
    /// 视图，投递完成后任务内回写状态（delivered/failed）并 emit
    /// `ChatStatus` 事件（前端按事件更新）。对端无地址/p2p 未运行同步判负
    /// （`failed`，不 spawn）。
    ///
    /// `message_id` 由客户端生成（内核不做同 id 去重，重试会产生重复消息）；
    /// `quote` 为可选引用回复；`link` 为发送方本地抓取的链接预览（ui-messages.md
    /// §6，抓取在 src-tauri 壳层），入库前经 [`sanitize_link_preview`] 收敛
    /// （trim + 限长截断，url 为空则整条不落）。正文超过 [`MAX_TEXT_BYTES`]
    /// （16 KiB）拒绝。
    pub fn message_send_text(
        &mut self,
        space: &str,
        conv_id: &str,
        message_id: &str,
        text: &str,
        quote: Option<QuoteRef>,
        link: Option<LinkPreview>,
    ) -> Result<ChatMessageView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        if text.len() > MAX_TEXT_BYTES {
            return Err(KernelError::Internal(format!(
                "消息正文超过长度上限（{MAX_TEXT_BYTES} 字节）"
            )));
        }
        let my_root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let conv = MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            .ok_or(crate::message::MessageError::ConversationNotFound)?;
        let record = MessageRecord {
            id: message_id.to_string(),
            sender_id: my_root_id.clone(),
            sender_name: self.my_nickname(&my_root_id),
            msg_type: MessageType::Text,
            content: text.to_string(),
            file_size: None,
            duration: None,
            link: link.and_then(sanitize_link_preview),
            quote,
            created_at: now,
            status: Some("sending".to_string()),
            recalled: false,
            read: false,
        };
        MessageService::append_message(self.require_storage_mut()?, space, conv_id, &record)?;
        let status = if conv.peer_root_id == my_root_id {
            // 给自己发消息 = 同步到同身份的所有节点：本机副本天然送达
            // （delivered），随后向已配对设备逐个尽力投递 chat 信封
            // （跳过 conv.peer 常规解析；单设备失败不影响状态——离线设备
            // 恢复后的历史同步依赖后续个人空间同步机制）。
            let body = serde_json::json!({
                "spaceKey": space,
                "message": serde_json::to_value(&record)?,
            });
            self.deliver_to_devices(&my_root_id, KIND_CHAT, body);
            "delivered"
        } else {
            match self.prepare_chat_delivery(space, &conv, &record)? {
                Some((peer, envelope)) => {
                    self.spawn_chat_delivery(space, conv_id, message_id, peer, envelope);
                    // 终态由投递任务回写 + ChatStatus 事件通知
                    "sending"
                }
                None => "failed",
            }
        };
        if status != "sending" {
            MessageService::set_message_status(
                self.require_storage_mut()?,
                space,
                conv_id,
                message_id,
                status,
            )?;
        }
        let mut view = message_view(&record, Some(&my_root_id));
        view.status = Some(status.to_string());
        Ok(view)
    }

    /// 重发失败/卡住的消息（`failed` 或 `sending` 可重发——后者是崩溃卡在
    /// 发送中的恢复路径；重跑投递，终态由投递任务回写并 emit `ChatStatus`）。
    /// 已撤回的消息拒绝重发（否则对端已撤回的内容会被「复活」）。
    /// 自己的会话豁免状态门槛：自消息恒 delivered，重发 = 重投向各已配对
    /// 设备投递（已撤回同样拒绝）。
    pub fn message_resend(
        &mut self,
        space: &str,
        conv_id: &str,
        message_id: &str,
    ) -> Result<ChatMessageView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        let conv = MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            .ok_or(crate::message::MessageError::ConversationNotFound)?;
        let record = MessageService::get_messages(self.require_storage()?, space, conv_id)?
            .into_iter()
            .find(|m| m.id == message_id)
            .ok_or_else(|| KernelError::Internal("Message not found".to_string()))?;
        if record.recalled {
            return Err(KernelError::Internal(
                "已撤回的消息不能重发".to_string(),
            ));
        }
        let is_self_conv = conv.peer_root_id == my_root_id;
        // 自己会话豁免状态门槛（自消息恒 delivered，重发 = 重投向各设备投递）
        if !is_self_conv && !matches!(record.status.as_deref(), Some("failed" | "sending")) {
            return Err(KernelError::Internal(
                "仅失败或发送中的消息可以重发".to_string(),
            ));
        }
        MessageService::set_message_status(
            self.require_storage_mut()?,
            space,
            conv_id,
            message_id,
            "sending",
        )?;
        let status = if conv.peer_root_id == my_root_id {
            // 自消息重发：重投向各已配对设备投递（状态仍 delivered，
            // 本机副本天然送达）
            let body = serde_json::json!({
                "spaceKey": space,
                "message": serde_json::to_value(&record)?,
            });
            self.deliver_to_devices(&my_root_id, KIND_CHAT, body);
            "delivered"
        } else {
            match self.prepare_chat_delivery(space, &conv, &record)? {
                Some((peer, envelope)) => {
                    self.spawn_chat_delivery(space, conv_id, message_id, peer, envelope);
                    "sending"
                }
                None => "failed",
            }
        };
        if status != "sending" {
            MessageService::set_message_status(
                self.require_storage_mut()?,
                space,
                conv_id,
                message_id,
                status,
            )?;
        }
        let mut view = message_view(&record, Some(&my_root_id));
        view.status = Some(status.to_string());
        Ok(view)
    }

    /// 撤回消息（发送后 2 分钟内且只能撤回自己发的消息；service 判定窗口）。
    /// 撤回成功且原状态为 `delivered`/`read` 时向对端发 recall 信封（尽力而为）。
    pub fn message_recall(&mut self, space: &str, conv_id: &str, message_id: &str) -> Result<bool> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        let conv = MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            .ok_or(crate::message::MessageError::ConversationNotFound)?;
        let message = MessageService::get_message(self.require_storage()?, space, conv_id, message_id)?;
        if let Some(msg) = &message
            && msg.sender_id != my_root_id
        {
            return Err(KernelError::Internal(
                "只能撤回自己发送的消息".to_string(),
            ));
        }
        let original_status = message.and_then(|m| m.status);
        let now = system_now_ms();
        let recalled =
            MessageService::recall_message(self.require_storage_mut()?, space, conv_id, message_id, now)?;
        if recalled && matches!(original_status.as_deref(), Some("delivered" | "read")) {
            self.notify_peer(
                space,
                &conv,
                KIND_RECALL,
                serde_json::json!({ "spaceKey": space, "messageId": message_id }),
            );
        }
        Ok(recalled)
    }

    // ------------------------------------------------------------------
    // 薄封装（本地状态变更）
    // ------------------------------------------------------------------

    /// 删除单条消息（仅本地）。
    pub fn message_delete(&mut self, space: &str, conv_id: &str, message_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::delete_message(self.require_storage_mut()?, space, conv_id, message_id)?;
        Ok(())
    }

    /// 清零会话未读；direct 会话且对端可达时发 read 信封（尽力而为）。
    /// 自己的会话改为向所有已配对设备发 read 信封（已读状态跨设备同步）。
    pub fn message_mark_read(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::mark_read(self.require_storage_mut()?, space, conv_id)?;
        if let Some(conv) = MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            && conv.kind == ConversationKind::Direct
        {
            let body = serde_json::json!({ "spaceKey": space });
            let my_root_id = self.current_root_id().ok().flatten();
            if my_root_id.as_deref() == Some(conv.peer_root_id.as_str()) {
                self.deliver_to_devices(&conv.peer_root_id, KIND_READ, body);
            } else {
                self.notify_peer(space, &conv, KIND_READ, body);
            }
        }
        Ok(())
    }

    /// 写入会话草稿。
    pub fn message_set_draft(&mut self, space: &str, conv_id: &str, draft: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::set_draft(self.require_storage_mut()?, space, conv_id, draft)?;
        Ok(())
    }

    /// 切换会话置顶。
    pub fn message_toggle_pin(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::toggle_pin(self.require_storage_mut()?, space, conv_id, system_now_ms())?;
        Ok(())
    }

    /// 切换会话免打扰。
    pub fn message_toggle_mute(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::toggle_mute(self.require_storage_mut()?, space, conv_id)?;
        Ok(())
    }

    /// 清空会话聊天记录（保留会话入口）。
    pub fn message_clear(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::clear_messages(self.require_storage_mut()?, space, conv_id)?;
        Ok(())
    }

    /// 删除会话（会话与消息一并删除）。
    pub fn message_delete_conversation(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::delete_conversation(self.require_storage_mut()?, space, conv_id)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 应用消息（服务号模型，p2p-messages.md §20）
    // ------------------------------------------------------------------

    /// 写入应用消息（本地生成、本地消费；无 peer 投递、无 delivered）。
    /// 校验链（按序）：身份解锁 → pluginId 字符集 → payload.summary 非空且
    /// ≤200 字符 → 限流（每 (space, pluginId) 固定窗口 60s 10 条，超限报
    /// [`MessageError::RateLimited`] 并累计拒绝计数）；校验先于限流，
    /// 非法消息不消耗配额。
    /// 内置 system 会话（壳层系统通知写入方，pluginId = "system"）豁免限流：
    /// 限流防的是插件刷会话（§20.5），system 为壳层可信写入方，安装/升级
    /// 等系统通知不应被配额挤掉（豁免不累计拒绝计数）。
    /// 会话由 pluginId 确定性派生（`app:{pluginId}`）并惰性创建——插件只能
    /// 写自己的会话（§20.4 不变量 2）；写入成功未读 +1，状态恒 `local`。
    pub fn message_app_send(
        &mut self,
        space: &str,
        plugin_id: &str,
        payload: serde_json::Value,
        card: Option<AppMessageCard>,
    ) -> Result<AppMessageView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let record = AppMessageService::build_app_message(
            plugin_id,
            payload,
            card,
            generate_message_id(now),
            now,
        )?;
        if plugin_id != "system" && !self.app_msg_limiter.check(space, plugin_id, now) {
            return Err(MessageError::RateLimited.into());
        }
        AppMessageService::ensure_app_conversation(self.require_storage_mut()?, space, plugin_id, now)?;
        AppMessageService::append_app_message(self.require_storage_mut()?, space, &record)?;
        Ok(app_message_view(&record))
    }

    /// 应用会话消息列表（时间升序）。
    pub fn message_app_list(&self, space: &str, plugin_id: &str) -> Result<Vec<AppMessageView>> {
        let messages = AppMessageService::list_app_messages(self.require_storage()?, space, plugin_id)?;
        Ok(messages.iter().map(app_message_view).collect())
    }

    /// 清零应用会话未读并把会话内未读消息批量置已读（语义与人际会话一致）。
    pub fn message_app_mark_read(&mut self, space: &str, plugin_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        AppMessageService::mark_app_read(self.require_storage_mut()?, space, plugin_id)?;
        Ok(())
    }

    /// 删除应用会话（会话与全部应用消息一并删除）。
    pub fn message_app_delete_conversation(&mut self, space: &str, plugin_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        AppMessageService::delete_app_conversation(self.require_storage_mut()?, space, plugin_id)?;
        Ok(())
    }

    /// 指定应用会话的限流累计拒绝数（熔断观测面；内存态，重启清零）。
    pub fn message_app_rate_rejected(&self, space: &str, plugin_id: &str) -> u64 {
        self.app_msg_limiter.rejected_count(space, plugin_id)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerRef;

    fn conv(peer: Option<PeerRef>) -> ConversationRecord {
        ConversationRecord {
            id: "dm:peer-root".to_string(),
            kind: ConversationKind::Direct,
            title: "对方".to_string(),
            peer_root_id: "peer-root".to_string(),
            peer,
            ..Default::default()
        }
    }

    fn online(peers: &[&str]) -> HashSet<String> {
        peers.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn online_uses_conv_peer_first() {
        let set = online(&["peer-conv"]);
        assert!(conversation_view(
            &conv(Some(PeerRef {
                peer_id: "peer-conv".to_string(),
                addresses: Vec::new(),
            })),
            &set,
            None,
            None,
        )
        .online);
    }

    #[test]
    fn online_falls_back_to_friend_peer_when_conv_peer_missing() {
        let set = online(&["peer-friend"]);
        let c = conv(None);
        // 朋友记录也没有寻址信息 → 不在线
        assert!(!conversation_view(&c, &set, None, None).online);
        // conv.peer 缺失时回退朋友记录的 peerId → 在线
        assert!(conversation_view(&c, &set, None, Some("peer-friend")).online);
    }

    #[test]
    fn conv_peer_takes_precedence_over_fallback() {
        // conv.peer 存在但与回退不同源：以 conv.peer 为准（它才是会话寻址）
        let set = online(&["peer-friend"]);
        let c = conv(Some(PeerRef {
            peer_id: "peer-conv".to_string(),
            addresses: Vec::new(),
        }));
        assert!(!conversation_view(&c, &set, None, Some("peer-friend")).online);
    }

    #[test]
    fn self_conversation_always_online() {
        let set = online(&[]);
        let mut c = conv(None);
        c.peer_root_id = "me".to_string();
        assert!(conversation_view(&c, &set, Some("me"), None).online);
    }
}
