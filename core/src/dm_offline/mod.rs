//! dm_offline：统一密文暂存补投层（S3，纯逻辑层）。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §6/§12-S3] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.3]。
//!
//! ## 职责
//!
//! 消息一旦发出就是密文，暂存方只负责持有 + 投递。本模块提供 pending 队列的
//! 存储语义（键构造 / CRUD / TTL / 容量），覆盖 chat + feed + friend-request
//! 等全 kind。补投的拨号与投递（网络）由 kernel 编排层执行：
//!
//! - 存储键：个人 `dm:pending:{toRootId}:{messageId}`（feed 复用个人空间键）、
//!   组织 `org:dm:pending:{orgId}:{toRootId}:{messageId}`；
//! - 个人 pending 经 `put_personal` 入 pdsync（自设备互为补投备份）；组织
//!   空间按个人同构落地（org-sync 网关通道未就绪，差距见报告）；
//! - TTL 7 天、单 recipient 上限 100 条、全局上限 1000 条（超限淘汰最旧）；
//! - 补投触发：`on_peer_connected` 按 peerId 反查 rootId flush + 60s 周期
//!   flush 兜底（编排层）。
//!
//! ## 纪律
//!
//! 纯逻辑层：只操作 `StorageBackend` 泛型，`now_ms` 时间注入，不碰网络、
//! 不依赖 Tauri。

mod service;
mod types;

pub use service::{
    DmOfflineError, PendingSpace, enqueue, list_for_recipient, pending_key, prune_expired, remove,
};
pub use types::{
    GLOBAL_PENDING_CAP, ORG_PENDING_PREFIX, PENDING_PREFIX, PER_RECIPIENT_PENDING_CAP,
    PENDING_TTL_MS, PendingRecord,
};

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
