//! 同步模块：meta 读写、版本向量比较、LWW 裁决与 `applyRemoteUpdate`。
//!
//! 算法精确规格见 `core/spec/sync-evidence.md` §4，验收向量见
//! `core/spec/vectors/sync-evidence.json`。

pub mod apply;
pub mod meta;

pub use apply::{
    ApplyOutcome, ApplyRemoteOptions, CollectionAdapter, PurgeWatermark, apply_remote_update,
};
pub use meta::{
    CompareResult, DocMeta, RemoteMeta, VersionVector, compare_version_vectors,
    generate_updated_meta, get_meta, merge_version_vectors, meta_key, resolve_conflict_by_lww,
    set_meta,
};

/// sync 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// schema 模块错误（策略解析失败）。
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),

    /// evidence 模块错误（存证写入失败）。
    #[error(transparent)]
    Evidence(#[from] crate::evidence::EvidenceError),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// 集合适配器错误。
    #[error("collection adapter error: {0}")]
    Adapter(String),

    /// 水位线检查错误。
    #[error("watermark check error: {0}")]
    Watermark(String),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// sync 模块 Result 别名。
pub type SyncResult<T> = std::result::Result<T, SyncError>;
