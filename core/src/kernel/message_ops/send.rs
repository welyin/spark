//! 发送/重发/撤回与 bot 回复：出站消息落库 + dm 信封投递编排（投递机器
//! 在 [`super::super::dm_delivery`]），以及插件侧 bot 回复的共享实现
//! （[`Kernel::message_bot_reply`] 门面与插件后台运行的 `message.reply`
//! 能力共用）。

use std::collections::HashSet;

use super::super::KernelError;
use super::super::dm_envelope::{KIND_CHAT, KIND_RECALL};
use super::{
    ChatMessageView, Kernel, Result, conversation_view, message_view, sanitize_link_preview,
};
use crate::contact::{ContactService, DmChannel, DmRecipientSkipReason};
use crate::message::{
    ConversationKind, LinkPreview, MAX_TEXT_BYTES, MessageError, MessageRecord, MessageService,
    MessageType, QuoteRef, generate_message_id,
};
use crate::p2p::P2pEvent;
use crate::p2p::node::system_now_ms;
use crate::plugin::{PluginError, PluginHostShared};
use crate::storage::StorageBackend;

/// Bot 回复共享实现：[`Kernel::message_bot_reply`] 门面与插件后台运行时的
/// `message.reply` 能力共用——落库 → ChatReceived 事件 → 回同步自设备。
///
/// 与门面版的语义差异仅一处：回同步需要当前身份（rootId 自签信封），共享
/// 实现从共享格读取、缺失时跳过回同步（尽力而为，与 `deliver_to_devices`
/// 「未启动 p2p 静默」口径一致）；门面调用点必然已解锁，行为不变。
pub(crate) fn bot_reply_shared(
    host: &PluginHostShared,
    space: &str,
    conv_id: &str,
    bot_root_id: &str,
    bot_name: &str,
    message_id: &str,
    text: &str,
) -> crate::plugin::Result<ChatMessageView> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    if text.len() > MAX_TEXT_BYTES {
        return Err(PluginError::InvalidInput(format!(
            "消息正文超过长度上限（{MAX_TEXT_BYTES} 字节）"
        )));
    }
    let mut storage = host.require_storage()?;
    let conv = MessageService::get_conversation(&storage, space, conv_id)?
        .ok_or(crate::message::MessageError::ConversationNotFound)?;
    let now = system_now_ms();
    let node_id = host.sync_node_id();
    let record = MessageRecord {
        id: message_id.to_string(),
        sender_id: bot_root_id.to_string(),
        sender_name: bot_name.to_string(),
        msg_type: MessageType::Text,
        content: text.to_string(),
        file_size: None,
        duration: None,
        link: None,
        quote: None,
        created_at: now,
        status: Some("delivered".to_string()),
        recalled: false,
        read: false,
    };
    MessageService::append_message_pdsync(
        &mut storage,
        space,
        conv_id,
        &record,
        now,
        Some(&node_id),
    )?;
    let conv_view = conversation_view(&conv, &HashSet::new(), None, None);
    let msg_view = message_view(&record, Some("__bot_sender__"));
    if let (Ok(conversation), Ok(message)) = (
        serde_json::to_value(&conv_view),
        serde_json::to_value(&msg_view),
    ) {
        let _ = host
            .event_tx
            .send(P2pEvent::ChatReceived(serde_json::json!({
                "spaceKey": space,
                "conversation": conversation,
                "message": message
            })));
    }
    if let Some(my_root_id) = host
        .my_root_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        host.deliver_to_devices(
            &my_root_id,
            crate::kernel::dm_envelope::KIND_CHAT,
            serde_json::json!({
                "spaceKey": space,
                "convId": conv_id,
                "message": msg_view
            }),
        );
    }
    Ok(msg_view)
}

/// 归属校验：目标会话必须是本插件的 bot 会话，返回 (bot_root_id, bot_name)。
/// reply 与流式回复三能力共用（流式 chunk/end 按消息 id 定位，仍需先验归属）。
pub(crate) fn require_owned_bot_conv(
    host: &PluginHostShared,
    plugin_id: &str,
    space: &str,
    conv_id: &str,
) -> crate::plugin::Result<(String, String)> {
    let storage = host.require_storage()?;
    let conv = MessageService::get_conversation(&storage, space, conv_id)?
        .ok_or(MessageError::ConversationNotFound)?;
    let prefix = format!("bot:{plugin_id}:");
    if !conv.peer_root_id.starts_with(&prefix) {
        return Err(PluginError::ConversationNotOwned(conv_id.to_string()));
    }
    let bot_root_id = conv.peer_root_id.clone();
    let bot_name = ContactService::get_friend(&storage, &bot_root_id)?
        .map(|friend| friend.nickname)
        .filter(|nickname| !nickname.is_empty())
        .unwrap_or_else(|| bot_root_id.clone());
    Ok((bot_root_id, bot_name))
}

/// 流式回复·开始：落一条 `status="streaming"` 的占位消息并广播，返回消息 id。
/// 逐 chunk 由 [`bot_reply_stream_chunk_shared`] 追加，终态由
/// [`bot_reply_stream_end_shared`] 收尾。占位消息与 bot_reply_shared 同构，
/// 仅 status 不同（前端以 streaming 态渲染光标/逐字追加）。
pub(crate) fn bot_reply_stream_start_shared(
    host: &PluginHostShared,
    space: &str,
    conv_id: &str,
    bot_root_id: &str,
    bot_name: &str,
) -> crate::plugin::Result<String> {
    let message_id = generate_message_id(system_now_ms());
    let _ = bot_reply_stream_upsert(
        host,
        space,
        conv_id,
        bot_root_id,
        bot_name,
        &message_id,
        "",
        "streaming",
    )?;
    Ok(message_id)
}

/// 流式回复·追加：把 chunk 文本追加到占位消息 content 并重发 ChatReceived
/// （前端按消息 id 更新同一条，逐字上屏）。
pub(crate) fn bot_reply_stream_chunk_shared(
    host: &PluginHostShared,
    space: &str,
    conv_id: &str,
    bot_root_id: &str,
    bot_name: &str,
    message_id: &str,
    chunk_text: &str,
) -> crate::plugin::Result<()> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    let storage = host.require_storage()?;
    let existing = MessageService::get_message(&storage, space, conv_id, message_id)?.ok_or(
        PluginError::InvalidCall(format!("stream message not found: {message_id}")),
    )?;
    let mut content = existing.content;
    content.push_str(chunk_text);
    if content.len() > MAX_TEXT_BYTES {
        return Err(PluginError::InvalidInput(format!(
            "流式消息累计超过长度上限（{MAX_TEXT_BYTES} 字节）"
        )));
    }
    drop(storage);
    drop(_io);
    let _ = bot_reply_stream_upsert(
        host,
        space,
        conv_id,
        bot_root_id,
        bot_name,
        message_id,
        &content,
        "streaming",
    )?;
    Ok(())
}

/// 流式回复·终态：status 收尾（delivered / 有 error 时 failed），内容已定稿。
pub(crate) fn bot_reply_stream_end_shared(
    host: &PluginHostShared,
    space: &str,
    conv_id: &str,
    bot_root_id: &str,
    bot_name: &str,
    message_id: &str,
    error: Option<&str>,
) -> crate::plugin::Result<()> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    let storage = host.require_storage()?;
    let existing = MessageService::get_message(&storage, space, conv_id, message_id)?.ok_or(
        PluginError::InvalidCall(format!("stream message not found: {message_id}")),
    )?;
    let content = if existing.content.is_empty() {
        error.unwrap_or("（无响应）").to_string()
    } else {
        existing.content.clone()
    };
    let status = if error.is_some() {
        "failed"
    } else {
        "delivered"
    };
    drop(storage);
    drop(_io);
    let _ = bot_reply_stream_upsert(
        host,
        space,
        conv_id,
        bot_root_id,
        bot_name,
        message_id,
        &content,
        status,
    )?;
    Ok(())
}

/// 流式回复的写库+广播共用：占位/追加/终态都是「按 id 覆盖同一条消息记录
/// 再发 ChatReceived」。与 bot_reply_shared 的差异：不在此回同步自设备
/// （流式中间态无需扩散；终态完成后由调用方按普通 reply 口径补一次回同步
/// ——见 host_env 的 reply_stream_end）。
#[allow(clippy::too_many_arguments)]
fn bot_reply_stream_upsert(
    host: &PluginHostShared,
    space: &str,
    conv_id: &str,
    bot_root_id: &str,
    bot_name: &str,
    message_id: &str,
    content: &str,
    status: &str,
) -> crate::plugin::Result<ChatMessageView> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut storage = host.require_storage()?;
    let now = system_now_ms();
    // 覆盖写：取既有记录的 created_at（保持消息键不变——消息键含 created_at，
    // 用新时间会写到不同键，产生重复消息），无则按当前时间（start 场景）
    let created_at = MessageService::get_message(&storage, space, conv_id, message_id)?
        .map(|m| m.created_at)
        .unwrap_or(now);
    let record = MessageRecord {
        id: message_id.to_string(),
        sender_id: bot_root_id.to_string(),
        sender_name: bot_name.to_string(),
        msg_type: MessageType::Text,
        content: content.to_string(),
        file_size: None,
        duration: None,
        link: None,
        quote: None,
        created_at,
        status: Some(status.to_string()),
        recalled: false,
        read: false,
    };
    MessageService::append_message_pdsync(
        &mut storage,
        space,
        conv_id,
        &record,
        now,
        Some(&host.sync_node_id()),
    )?;
    let conv = MessageService::get_conversation(&storage, space, conv_id)?
        .ok_or(MessageError::ConversationNotFound)?;
    let conv_view = conversation_view(&conv, &HashSet::new(), None, None);
    let msg_view = message_view(&record, Some("__bot_sender__"));
    if let (Ok(conversation), Ok(message)) = (
        serde_json::to_value(&conv_view),
        serde_json::to_value(&msg_view),
    ) {
        let _ = host
            .event_tx
            .send(P2pEvent::ChatReceived(serde_json::json!({
                "spaceKey": space,
                "conversation": conversation,
                "message": message
            })));
    }
    Ok(msg_view)
}

impl Kernel {
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
        let node_id = self.sync_node_id();
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
        MessageService::append_message_pdsync(
            self.require_storage_raw_mut()?,
            space,
            conv_id,
            &record,
            now,
            Some(&node_id),
        )?;
        let status = if conv.peer_root_id == my_root_id {
            // 给自己发消息 = 同步到同身份的所有节点：本机副本天然送达
            // （delivered），随后向已配对设备逐个尽力投递 chat 信封
            // （跳过 conv.peer 常规解析；单设备失败不影响状态——离线设备
            // 恢复后的历史同步依赖后续个人空间同步机制）。
            // note: body 必须包含 convId，用于回同步侧 is_self_echo 分支
            // 按目标会话落库而非兜底走到 ensure_inbound_conversation 推导
            let body = serde_json::json!({
                "spaceKey": space,
                "convId": conv_id,
                "message": serde_json::to_value(&record)?,
            });
            self.deliver_to_devices(&my_root_id, KIND_CHAT, body);
            "delivered"
        } else if conv.peer_root_id.starts_with("bot:") {
            // Bot 会话：无 P2P 对端，消息由本机插件处理，直接标记 delivered
            "delivered"
        } else {
            // S5 出站拉黑（social-feed §5）：仅个人空间 direct 人际会话检查
            // （自消息/bot/应用会话与组织空间不经此分支——拉黑是个人空间语义）。
            // 命中拉黑**不投递**，消息落库 failed，错误文案「你已拉黑对方」透传
            // 到壳层供 UI 提示。
            let blocked = space == "personal"
                && conv.kind == ConversationKind::Direct
                && Self::chat_recipient_blocked(self.require_storage()?, &conv.peer_root_id);
            if blocked {
                MessageService::set_message_status(
                    self.require_storage_raw_mut()?,
                    space,
                    conv_id,
                    message_id,
                    "failed",
                )?;
                return Err(KernelError::Internal(
                    "你已拉黑对方，无法发送消息".to_string(),
                ));
            }
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
                self.require_storage_raw_mut()?,
                space,
                conv_id,
                message_id,
                status,
            )?;
        }
        // 自设备回同步（多设备 echo）：除「给自己发消息」外的所有会话（真人 + bot），
        // 发送后回同步到本账号其他自设备——其他设备按 body.convId 落库、标记 sender=
        // 我、不计未读。bot 会话靠这条让宿主设备的插件经 ChatReceived 收到并处理；
        // 真人会话靠这条让其他设备看到我发出的消息。self 分支已内含回同步，不重复。
        if conv.peer_root_id != my_root_id {
            let mut echo = message_view(&record, Some(&my_root_id));
            echo.status = Some(status.to_string());
            self.deliver_to_devices(
                &my_root_id,
                KIND_CHAT,
                serde_json::json!({
                    "spaceKey": space,
                    "convId": conv_id,
                    "message": echo
                }),
            );
        }
        // 本机发往 bot 会话的消息显式 dispatch 给归属插件的后台运行时：本路径不发
        // ChatReceived 广播（前端以返回值刷新），路由任务收不到；多设备 echo 路径
        // 由 plugin router 订阅广播覆盖（kernel/plugin_ops.rs）。事件载荷与
        // ChatReceived 同构，会话取 append 后的最新快照。
        if conv.peer_root_id.starts_with("bot:") {
            if let Some(latest) =
                MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            {
                self.plugin_registry.dispatch_chat(&serde_json::json!({
                    "spaceKey": space,
                    "conversation": serde_json::to_value(conversation_view(
                        &latest,
                        &HashSet::new(),
                        Some(&my_root_id),
                        None,
                    ))?,
                    "message": serde_json::to_value(message_view(&record, Some(&my_root_id)))?,
                }));
            }
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
            return Err(KernelError::Internal("已撤回的消息不能重发".to_string()));
        }
        let is_self_conv = conv.peer_root_id == my_root_id;
        // 自己会话豁免状态门槛（自消息恒 delivered，重发 = 重投向各设备投递）
        if !is_self_conv && !matches!(record.status.as_deref(), Some("failed" | "sending")) {
            return Err(KernelError::Internal(
                "仅失败或发送中的消息可以重发".to_string(),
            ));
        }
        MessageService::set_message_status(
            self.require_storage_raw_mut()?,
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
            // S5 出站拉黑：重发到已拉黑对端同样不投递（个人空间语义，组织会话
            // 跳过），置 failed + 透传文案
            let blocked = space == "personal"
                && conv.kind == ConversationKind::Direct
                && Self::chat_recipient_blocked(self.require_storage()?, &conv.peer_root_id);
            if blocked {
                MessageService::set_message_status(
                    self.require_storage_raw_mut()?,
                    space,
                    conv_id,
                    message_id,
                    "failed",
                )?;
                return Err(KernelError::Internal(
                    "你已拉黑对方，无法发送消息".to_string(),
                ));
            }
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
                self.require_storage_raw_mut()?,
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

    /// Bot 回复：由插件在收到 bot 消息后调用，向 bot 会话插入一条 bot 回复消息。
    /// `bot_root_id` 为 bot 的 rootId（如 `bot:ai-chat:codebuddy-bot`），
    /// 也是消息的 sender_id（对端视角）；`bot_name` 为发送者显示名。
    /// 消息落库后立即返回视图（无 P2P 投递）。
    pub fn message_bot_reply(
        &mut self,
        space: &str,
        conv_id: &str,
        bot_root_id: &str,
        bot_name: &str,
        message_id: &str,
        text: &str,
    ) -> Result<ChatMessageView> {
        Ok(bot_reply_shared(
            &self.plugin_host,
            space,
            conv_id,
            bot_root_id,
            bot_name,
            message_id,
            text,
        )?)
    }

    /// 撤回消息（发送后 2 分钟内且只能撤回自己发的消息；service 判定窗口）。
    /// 撤回成功且原状态为 `delivered`/`read` 时向对端发 recall 信封（尽力而为）。
    pub fn message_recall(&mut self, space: &str, conv_id: &str, message_id: &str) -> Result<bool> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        let conv = MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            .ok_or(crate::message::MessageError::ConversationNotFound)?;
        let message =
            MessageService::get_message(self.require_storage()?, space, conv_id, message_id)?;
        if let Some(msg) = &message
            && msg.sender_id != my_root_id
        {
            return Err(KernelError::Internal("只能撤回自己发送的消息".to_string()));
        }
        let original_status = message.and_then(|m| m.status);
        let now = system_now_ms();
        let recalled = MessageService::recall_message(
            self.require_storage_raw_mut()?,
            space,
            conv_id,
            message_id,
            now,
        )?;
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

    /// S5 出站拉黑判定（social-feed §5）：目标 peerRootId 是否在本机拉黑集合
    /// 命中。走 `ContactService::filter_dm_recipients` 的 chat 通道（只查拉黑，
    /// 不查仅聊天/非朋友）——命中拉黑即 `Blocked`。存储读取失败按未拉黑处理
    /// （不阻塞发送，错误由下游投递路径暴露）。
    fn chat_recipient_blocked<S: StorageBackend>(storage: &S, peer_root_id: &str) -> bool {
        let recipients = vec![peer_root_id.to_string()];
        ContactService::filter_dm_recipients(storage, &recipients, DmChannel::Chat)
            .map(|filter| {
                filter
                    .skipped
                    .iter()
                    .any(|s| s.reason == DmRecipientSkipReason::Blocked)
            })
            .unwrap_or(false)
    }
}
