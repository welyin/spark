//! 内容面：blob 内容寻址存储 + Kad provider「持有即做种」+ 按 CID 直连拉取。
//!
//! 设计依据（以协议文档为准）：
//! - wiki `product/public-topics.md` §七「内容面：代码与文件」：内容寻址 blob，
//!   **持有即做种**（任何持有副本的节点自然响应拉取，移动端叶子模式除外——
//!   手机持有副本但不服务），退出议题即停止做种；
//! - wiki `architecture/community-affairs.md`：内容面 = Git/blob 内容寻址 +
//!   Kad provider 记录（与事务日志面 A1、元数据发现面 gossipsub 并列）；
//! - wiki `architecture/plugins/social-feed.md` §7.3：hash 即能力——256 bit
//!   内容寻址不可枚举，持有哈希即持有读取能力；
//! - wiki `architecture/p2p/dht-republish-libp2p.md`：provider 声明无 TTL，
//!   靠周期重发维持（本骨架复用 keepalive tick 重发线形）。
//!
//! 与既有 `plugindata::blob`（pdsync 面）的边界：pdsync blob 是同身份自设备间
//! 的附件同步（`blob:` 键命名空间、dm 信封拉取、PC eager/手机 lazy）；本模块是
//! **跨主体内容面**（`cblob:` 键命名空间、Kad provider 发现），两者互不复用
//! 存储区，避免互相被对方的 GC 误回收。
//!
//! 本阶段不做（后续里程碑）：扩散策略、激励机制、与事务/凭证面的深度集成、
//! Git 仓库托管。按 CID 从 provider 拉取本体的传输协议已落地：
//! `/spark/blob-fetch/1.0.0` 直连 request/response（线形在
//! `p2p/direct/blob_fetch.rs`，事件循环处理在 `p2p/node/blob_fetch.rs`），
//! kernel 门面编排（Kad 检索 → 地址解析 → 连接 → 拉取）在
//! `kernel/content_ops.rs`。

pub mod cid;
pub mod refs;
pub mod store;

pub use cid::{BLOB_KAD_KEY_PREFIX, Cid, blob_kad_key};
pub use refs::extract_blob_cids;
pub use store::{
    CONTENT_BLOB_MAX_BYTES, CONTENT_GC_GRACE_MS, ContentBlobInfo, delete_blob, gc_sweep, has_blob,
    list_blobs, pin_root, read_blob, roots_of, save_blob, unpin_root,
};

/// 内容面统一错误。
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    /// CID 形状非法（须为 64 位小写 hex 的 SHA-256）。
    #[error("invalid cid: {0}")]
    InvalidCid(String),

    /// blob 超过大小上限。
    #[error("blob exceeds {0} bytes")]
    TooLarge(usize),

    /// 完整性校验失败（读出的内容与 CID 不匹配）。
    #[error("blob integrity check failed: {0}")]
    Integrity(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 内容面 Result 别名。
pub type Result<T> = std::result::Result<T, ContentError>;
