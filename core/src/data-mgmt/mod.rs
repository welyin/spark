//! 数据治理模块：用量统计（usage）、L1 过期清理（cleanup）、手动清理（purge）、
//! purge 水位线（watermark）、全库导出（exporter）与门面服务（service）。
//!
//! 逐行对齐 `desktop/src/main/data-management/{constants,usage,cleanup,purge,watermark,exporter,service}.ts`
//! 与 `desktop/src/main/ipc/data.ts`；精确规格见 `core/spec/data-mgmt.md`。
//!
//! 本模块为纯逻辑层：只操作 [`crate::storage::StorageBackend`]；时间（`Date.now()`）
//! 一律以 `now_ms` 参数注入；1h 周期调度由宿主负责（Rust 内核不含定时器，
//! [`service::DataManagementService::tick`] 由宿主按
//! [`constants::DATA_MAINTENANCE_INTERVAL_MS`] 间隔调用）。
//!
//! ## 如实复刻的 TS 已知坑（core/spec/data-mgmt.md §9）
//!
//! - 坑 #2：全库扫描 `prefix:''` + 排他上界 `U+10FFFF`——恰好等于/以 `U+10FFFF`
//!   开头的极端 key 不被统计/导出（实际不产生此类 key）；
//! - 坑 #3：cleanup 对 JSON 损坏行保守跳过，永不清理；
//! - 坑 #4：空 purge 不留痕（不抬水位线、不写审计日志）；
//! - 坑 #5：审计日志 key 以毫秒时间戳结尾，同毫秒两次 purge 互相覆盖；
//! - 坑 #6：`confirmExported` 仅为调用方布尔确认，无导出事实核验；
//! - 坑 #7：水位线对 `remoteTs <= 0`（TS 中还有"非 number"，Rust 类型系统已排除）
//!   一律放行；preview 不鉴权管理员（见 [`service::DataManagementService::preview_purge`]）；
//! - 坑 #8：单集合 purge 仅模块层支持（[`purge::PurgeOptions::collection`]），
//!   service 的 execute 恒为全域清理；
//! - 坑 #10：idx 尾部匹配 `endsWith(':'+id)` 对含冒号 id 会误匹配
//!   （当前各环节产生的 id 均不含冒号）；
//! - 坑 #11：`cachedUsage`/`lastAutoCleanupAt`/`purgeInFlight` 纯内存，重启即丢；
//!   首个 tick 因 `last_auto_cleanup_at = 0` 必然执行一轮清理；
//! - 坑 #12：`run_cleanup_now` 不重采样，缓存置 `None` 后等下次 `get_usage` 现算。
//!
//! ## 有意修复（继承自 storage 层，非本模块新增）
//!
//! 坑 #1（扫描上界双标）：TS 仅 data-management 内统一 `U+10FFFF`，模块外 12 处
//! 仍为 `\xFF` 漏扫非 ASCII key；Rust 内核的 [`crate::storage`] 默认上界即
//! `U+10FFFF`，全部路径一致覆盖。

pub mod cleanup;
pub mod constants;
pub mod exporter;
pub mod purge;
pub mod service;
pub mod usage;
pub mod watermark;

pub use cleanup::{AutoCleanupResult, P2P_PEER_RECORD_PREFIX, run_auto_cleanup};
pub use constants::{
    AUTO_CLEANUP_MIN_INTERVAL_MS, DATA_MAINTENANCE_INTERVAL_MS, DISK_FREE_WARN_RATIO,
    ORG_SYNC_STATE_RETENTION_MS, PEER_RECORD_RETENTION_MS, TOMBSTONE_RETENTION_MS,
    USAGE_WARN_TOTAL_BYTES,
};
pub use exporter::{
    EXPORT_APP, EXPORT_FORMAT_VERSION, ExportDump, ExportEntry, ExportWriteResult,
    build_export_dump, write_export_dump,
};
pub use purge::{
    PurgeOptions, PurgePreview, PurgeResult, preview_purge_domain_docs, purge_domain_docs,
};
pub use service::{DataManagementService, ReplicaStatus};
pub use usage::{
    DataUsageReport, DiskInfo, UsageClass, UsageClassStat, UsageClasses, UsageWarnings,
    classify_key, collect_data_usage, measure_disk_info,
};
pub use watermark::{
    PURGE_WATERMARK_KEY_PREFIX, PurgeWatermarkRecord, StoragePurgeWatermark,
    get_purge_watermark, is_purged_by_watermark, purge_watermark_key, raise_purge_watermark,
};

/// data-mgmt 模块错误。对外校验类消息文本与 TS 抛出的 `Error.message` 逐字一致。
#[derive(Debug, thiserror::Error)]
pub enum DataMgmtError {
    /// purge 目标域非 `plugin:` 域（purge.ts:51-53）。
    #[error("Refused to purge non-plugin domain \"{0}\": only plugin domains can be purged")]
    NonPluginDomain(String),

    /// `beforeTs` 非正数（purge.ts:54-56；TS 还检查 `typeof === 'number'`，Rust 类型已排除）。
    #[error("beforeTs must be a positive timestamp")]
    InvalidBeforeTs,

    /// 当前用户不是组织管理员（ipc/data.ts:96-98）。
    #[error("Only organization admins can purge historical data")]
    NotOrgAdmin,

    /// 未确认已导出备份（ipc/data.ts:99-101）。
    #[error("Export backup first: confirmExported must be true before purging")]
    ExportNotConfirmed,

    /// P2P 未启动，无法核验副本充足性（ipc/data.ts:102-105）。
    #[error("P2P network is not started; cannot verify replica sufficiency, purge refused")]
    P2PNotStarted,

    /// K 副本不足（ipc/data.ts:106-111）。
    #[error("Replica insufficient ({synced}/{target}): purging local copies now may lose organization data. Wait for replicas to replenish or add disk space instead.")]
    ReplicaInsufficient {
        /// 已同步副本数。
        synced: u32,
        /// 副本目标 K。
        target: u32,
    },

    /// 同域 purge 进行中（ipc/data.ts:113-115 的 `purgeInFlight` 护栏）。
    #[error("A purge for domain {0} is already running; wait for it to finish")]
    PurgeInFlight(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// 文件 IO 错误（exporter 写文件）。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// data-mgmt 模块 Result 别名。
pub type Result<T> = std::result::Result<T, DataMgmtError>;
