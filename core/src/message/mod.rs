//! 消息模块：1:1 会话与消息的本地存储（对齐 wiki/design/ui/ui-messages.md，
//! 逻辑语义对齐前端 `app/src/mock/messages.ts` 的 store 方法）。
//!
//! 本模块为纯逻辑层：服务层（[`service::MessageService`]）只操作
//! [`crate::storage::StorageBackend`]，不涉及网络传输（加密发送、直连/网关
//! 路由、回执推送属 p2p 模块）。空间以 `space: &str` 参数体现
//! （`'personal'` 或 `'org:<orgId>'`）；存储后端本身已按身份隔离。
//!
//! 有意与前端 mock 的差异：`senderId` 一律存真实 rootId，TS mock 中的 `'me'`
//! 映射在 kernel 视图层完成，本层不感知。

pub mod service;
pub mod types;

pub use service::MessageService;
pub use types::{
    ConversationKind, ConversationRecord, LinkPreview, MAX_TEXT_BYTES, MessageRecord, MessageType,
    PeerRef, QuoteRef, RECALL_WINDOW_MS, conversation_key, conversation_prefix,
    generate_message_id, message_id_index_key, message_id_index_prefix, message_key,
    message_prefix,
};

/// 消息模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    /// 会话不存在。
    #[error("Conversation not found")]
    ConversationNotFound,

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 消息模块 Result 别名。
pub type Result<T> = std::result::Result<T, MessageError>;
