//! affairsync 事务复制面（wiki/protocol/community/affair-sync.md，总体方案
//! §6 方案 A1）：orgsync 复制组机制的 scope 泛化——复制组 = 动态关注者集合。
//!
//! 复用 hello/need/data 三信封反熵（affairId 单集合，无墓碑/dlog 面）；
//! 验签从「from ∈ 成员表复制组」放宽为「from ∈ 关注者 + 逐条签名链」：
//! 数据面安全边界 = 创世 `verify_genesis` 全链 + 操作 `verify_op` 入站校验链
//! （affair.md §3.2），乱序暂存/drain 语义与 C1 `OpLog` 对齐并持久化。
//!
//! 本模块是纯逻辑（存储泛型），不触碰 p2p/签名——信封装配与投递由调用方
//! （kernel 编排或测试宿主）完成。membership 发现最小可用 = 关注者目录
//! （复制流量线索，dir.rs）+ `spark-affair-meta` 元数据面（p2p 层路由，
//! affair-metadata.md；indexer 查询协议归 C10）。
//!
//! 目录形态（按职责）：
//! - [`follow`]：关注/取关簿记（`affair:follow:` 本地键）；
//! - [`envelope`]：三信封 body 构造/解析 + 记录类型 + 分批 + key 白名单；
//! - [`collect`]：vv 折叠、增量采集、diff 裁决；
//! - [`apply`]：入站应用（校验链 + 落库 + 存证锚定 + 头维护）与本地写入入口；
//! - [`drain`]：乱序暂存与 drain 循环；
//! - [`dir`]：关注者目录（覆盖网线索）；
//! - [`inbound`]：hello/need 入站编排（diff 裁决 → 应答出向）；
//! - [`keys`]：本面层簿记键构造函数。

mod apply;
mod collect;
mod dir;
mod drain;
mod envelope;
mod follow;
mod inbound;
mod keys;

#[cfg(test)]
mod tests;

pub use apply::{AffairApplySummary, apply_affairsync_records, ingest_local_entries};
pub use collect::{
    AffairDiffOutcome, affair_has_any_records, collect_affair_records, collect_affair_vv,
    diff_affair,
};
pub use dir::{FollowerHint, follower_hints, note_follower_seen};
pub use envelope::{
    AFFAIRSYNC_BATCH_BYTES, AffairsyncOut, AffairsyncRecord, KIND_AFFAIRSYNC_DATA,
    KIND_AFFAIRSYNC_HELLO, KIND_AFFAIRSYNC_NEED, build_affairsync_data_batch,
    build_affairsync_hello, build_affairsync_need, parse_affairsync_data, parse_affairsync_hello,
    parse_affairsync_need, record_key_in_scope, split_affairsync_batches,
};
pub use follow::{
    FollowState, follow_affair, follow_state, is_following, list_followed_affairs, unfollow_affair,
};
pub use inbound::{handle_affairsync_hello, handle_affairsync_need};
pub use keys::{AFFAIRSYNC_DIR_PREFIX, AFFAIRSYNC_PEND_PREFIX};
