//! indexer 内核角色（wiki/architecture/community-affairs.md §9 C10、§10 决策 4；
//! 协议面 wiki/protocol/community/affair-metadata.md）。节点自选启用的角色：
//! 订阅 `spark-affair-meta` 元数据面 → 暂存区裁决（§5）→ 本地索引
//! （标题/简介/标签，§1）→ 确定性模糊匹配 + 健康信号（任何节点对同一查询
//! 返回同一结果，可全网复算）。推荐与呈现不在本模块（协议零约定）。
//!
//! 分层：announce/staging/local_index/match/health/log/query/directory 为纯
//! 逻辑 + 存储簿记（不碰网络与运行时，时间一律 `now_ms` 注入）；
//! kernel/index_ops 为门面编排；p2p 查询传输在 `crate::p2p`
//! （`/spark/affairmeta/1.0.0`）。directory 承担目录面：角色子集覆盖配置、
//! indexer 名片（`indexer-card`）线形与目录簿记（affair-metadata §7）。

use thiserror::Error;

pub mod announce;
pub mod directory;
pub mod health;
pub mod local_index;
pub mod log;
#[path = "match.rs"]
pub mod match_;
pub mod query;
pub mod staging;

// 模块名 match 是关键字，文件 match.rs 以 match_ 挂到 match_ 路径下，
// 门面 re-export 为 match 语义无冲突的消费名。
pub use match_::{MATCH_LIMIT_MAX, MatchHit, SearchQuery, search};

#[derive(Debug, Error)]
pub enum IndexError {
    /// 存储读写失败。
    #[error("index storage: {0}")]
    Storage(String),
    /// 公告线形校验失败（reason 稳定字符串）。
    #[error("{0}")]
    BadAnnounce(&'static str),
    /// 本地日志副本损坏（创世无法解析）。
    #[error("corrupt affair log: {0}")]
    CorruptLog(String),
}

impl From<crate::storage::StorageError> for IndexError {
    fn from(e: crate::storage::StorageError) -> Self {
        IndexError::Storage(e.to_string())
    }
}

impl From<crate::evidence::EvidenceError> for IndexError {
    fn from(e: crate::evidence::EvidenceError) -> Self {
        IndexError::Storage(format!("evidence: {e}"))
    }
}
