//! 发送/重发/撤回与 bot 回复：出站消息落库 + dm 信封投递编排（投递机器
//! 在 [`super::super::dm_delivery`]），以及插件侧 bot 回复的共享实现
//! （[`Kernel::message_bot_reply`] 门面与插件后台运行的 `message.reply`
//! 能力共用）。

use std::collections::HashSet;

use super::{
    ChatMessageView, Kernel, Result, conversation_view, message_view, sanitize_link_preview,
};
use super::super::KernelError;
use super::super::dm_envelope::{KIND_CHAT, KIND_RECALL};
use crate::message::{
    LinkPreview, MAX_TEXT_BYTES, MessageRecord, MessageService, MessageType, QuoteRef,
};
use crate::p2p::P2pEvent;
use crate::p2p::node::system_now_ms;
use crate::plugin::{PluginError, PluginHostShared};

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
    MessageService::append_message_pdsync(&mut storage, space, conv_id, &record, now, Some(&node_id))?;
    let conv_view = conversation_view(&conv, &HashSet::new(), None, None);
    let msg_view = message_view(&record, Some("__bot_sender__"));
    if let (Ok(conversation), Ok(message)) = (
        serde_json::to_value(&conv_view),
        serde_json::to_value(&msg_view),
    ) {
        let _ = host.event_tx.send(P2pEvent::ChatReceived(serde_json::json!({
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
            MessageService::recall_message(self.require_storage_raw_mut()?, space, conv_id, message_id, now)?;
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
}
