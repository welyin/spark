//! 消息服务层（对齐 `app/src/mock/messages.ts` 的 store 方法语义）。
//!
//! 纯逻辑层：只操作 [`StorageBackend`]，不触碰网络（发送/接收、加密、网关
//! 转发属 p2p 模块职责；`'me'` 与真实 rootId 的映射在 kernel 视图层完成）。
//! 时间一律以 `now_ms` 参数注入，保证纯函数可测。

use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

use super::types::{
    ConversationRecord, MessageRecord, PeerRef, RECALL_WINDOW_MS, conversation_key,
    conversation_prefix, generate_conversation_id, message_id_index_key, message_key,
    message_prefix,
};
use super::{MessageError, Result};

mod internal;

/// 消息服务（无状态；全部方法以存储与参数为输入）。
pub struct MessageService;

impl MessageService {
    // ---------- 会话读写 ----------

    /// 读取单个会话记录；不存在返回 `Ok(None)`。
    pub fn get_conversation<S: StorageBackend>(
        storage: &S,
        space: &str,
        conv_id: &str,
    ) -> Result<Option<ConversationRecord>> {
        let Some(raw) = storage.get(&conversation_key(space, conv_id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 读取指定空间的全部会话（键升序；置顶/时间排序在前端/kernel 视图层做）。
    ///
    /// 对齐 TS `readAllOrganizations` 的口径：损坏 JSON 直接报错（不静默跳过）。
    pub fn list_conversations<S: StorageBackend>(
        storage: &S,
        space: &str,
    ) -> Result<Vec<ConversationRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(conversation_prefix(space)))?;
        rows.into_iter()
            .map(|(_, value)| serde_json::from_str(&value).map_err(MessageError::from))
            .collect()
    }

    /// 按对方 rootId 查找 1:1 会话；不存在返回 `Ok(None)`。
    pub fn find_direct_conversation<S: StorageBackend>(
        storage: &S,
        space: &str,
        peer_root_id: &str,
    ) -> Result<Option<ConversationRecord>> {
        Ok(Self::list_conversations(storage, space)?.into_iter().find(|c| {
            c.kind == super::types::ConversationKind::Direct && c.peer_root_id == peer_root_id
        }))
    }

    /// 找到或创建与 `peer_root_id` 的 1:1 会话（存在即原样返回，幂等）。
    pub fn ensure_direct_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        peer_root_id: &str,
        title: &str,
        peer: Option<PeerRef>,
        now_ms: i64,
    ) -> Result<ConversationRecord> {
        if let Some(existing) = Self::find_direct_conversation(storage, space, peer_root_id)? {
            return Ok(existing);
        }
        let record = ConversationRecord {
            id: generate_conversation_id(now_ms),
            kind: super::types::ConversationKind::Direct,
            title: title.to_string(),
            peer_root_id: peer_root_id.to_string(),
            peer,
            unread_count: 0,
            pinned_at: 0,
            muted: false,
            draft: String::new(),
            updated_at: now_ms,
            meta_updated_at: 0,
        };
        Self::upsert_conversation(storage, space, &record)?;
        Ok(record)
    }

    /// 写入/覆盖会话记录。
    pub fn upsert_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        record: &ConversationRecord,
    ) -> Result<()> {
        storage.put(
            &conversation_key(space, &record.id),
            &serde_json::to_string(record)?,
        )?;
        Ok(())
    }

    /// 合并远端会话元数据（pdsync-data 的 `msg:conv` 合入）。
    ///
    /// 仅取同步字段（置顶/免打扰/草稿 + 会话身份字段 id/kind/title/peer），
    /// **保留本地消息驱动字段**（unread_count / updated_at / last 消息字段）——
    /// 那些由消息收发高频刷新，不应被远端元数据快照覆盖。
    ///
    /// 例外：自聊会话（peer_root == 本机 rootId）的 `peer` 是设备相对寻址，
    /// 由调用侧（inbound_dm 的 pdsync-data conv 合入分支）在合并后恢复本地值。
    ///
    /// 返回合并后的记录（不落盘，调用方自行写）。
    pub fn merge_conv_meta(
        local: &mut ConversationRecord,
        remote: &ConversationRecord,
    ) {
        // 身份字段：远端为准（同会话 id 同 peer）
        local.kind = remote.kind;
        local.title = remote.title.clone();
        local.peer = remote.peer.clone();
        // 同步的元数据字段
        local.pinned_at = remote.pinned_at;
        local.muted = remote.muted;
        local.draft = remote.draft.clone();
        local.meta_updated_at = remote.meta_updated_at;
        // 保留 local 的 unread_count / updated_at
    }

    /// pdsync 感知的会话写入：落记录 + bump pmeta（P2 conv 迁入）。
    ///
    /// 仅个人空间会话可同步（组织/应用会话不走 pdsync）；键含 `space`，
    /// 由调用方确保只对 `space=="personal"` 传入 node_id（对组织/应用空间
    /// 传 `None` 则退化为普通写入，不写 pmeta）。
    pub fn upsert_conversation_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        record: &ConversationRecord,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<()> {
        let key = conversation_key(space, &record.id);
        if space == "personal" {
            if let Some(node_id) = node_id {
                let json = serde_json::to_string(record)?;
                crate::sync::put_personal(storage, node_id, &key, &json, now_ms)
                    .map_err(|e| MessageError::Sync(e))?;
                return Ok(());
            }
        }
        storage.put(&key, &serde_json::to_string(record)?)?;
        Ok(())
    }

    /// 清零未读（`markRead`；会话不存在时不动），同时把会话内对端发来的
    /// 消息批量置本地已读（`read` 标记）——`delete_message` 据此精确判断
    /// 「删的是否未读消息」，只对真正未读的消息做未读 -1。
    pub fn mark_read<S: StorageBackend>(storage: &mut S, space: &str, conv_id: &str) -> Result<()> {
        let Some(mut conv) = Self::get_conversation(storage, space, conv_id)? else {
            return Ok(());
        };
        let mut ops = Vec::new();
        if conv.unread_count > 0 {
            // 对端消息批量置已读（自己发的消息 read 恒 false，不动）
            for (key, value) in
                storage.scan(&ScanOptions::prefix(message_prefix(space, conv_id)))?
            {
                let mut msg: MessageRecord = serde_json::from_str(&value)?;
                if msg.sender_id == conv.peer_root_id && !msg.read {
                    msg.read = true;
                    ops.push(BatchOperation::put(key, serde_json::to_string(&msg)?));
                }
            }
            conv.unread_count = 0;
            ops.push(BatchOperation::put(
                conversation_key(space, conv_id),
                serde_json::to_string(&conv)?,
            ));
        }
        if !ops.is_empty() {
            storage.batch(ops)?;
        }
        Ok(())
    }

    /// 未读 +1（会话不存在时不动）。
    pub fn increment_unread<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
    ) -> Result<()> {
        Self::mutate_conversation(storage, space, conv_id, |conv| {
            conv.unread_count = conv.unread_count.saturating_add(1);
        })
    }

    /// 写入草稿（会话不存在时不动）。刷新 `meta_updated_at`（conv-sync
    /// LWW 依据）。
    pub fn set_draft<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        draft: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::mutate_conversation(storage, space, conv_id, |conv| {
            conv.draft = draft.to_string();
            conv.meta_updated_at = now_ms;
        })
    }

    /// pdsync 感知的写入草稿（写 pmeta）。
    pub fn set_draft_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        draft: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        Self::mutate_conversation_pdsync(storage, space, conv_id, now_ms, node_id, |conv| {
            conv.draft = draft.to_string();
            conv.meta_updated_at = now_ms;
        })
    }

    /// 切换置顶：`pinnedAt` 在 0 与 `now_ms` 之间切换（会话不存在时不动）。
    /// 刷新 `meta_updated_at`（conv-sync LWW 依据）。
    pub fn toggle_pin<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::mutate_conversation(storage, space, conv_id, |conv| {
            conv.pinned_at = if conv.pinned_at > 0 { 0 } else { now_ms };
            conv.meta_updated_at = now_ms;
        })
    }

    /// pdsync 感知的切换置顶（写 pmeta）。
    pub fn toggle_pin_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        Self::mutate_conversation_pdsync(storage, space, conv_id, now_ms, node_id, |conv| {
            conv.pinned_at = if conv.pinned_at > 0 { 0 } else { now_ms };
            conv.meta_updated_at = now_ms;
        })
    }

    /// 切换免打扰（会话不存在时不动）。刷新 `meta_updated_at`（conv-sync
    /// LWW 依据）。
    pub fn toggle_mute<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::mutate_conversation(storage, space, conv_id, |conv| {
            conv.muted = !conv.muted;
            conv.meta_updated_at = now_ms;
        })
    }

    /// pdsync 感知的切换免打扰（写 pmeta）。
    pub fn toggle_mute_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        Self::mutate_conversation_pdsync(storage, space, conv_id, now_ms, node_id, |conv| {
            conv.muted = !conv.muted;
            conv.meta_updated_at = now_ms;
        })
    }

    /// 清空聊天记录：仅删本地消息，保留会话入口并把未读清零（ui-messages.md §5.1）。
    pub fn clear_messages<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
    ) -> Result<()> {
        Self::delete_all_messages(storage, space, conv_id)?;
        Self::mutate_conversation(storage, space, conv_id, |conv| conv.unread_count = 0)
    }

    /// 删除会话：会话与消息一并删除（ui-messages.md §5.1）。
    pub fn delete_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
    ) -> Result<()> {
        Self::delete_all_messages(storage, space, conv_id)?;
        storage.delete(&conversation_key(space, conv_id))?;
        Ok(())
    }

    /// pdsync 感知的删除会话：个人空间会话删除写 tombstone pmeta（删除可经
    /// 自设备 pdsync 传播；裸 delete 会留下非 tombstone 的陈旧 pmeta 污染
    /// 该类目 folded vv）。组织/应用空间或 node_id 缺失时退化为裸删除。
    pub fn delete_conversation_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<()> {
        Self::delete_all_messages(storage, space, conv_id)?;
        let key = conversation_key(space, conv_id);
        if space == "personal"
            && let Some(node_id) = node_id
        {
            // 只有记录存在时才 tombstone（避免空删产生垃圾 pmeta，同 delete_tag）
            if storage.get(&key)?.is_some() {
                crate::sync::delete_personal(storage, node_id, &key, now_ms)
                    .map_err(MessageError::Sync)?;
            }
            return Ok(());
        }
        storage.delete(&key)?;
        Ok(())
    }

    // ---------- 消息读写 ----------

    /// 读取会话全部消息（键升序 = 时间升序）。
    pub fn get_messages<S: StorageBackend>(
        storage: &S,
        space: &str,
        conv_id: &str,
    ) -> Result<Vec<MessageRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(message_prefix(space, conv_id)))?;
        rows.into_iter()
            .map(|(_, value)| serde_json::from_str(&value).map_err(MessageError::from))
            .collect()
    }

    /// 追加消息，同时把会话 `updatedAt` 更新为消息 `createdAt`（乱序/补发
    /// 消息不回退：取两者较大值），并维护 `msg:byid:` 二级索引（按 id
    /// 定位直取，见 [`Self::find_message_row`]）。
    ///
    /// 会话不存在时报 [`MessageError::ConversationNotFound`]（对齐 TS `sendText`
    /// 对缺失会话返回 `undefined` 的拒绝语义）。
    pub fn append_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        message: &MessageRecord,
    ) -> Result<()> {
        let mut conv = Self::get_conversation(storage, space, conv_id)?
            .ok_or(MessageError::ConversationNotFound)?;
        let key = message_key(space, conv_id, message.created_at, &message.id);
        storage.batch(vec![
            BatchOperation::put(key.clone(), serde_json::to_string(message)?),
            BatchOperation::put(message_id_index_key(space, conv_id, &message.id), key),
            BatchOperation::put(
                conversation_key(space, conv_id),
                serde_json::to_string(&{
                    conv.updated_at = conv.updated_at.max(message.created_at);
                    conv
                })?,
            ),
        ])?;
        Ok(())
    }

    /// pdsync 感知的追加消息：消息/索引/会话壳写入同 [`Self::append_message`]，
    /// 个人空间会话壳额外 bump pmeta——只收发消息的会话由此进入 pdsync
    /// （无 pmeta 则对 folded vv 与增量推送不可见）。组织/应用空间或
    /// node_id 缺失时退化为普通追加。
    pub fn append_message_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        message: &MessageRecord,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<()> {
        Self::append_message(storage, space, conv_id, message)?;
        if let Some(node_id) = node_id {
            Self::bump_conv_pmeta_for_message(storage, space, conv_id, now_ms, node_id)?;
        }
        Ok(())
    }

    /// 消息追加路径的 conv pmeta bump（仅个人空间，其余空间直接返回）：
    /// vv 递增让"只收发消息"的会话壳进入 pdsync folded vv 与增量推送；
    /// **ts 保持会话的元数据时间 `meta_updated_at`**——LWW 裁决依据的是
    /// 元数据（pin/mute/draft）编辑时间，消息流把 pmeta.ts 刷成当前时间会
    /// 让纯收发消息的设备在并发裁决中仅凭 ts 胜出，吃掉对端真实的元数据
    /// 编辑。人消息（msg:item）与应用消息（msg:app）的 append 路径共用。
    pub fn bump_conv_pmeta_for_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        if space != "personal" {
            return Ok(());
        }
        // 会话刚由调用方的 append 写入，必然存在；读不到时以当前时间兜底
        let ts = Self::get_conversation(storage, space, conv_id)?
            .map(|conv| conv.meta_updated_at)
            .unwrap_or(now_ms);
        crate::sync::bump_personal_meta(storage, node_id, &conversation_key(space, conv_id), ts)
            .map_err(MessageError::Sync)?;
        Ok(())
    }

    /// 更新单条消息的发送状态；已撤回或消息不存在时不动（对齐 TS `setStatus`）。
    pub fn set_message_status<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
        status: &str,
    ) -> Result<()> {
        Self::mutate_message(storage, space, conv_id, msg_id, |msg| {
            if !msg.recalled {
                msg.status = Some(status.to_string());
            }
        })
    }

    /// 条件回写发送状态（compare-and-set）：仅当消息存在、未撤回且当前
    /// 状态仍为 `sending` 时才写入 `status`，返回是否实际写入。
    ///
    /// 投递任务的终态回写用（`sending → delivered/failed`）：重发会把状态
    /// 重新置为 `sending` 并 spawn 新投递任务，旧任务的迟到回写若发现
    /// 新任务已写入终态（`delivered`/`failed`）则放弃——避免过期任务把
    /// 重发成功的 `delivered` 覆写回 `failed`（resend 回写竞态）。
    pub fn set_message_status_if_sending<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
        status: &str,
    ) -> Result<bool> {
        let Some((key, mut msg)) = Self::find_message_row(storage, space, conv_id, msg_id)? else {
            return Ok(false);
        };
        if msg.recalled || msg.status.as_deref() != Some("sending") {
            return Ok(false);
        }
        msg.status = Some(status.to_string());
        storage.put(&key, &serde_json::to_string(&msg)?)?;
        Ok(true)
    }

    /// 把 `reader_root_id` 在此会话发出的、状态为 `sent`/`delivered` 的消息置为
    /// `read`（「对方已读我发的」回执路径：调用方传入自己的 rootId），返回改动的
    /// 消息 id 列表（键升序）。
    pub fn mark_peer_messages_read<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        reader_root_id: &str,
    ) -> Result<Vec<String>> {
        let prefix = message_prefix(space, conv_id);
        let rows = storage.scan(&ScanOptions::prefix(&prefix))?;
        let mut ops = Vec::new();
        let mut changed = Vec::new();
        for (key, value) in rows {
            let mut msg: MessageRecord = serde_json::from_str(&value)?;
            let unread_receipt = matches!(msg.status.as_deref(), Some("sent" | "delivered"));
            if msg.sender_id == reader_root_id && unread_receipt && !msg.recalled {
                msg.status = Some("read".to_string());
                ops.push(BatchOperation::put(key, serde_json::to_string(&msg)?));
                changed.push(msg.id);
            }
        }
        if !ops.is_empty() {
            storage.batch(ops)?;
        }
        Ok(changed)
    }

    /// 撤回：仅发送后 2 分钟内且未撤回的消息允许（ui-messages.md §9.1），
    /// 返回是否成功。
    pub fn recall_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(msg) = Self::get_message(storage, space, conv_id, msg_id)? else {
            return Ok(false);
        };
        if msg.recalled || now_ms - msg.created_at > RECALL_WINDOW_MS {
            return Ok(false);
        }
        Self::mutate_message(storage, space, conv_id, msg_id, |m| m.recalled = true)?;
        Ok(true)
    }

    /// 强制撤回（入站 recall 信封路径）：不做 2 分钟窗口判定——窗口约束由
    /// 发送方在本地执行，接收方信任对端信封（已验签）并按指令落库。
    /// 归属校验：仅当存储消息的 `sender_id == expected_sender`（信封 from）
    /// 才撤回——否则对端可撤回我方发出的消息。
    /// 消息存在、未撤回且归属匹配时置 `recalled` 并返回 `true`，否则 `false`。
    pub fn force_recall<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
        expected_sender: &str,
    ) -> Result<bool> {
        let Some(msg) = Self::get_message(storage, space, conv_id, msg_id)? else {
            return Ok(false);
        };
        if msg.recalled || msg.sender_id != expected_sender {
            return Ok(false);
        }
        Self::mutate_message(storage, space, conv_id, msg_id, |m| m.recalled = true)?;
        Ok(true)
    }

    /// 删除单条消息（仅本地删除，ui-messages.md §5.2；不存在时不动）。
    /// 仅当被删消息是「对端发来且本地未读」（`read` 标记未置位，由
    /// `mark_read` 维护）时才未读 -1——删早已读的历史消息不误清真正
    /// 未读的角标。同时清理 `msg:byid:` 索引项。
    pub fn delete_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
    ) -> Result<()> {
        let Some((key, msg)) = Self::find_message_row(storage, space, conv_id, msg_id)? else {
            return Ok(());
        };
        storage.batch(vec![
            BatchOperation::delete(key),
            BatchOperation::delete(message_id_index_key(space, conv_id, msg_id)),
        ])?;
        if let Some(mut conv) = Self::get_conversation(storage, space, conv_id)?
            && msg.sender_id == conv.peer_root_id
            && !msg.read
            && conv.unread_count > 0
        {
            conv.unread_count -= 1;
            Self::upsert_conversation(storage, space, &conv)?;
        }
        Ok(())
    }

}
