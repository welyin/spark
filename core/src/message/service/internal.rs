use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

use super::super::Result;
use super::super::types::{
    ConversationRecord, MessageRecord, message_id_index_key, message_id_index_prefix,
    message_prefix,
};
use super::MessageService;

impl MessageService {
    /// 按消息 id 定位（键, 记录）：优先走 `msg:byid:` 二级索引直取；
    /// 索引缺失（索引机制上线前的存量消息无索引项）回退会话内全量扫描，
    /// 保证旧数据的去重/撤回/回写路径语义不变。
    pub(super) fn find_message_row<S: StorageBackend>(
        storage: &S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
    ) -> Result<Option<(String, MessageRecord)>> {
        if let Some(key) = storage.get(&message_id_index_key(space, conv_id, msg_id))?
            && let Some(raw) = storage.get(&key)?
        {
            return Ok(Some((key, serde_json::from_str(&raw)?)));
        }
        let rows = storage.scan(&ScanOptions::prefix(message_prefix(space, conv_id)))?;
        for (key, value) in rows {
            let msg: MessageRecord = serde_json::from_str(&value)?;
            if msg.id == msg_id {
                return Ok(Some((key, msg)));
            }
        }
        Ok(None)
    }

    /// 按消息 id 读取记录（入站去重/归属判定用；集成测试按 id 定点断言
    /// 也用本入口——优先 byid 索引直取，O(1)）。
    pub fn get_message<S: StorageBackend>(
        storage: &S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
    ) -> Result<Option<MessageRecord>> {
        Ok(Self::find_message_row(storage, space, conv_id, msg_id)?.map(|(_, msg)| msg))
    }

    /// 读-改-写单条消息；不存在时不动。
    pub(super) fn mutate_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        msg_id: &str,
        f: impl FnOnce(&mut MessageRecord),
    ) -> Result<()> {
        let Some((key, mut msg)) = Self::find_message_row(storage, space, conv_id, msg_id)? else {
            return Ok(());
        };
        f(&mut msg);
        storage.put(&key, &serde_json::to_string(&msg)?)?;
        Ok(())
    }

    /// 读-改-写单个会话；不存在时不动（对齐 TS `if (conv) ...`）。
    pub(super) fn mutate_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        f: impl FnOnce(&mut ConversationRecord),
    ) -> Result<()> {
        let Some(mut conv) = Self::get_conversation(storage, space, conv_id)? else {
            return Ok(());
        };
        f(&mut conv);
        Self::upsert_conversation(storage, space, &conv)
    }

    /// 读-改-写单个会话（pdsync 感知：写 pmeta）；不存在时不动。
    pub(super) fn mutate_conversation_pdsync<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
        now_ms: i64,
        node_id: &str,
        f: impl FnOnce(&mut ConversationRecord),
    ) -> Result<()> {
        let Some(mut conv) = Self::get_conversation(storage, space, conv_id)? else {
            return Ok(());
        };
        f(&mut conv);
        Self::upsert_conversation_pdsync(storage, space, &conv, now_ms, Some(node_id))
    }

    /// 删除会话全部消息键及其 `msg:byid:` 索引项。
    pub(super) fn delete_all_messages<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        conv_id: &str,
    ) -> Result<()> {
        let mut keys: Vec<String> = storage
            .scan(&ScanOptions::prefix(message_prefix(space, conv_id)))?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        keys.extend(
            storage
                .scan(&ScanOptions::prefix(message_id_index_prefix(
                    space, conv_id,
                )))?
                .into_iter()
                .map(|(key, _)| key),
        );
        if keys.is_empty() {
            return Ok(());
        }
        storage.batch(keys.into_iter().map(BatchOperation::delete).collect())?;
        Ok(())
    }
}
