//! 应用消息服务与限流器（服务号模型，p2p-messages.md §20）。
//!
//! 与 [`super::service::MessageService`] 同为纯逻辑层：只操作
//! [`StorageBackend`]，时间一律以 `now_ms` 注入。应用消息本地生成、本地
//! 消费——无 peer 投递、无 delivered 语义、不参与 dm 同步与存证（§20.4）。
//!
//! 写入校验链（按序，§20.2/§20.5）：pluginId 字符集 → payload.summary
//! 非空且 ≤ [`APP_SUMMARY_MAX_CHARS`] → 限流（内核门面侧，
//! [`AppMessageRateLimiter`]）。校验先于限流：非法消息不消耗配额。

use std::collections::HashMap;

use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

use super::types::{
    APP_MSG_RATE_LIMIT, APP_MSG_RATE_WINDOW_MS, APP_SUMMARY_MAX_CHARS, AppMessageCard,
    AppMessageRecord, ConversationRecord, app_conversation_id, app_message_key,
    app_message_prefix, conversation_key, is_valid_plugin_id,
};
use super::{MessageError, Result};

/// 应用消息本地状态集的唯一取值（§20.3：落库即终态，无投递状态机）。
pub const APP_MESSAGE_STATUS_LOCAL: &str = "local";

/// 限流器容量上限（键数；满时先回收过期窗口、仍满整体清空——同 §19 dm
/// 入站限流器口径）。
const RATE_LIMITER_MAX_KEYS: usize = 1024;

/// 单个 (space, pluginId) 的固定窗口计数。
#[derive(Clone, Copy, Debug, Default)]
struct RateWindow {
    /// 当前窗口起点（epoch 毫秒）。
    window_start: i64,
    /// 当前窗口已写入条数。
    count: u32,
    /// 累计拒绝数（熔断观测面，单调递增、不落盘）。
    rejected: u64,
}

/// 应用消息限流器（内核内存态，进程重启清零，§20.5）。
///
/// 每 `(space, pluginId)` 固定窗口 [`APP_MSG_RATE_WINDOW_MS`] 内最多写入
/// [`APP_MSG_RATE_LIMIT`] 条；超限拒绝并累计 `rejected`。
#[derive(Default)]
pub struct AppMessageRateLimiter {
    windows: HashMap<String, RateWindow>,
}

impl AppMessageRateLimiter {
    /// 判定是否放行本次写入：窗口过期先重置；放行计数 +1，超限拒绝计数 +1。
    pub fn check(&mut self, space: &str, plugin_id: &str, now_ms: i64) -> bool {
        self.evict_if_full(now_ms);
        let window = self
            .windows
            .entry(format!("{space}:{plugin_id}"))
            .or_insert_with(|| RateWindow {
                window_start: now_ms,
                count: 0,
                rejected: 0,
            });
        if now_ms - window.window_start >= APP_MSG_RATE_WINDOW_MS {
            // 固定窗口过期重置（拒绝计数不重置——它是累计观测面）
            window.window_start = now_ms;
            window.count = 0;
        }
        if window.count >= APP_MSG_RATE_LIMIT {
            window.rejected = window.rejected.saturating_add(1);
            return false;
        }
        window.count += 1;
        true
    }

    /// 指定会话的累计拒绝数（熔断观测面）。
    pub fn rejected_count(&self, space: &str, plugin_id: &str) -> u64 {
        self.windows
            .get(&format!("{space}:{plugin_id}"))
            .map(|w| w.rejected)
            .unwrap_or(0)
    }

    /// 容量守卫：满 1024 键时先回收过期窗口条目，仍满则整体清空（防内存无界）。
    fn evict_if_full(&mut self, now_ms: i64) {
        if self.windows.len() < RATE_LIMITER_MAX_KEYS {
            return;
        }
        self.windows
            .retain(|_, w| now_ms - w.window_start < APP_MSG_RATE_WINDOW_MS);
        if self.windows.len() >= RATE_LIMITER_MAX_KEYS {
            self.windows.clear();
        }
    }
}

/// 应用消息服务（无状态；全部方法以存储与参数为输入）。
pub struct AppMessageService;

impl AppMessageService {
    // ---------- 写入 ----------

    /// 校验并构造应用消息记录（不落库）：pluginId 字符集 → `payload.summary`
    /// 提取（缺失/非字符串/trim 后为空 → [`MessageError::MissingSummary`]；
    /// trim 后超 [`APP_SUMMARY_MAX_CHARS`] → [`MessageError::SummaryTooLong`]）。
    /// 记录内 `summary` = trim 后的 `payload.summary`（冗余提升，§20.2）。
    pub fn build_app_message(
        plugin_id: &str,
        payload: serde_json::Value,
        card: Option<AppMessageCard>,
        msg_id: String,
        now_ms: i64,
    ) -> Result<AppMessageRecord> {
        if !is_valid_plugin_id(plugin_id) {
            return Err(MessageError::InvalidPluginId);
        }
        let summary = payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(MessageError::MissingSummary)?;
        if summary.chars().count() > APP_SUMMARY_MAX_CHARS {
            return Err(MessageError::SummaryTooLong);
        }
        Ok(AppMessageRecord {
            id: msg_id,
            plugin_id: plugin_id.to_string(),
            summary: summary.to_string(),
            payload,
            card,
            created_at: now_ms,
            status: APP_MESSAGE_STATUS_LOCAL.to_string(),
            read: false,
        })
    }

    /// 找到或创建应用会话（幂等；id = `app:{pluginId}`，kind = App）。
    /// 首次创建标题缺省取 pluginId（壳层可据插件清单刷新标题）。
    pub fn ensure_app_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        plugin_id: &str,
        now_ms: i64,
    ) -> Result<ConversationRecord> {
        if !is_valid_plugin_id(plugin_id) {
            return Err(MessageError::InvalidPluginId);
        }
        let conv_id = app_conversation_id(plugin_id);
        if let Some(existing) =
            super::service::MessageService::get_conversation(storage, space, &conv_id)?
        {
            return Ok(existing);
        }
        let record = ConversationRecord {
            id: conv_id,
            kind: super::types::ConversationKind::App,
            title: plugin_id.to_string(),
            peer_root_id: plugin_id.to_string(),
            peer: None,
            unread_count: 0,
            pinned_at: 0,
            muted: false,
            draft: String::new(),
            updated_at: now_ms,
            meta_updated_at: 0,
        };
        super::service::MessageService::upsert_conversation(storage, space, &record)?;
        Ok(record)
    }

    /// 写入应用消息：落库 + 会话 `updatedAt` 前进 + 未读 +1（单 batch）。
    /// 会话不存在报 [`MessageError::ConversationNotFound`]（调用方应先
    /// [`Self::ensure_app_conversation`]）。归属由键派生天然保证：记录
    /// `plugin_id` 必须与会话 id `app:{pluginId}` 一致，否则
    /// [`MessageError::InvalidPluginId`]（§20.4 不变量 2）。
    pub fn append_app_message<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        message: &AppMessageRecord,
    ) -> Result<()> {
        let conv_id = app_conversation_id(&message.plugin_id);
        if !is_valid_plugin_id(&message.plugin_id) {
            return Err(MessageError::InvalidPluginId);
        }
        let mut conv = super::service::MessageService::get_conversation(storage, space, &conv_id)?
            .ok_or(MessageError::ConversationNotFound)?;
        conv.updated_at = conv.updated_at.max(message.created_at);
        conv.unread_count = conv.unread_count.saturating_add(1);
        storage.batch(vec![
            BatchOperation::put(
                app_message_key(space, &message.plugin_id, message.created_at, &message.id),
                serde_json::to_string(message)?,
            ),
            BatchOperation::put(
                conversation_key(space, &conv_id),
                serde_json::to_string(&conv)?,
            ),
        ])?;
        Ok(())
    }

    // ---------- 读取 ----------

    /// 应用会话消息列表（键升序 = 时间升序）。
    pub fn list_app_messages<S: StorageBackend>(
        storage: &S,
        space: &str,
        plugin_id: &str,
    ) -> Result<Vec<AppMessageRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(app_message_prefix(space, plugin_id)))?;
        rows.into_iter()
            .map(|(_, value)| serde_json::from_str(&value).map_err(MessageError::from))
            .collect()
    }

    /// 指定空间的全部应用会话（kind = App，键升序；水合用）。
    pub fn list_app_conversations<S: StorageBackend>(
        storage: &S,
        space: &str,
    ) -> Result<Vec<ConversationRecord>> {
        Ok(super::service::MessageService::list_conversations(storage, space)?
            .into_iter()
            .filter(|c| c.kind == super::types::ConversationKind::App)
            .collect())
    }

    // ---------- 已读 / 删除 ----------

    /// 标记已读：清零会话未读 + 会话内未读消息批量置 `read=true`
    ///（会话不存在时不动，对齐 `MessageService::mark_read`）。
    pub fn mark_app_read<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        plugin_id: &str,
    ) -> Result<()> {
        let conv_id = app_conversation_id(plugin_id);
        let Some(mut conv) =
            super::service::MessageService::get_conversation(storage, space, &conv_id)?
        else {
            return Ok(());
        };
        let mut ops = Vec::new();
        for (key, value) in storage.scan(&ScanOptions::prefix(app_message_prefix(space, plugin_id)))?
        {
            let mut msg: AppMessageRecord = serde_json::from_str(&value)?;
            if !msg.read {
                msg.read = true;
                ops.push(BatchOperation::put(key, serde_json::to_string(&msg)?));
            }
        }
        conv.unread_count = 0;
        ops.push(BatchOperation::put(
            conversation_key(space, &conv_id),
            serde_json::to_string(&conv)?,
        ));
        storage.batch(ops)?;
        Ok(())
    }

    /// 删除应用会话：会话与全部应用消息一并删除（§20.1）。
    pub fn delete_app_conversation<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        plugin_id: &str,
    ) -> Result<()> {
        let keys: Vec<String> = storage
            .scan(&ScanOptions::prefix(app_message_prefix(space, plugin_id)))?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        let mut ops: Vec<BatchOperation> =
            keys.into_iter().map(BatchOperation::delete).collect();
        ops.push(BatchOperation::delete(
            conversation_key(space, &app_conversation_id(plugin_id)),
        ));
        storage.batch(ops)?;
        Ok(())
    }
}
