//! 消息模块：1:1 会话与消息的本地存储（对齐 wiki/design/ui/ui-messages.md，
//! 逻辑语义对齐前端 `app/src/mock/messages.ts` 的 store 方法）；应用消息
//! （服务号模型，p2p-messages.md §20）见 [`app`] 子模块。
//!
//! 本模块为纯逻辑层：服务层（[`service::MessageService`]）只操作
//! [`crate::storage::StorageBackend`]，不涉及网络传输（加密发送、直连/网关
//! 路由、回执推送属 p2p 模块）。空间以 `space: &str` 参数体现
//! （`'personal'` 或 `'org:<orgId>'`）；存储后端本身已按身份隔离。
//!
//! 有意与前端 mock 的差异：`senderId` 一律存真实 rootId，TS mock 中的 `'me'`
//! 映射在 kernel 视图层完成，本层不感知。

pub mod app;
pub mod service;
pub mod types;

pub use app::{APP_MESSAGE_STATUS_LOCAL, AppMessageRateLimiter, AppMessageService};
pub use service::MessageService;
pub use types::{
    APP_CONV_PREFIX, APP_MSG_RATE_LIMIT, APP_MSG_RATE_WINDOW_MS, APP_SUMMARY_MAX_CHARS,
    AppMessageCard, AppMessageRecord, ConversationKind, ConversationRecord, LinkPreview,
    MAX_TEXT_BYTES, MessageRecord, MessageType, PeerRef, QuoteRef, RECALL_WINDOW_MS,
    app_conversation_id, app_message_key, app_message_prefix, conversation_key,
    conversation_prefix, generate_message_id, is_valid_plugin_id, message_id_index_key,
    message_id_index_prefix, message_key, message_prefix,
};

/// 消息模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    /// 会话不存在。
    #[error("Conversation not found")]
    ConversationNotFound,

    /// 应用消息 payload 缺失 summary / 非字符串 / trim 后为空（§20.2）。
    #[error("missing-summary: app message payload requires a non-empty summary")]
    MissingSummary,

    /// 应用消息 summary 超长（trim 后 > [`types::APP_SUMMARY_MAX_CHARS`] 字符）。
    #[error("summary-too-long: app message summary exceeds 200 chars")]
    SummaryTooLong,

    /// pluginId 非法（字符集 `^[a-z0-9][a-z0-9-]{0,63}$`，§20.1）。
    #[error("invalid-plugin-id")]
    InvalidPluginId,

    /// 应用消息限流超限（§20.5：消息不落库、未读不变）。
    #[error("rate-limited: app message rate limit exceeded (10/60s)")]
    RateLimited,

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 消息模块 Result 别名。
pub type Result<T> = std::result::Result<T, MessageError>;
