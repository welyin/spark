//! 消息命令。
//!
//! 会话/消息视图直通内核 `message_*` 门面：视图层已完成 `'me'` 映射
//! （自己发的消息 `senderId = "me"`、`senderName = "我"`）；direct 会话 id
//! 约定为 `dm:{peerRootId}`。出站消息 p2p 未启动/投递失败落库为 `failed`，
//! 可经 `message_resend` 重发。

use spark_core::kernel::{ChatMessageView, ConversationView, Kernel};
use spark_core::message::QuoteRef;

use super::dto::SuccessResult;
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn list_conversations_inner(
    kernel: &Kernel,
    space: &str,
) -> Result<Vec<ConversationView>, String> {
    kernel.message_list_conversations(space).map_err(err)
}

pub(crate) fn list_messages_inner(
    kernel: &Kernel,
    space: &str,
    conv_id: &str,
) -> Result<Vec<ChatMessageView>, String> {
    kernel.message_list_messages(space, conv_id).map_err(err)
}

pub(crate) fn ensure_direct_inner(
    kernel: &mut Kernel,
    space: &str,
    peer_id: &str,
    title: &str,
) -> Result<ConversationView, String> {
    kernel
        .message_ensure_direct(space, peer_id, title)
        .map_err(err)
}

pub(crate) fn send_text_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
    message_id: &str,
    text: &str,
    quote: Option<QuoteRef>,
) -> Result<ChatMessageView, String> {
    kernel
        .message_send_text(space, conv_id, message_id, text, quote)
        .map_err(err)
}

pub(crate) fn resend_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
    message_id: &str,
) -> Result<ChatMessageView, String> {
    kernel
        .message_resend(space, conv_id, message_id)
        .map_err(err)
}

/// 内核语义：消息不存在 / 已撤回 / 超 2 分钟窗口均返回 `Ok(false)`，
/// 透传为 `{ success: false }`（不报错）。
pub(crate) fn recall_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
    message_id: &str,
) -> Result<SuccessResult, String> {
    let recalled = kernel
        .message_recall(space, conv_id, message_id)
        .map_err(err)?;
    Ok(SuccessResult {
        success: recalled,
    })
}

pub(crate) fn delete_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
    message_id: &str,
) -> Result<SuccessResult, String> {
    kernel
        .message_delete(space, conv_id, message_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn mark_read_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
) -> Result<SuccessResult, String> {
    kernel.message_mark_read(space, conv_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn set_draft_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
    draft: &str,
) -> Result<SuccessResult, String> {
    kernel
        .message_set_draft(space, conv_id, draft)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn toggle_pin_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
) -> Result<SuccessResult, String> {
    kernel.message_toggle_pin(space, conv_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn toggle_mute_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
) -> Result<SuccessResult, String> {
    kernel.message_toggle_mute(space, conv_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn clear_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
) -> Result<SuccessResult, String> {
    kernel.message_clear(space, conv_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn delete_conversation_inner(
    kernel: &mut Kernel,
    space: &str,
    conv_id: &str,
) -> Result<SuccessResult, String> {
    kernel
        .message_delete_conversation(space, conv_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

/// 会话列表（置顶优先，其余按最后消息时间倒序）。
#[tauri::command]
pub fn message_list_conversations(
    state: tauri::State<'_, KernelState>,
    space_key: String,
) -> Result<Vec<ConversationView>, String> {
    list_conversations_inner(&*lock_kernel(&state)?, &space_key)
}

/// 会话消息列表（时间升序；自己发的消息 senderId/senderName 映射为 me/我）。
#[tauri::command]
pub fn message_list_messages(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<Vec<ChatMessageView>, String> {
    list_messages_inner(&*lock_kernel(&state)?, &space_key, &conv_id)
}

/// 找到或创建与 `peer_id` 的 1:1 会话（幂等；id 为 `dm:{peerId}`）。
#[tauri::command]
pub fn message_ensure_direct(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    peer_id: String,
    title: String,
) -> Result<ConversationView, String> {
    ensure_direct_inner(&mut *lock_kernel(&state)?, &space_key, &peer_id, &title)
}

/// 发送文本消息（`message_id` 客户端生成作幂等键；`quote` 可选引用回复）。
#[tauri::command]
pub fn message_send_text(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
    message_id: String,
    text: String,
    quote: Option<QuoteRef>,
) -> Result<ChatMessageView, String> {
    send_text_inner(
        &mut *lock_kernel(&state)?,
        &space_key,
        &conv_id,
        &message_id,
        &text,
        quote,
    )
}

/// 重发失败的消息（仅 `failed` 状态可重发）。
#[tauri::command]
pub fn message_resend(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
    message_id: String,
) -> Result<ChatMessageView, String> {
    resend_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id, &message_id)
}

/// 撤回消息（2 分钟窗口内；窗口外/不存在返回 `{ success: false }`）。
#[tauri::command]
pub fn message_recall(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
    message_id: String,
) -> Result<SuccessResult, String> {
    recall_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id, &message_id)
}

/// 删除单条消息（仅本地）。
#[tauri::command]
pub fn message_delete(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
    message_id: String,
) -> Result<SuccessResult, String> {
    delete_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id, &message_id)
}

/// 清零会话未读（direct 会话且对端可达时尽力发 read 信封）。
#[tauri::command]
pub fn message_mark_read(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<SuccessResult, String> {
    mark_read_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id)
}

/// 写入会话草稿。
#[tauri::command]
pub fn message_set_draft(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
    draft: String,
) -> Result<SuccessResult, String> {
    set_draft_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id, &draft)
}

/// 切换会话置顶。
#[tauri::command]
pub fn message_toggle_pin(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<SuccessResult, String> {
    toggle_pin_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id)
}

/// 切换会话免打扰。
#[tauri::command]
pub fn message_toggle_mute(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<SuccessResult, String> {
    toggle_mute_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id)
}

/// 清空会话聊天记录（保留会话入口）。
#[tauri::command]
pub fn message_clear(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<SuccessResult, String> {
    clear_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id)
}

/// 删除会话（会话与消息一并删除）。
#[tauri::command]
pub fn message_delete_conversation(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    conv_id: String,
) -> Result<SuccessResult, String> {
    delete_conversation_inner(&mut *lock_kernel(&state)?, &space_key, &conv_id)
}

// ------------------------------------------------------------------
// 单元测试（tests.rs）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests;
