//! 消息命令：conversations/messages/send-text/mark-read/recall/resend。

use serde_json::{Value, json};
use spark_core::kernel::Kernel;
use spark_core::message::LinkPreview;

use crate::dispatch::{Params, to_json};

/// `conversations`：会话列表（space 缺省 personal）。
pub fn conversations(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    to_json(kernel.message_list_conversations(params.str_or("space", "personal")))
}

/// `messages`：会话消息列表（convId 必填，direct 约定 `dm:{peerRootId}`）。
pub fn messages(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    let conv_id = params.need_str("convId")?;
    to_json(kernel.message_list_messages(params.str_or("space", "personal"), conv_id))
}

/// `send-text`：ensure direct（幂等）→ 发送文本。link 可选
/// `{url,title,description,siteName,domain}`——截断/非法 url 丢弃由内核
/// `sanitize_link_preview` 收敛；返回消息视图（status 为投递态）。
pub fn send_text(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let space = params.str_or("spaceKey", "personal");
    let peer_root_id = params.need_str("peerRootId")?;
    let text = params.need_str("text")?;
    let conv = kernel
        .message_ensure_direct(space, peer_root_id, params.str_or("title", peer_root_id))
        .map_err(|e| e.to_string())?;
    let link: Option<LinkPreview> = match params.opt_value("link") {
        Some(value) => Some(serde_json::from_value(value.clone()).map_err(|e| e.to_string())?),
        None => None,
    };
    let message_id = params
        .opt_str("messageId")
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("msg-{}", spark_core::p2p::node::system_now_ms()));
    let view = kernel
        .message_send_text(space, &conv.id, &message_id, text, None, link)
        .map_err(|e| e.to_string())?;
    let mut value = serde_json::to_value(&view).map_err(|e| e.to_string())?;
    value["convId"] = json!(conv.id);
    Ok(value)
}

/// `mark-read`：清零未读并尽力发 read 信封。
pub fn mark_read(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let conv_id = params.need_str("convId")?;
    kernel
        .message_mark_read(params.str_or("space", "personal"), conv_id)
        .map_err(|e| e.to_string())?;
    Ok(json!({"read": true}))
}

/// `recall`：2 分钟窗口内撤回；返回 `{recalled: bool}`（窗口外/不存在为 false）。
pub fn recall(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let conv_id = params.need_str("convId")?;
    let message_id = params.need_str("messageId")?;
    let recalled = kernel
        .message_recall(params.str_or("space", "personal"), conv_id, message_id)
        .map_err(|e| e.to_string())?;
    Ok(json!({"recalled": recalled}))
}

/// `resend`：重发 failed 消息，返回消息视图。
pub fn resend(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let conv_id = params.need_str("convId")?;
    let message_id = params.need_str("messageId")?;
    to_json(kernel.message_resend(params.str_or("space", "personal"), conv_id, message_id))
}
