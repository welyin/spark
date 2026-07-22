//! 常量总表（data-management/constants.ts；core/spec/data-mgmt.md §1）。
//!
//! 扫描上界 [`crate::storage::KEY_RANGE_UPPER_BOUND`]（`U+10FFFF`）定义在 storage 层，
//! Rust 内核全部路径统一使用，不继承 TS 模块外 `\xFF` 的历史坑（spec §9.1）。

/// lww tombstone 保留期：90 天（constants.ts:11）。
pub const TOMBSTONE_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000; // 7_776_000_000

/// p2p 节点活跃记录保留期：90 天（constants.ts:14）。
pub const PEER_RECORD_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// p2p 组织同步记账保留期：90 天（constants.ts:17）。
pub const ORG_SYNC_STATE_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// 调度 tick 周期：1 小时（constants.ts:20）。由宿主定时器按此间隔调用 tick。
pub const DATA_MAINTENANCE_INTERVAL_MS: i64 = 60 * 60 * 1000; // 3_600_000

/// 自动清理最小间隔：24 小时（constants.ts:23）。
pub const AUTO_CLEANUP_MIN_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000; // 86_400_000

/// 软配额警告阈值：1 GiB（constants.ts:26；只提示，不拒绝写入）。
pub const USAGE_WARN_TOTAL_BYTES: u64 = 1024 * 1024 * 1024; // 1_073_741_824

/// 磁盘可用比例警告阈值（constants.ts:29；严格 `<` 判定）。
pub const DISK_FREE_WARN_RATIO: f64 = 0.15;
