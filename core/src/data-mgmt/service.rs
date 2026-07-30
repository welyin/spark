//! 数据自动管理门面（service.ts + ipc/data.ts；core/spec/data-mgmt.md §6/§8）。
//!
//! - 周期调度：TS 为 `KeepaliveScheduler('data-maintenance', 3_600_000, tick)`
//!   （setInterval，start 后首个 tick 1h 才触发，防重入，timer unref）；Rust 内核
//!   不含定时器，由宿主按 [`DATA_MAINTENANCE_INTERVAL_MS`] 间隔调用
//!   [`DataManagementService::tick`]，`start`/`stop` 仅维护幂等的运行标记；
//! - 手动入口：立即清理 / 刷新用量 / purge 预览与执行；
//! - 管理员/副本/P2P 状态以参数注入，不依赖 org/p2p 具体类型。

use std::collections::HashSet;

use crate::storage::StorageBackend;

use super::cleanup::{AutoCleanupResult, run_auto_cleanup};
use super::constants::AUTO_CLEANUP_MIN_INTERVAL_MS;
use super::purge::{
    PurgeOptions, PurgePreview, PurgeResult, preview_purge_domain_docs, purge_domain_docs,
};
use super::usage::{DataUsageReport, collect_data_usage};
use super::{DataMgmtError, Result};

pub use super::constants::DATA_MAINTENANCE_INTERVAL_MS;

/// 组织 K 副本充足性（ipc/data.ts:38-43 `getReplicaOverview` 的注入形态；
/// `None` 表示 P2P 未初始化或未启动）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplicaStatus {
    /// 已持有副本的成员数（含本机）。
    pub synced_peers: u32,
    /// 副本目标 K。
    pub replica_target: u32,
}

/// 数据自动管理门面（service.ts:16-76）。
///
/// 坑 #11 如实复刻：`cached_usage`/`last_auto_cleanup_at`/`purge_in_flight`
/// 均为**纯内存**字段，进程重启即丢；`last_auto_cleanup_at` 初值 0 →
/// 启动后第一个 tick 必然执行一轮清理（前提：`now >= 24h`，现实时间恒成立）。
pub struct DataManagementService {
    /// 数据目录路径（用量统计附带磁盘信息；`None` 时 disk 恒为 `None`）。
    disk_path: Option<String>,
    /// 运行标记（start/stop 幂等；实际调度由宿主定时器负责）。
    running: bool,
    /// 上次自动清理时间（内存字段，初值 0）。
    last_auto_cleanup_at: i64,
    /// 用量缓存（内存字段；`None` 时 `get_usage` 现算并回填）。
    cached_usage: Option<DataUsageReport>,
    /// 进行中的 purge 域（ipc/data.ts:16 `purgeInFlight` 护栏：
    /// select → batch 非原子，并发两次 execute 会让统计重复计数）。
    purge_in_flight: HashSet<String>,
}

impl DataManagementService {
    /// 构造门面；`disk_path` 为数据目录路径（对齐 TS `db.path`，可空）。
    pub fn new(disk_path: Option<String>) -> Self {
        Self {
            disk_path,
            running: false,
            last_auto_cleanup_at: 0,
            cached_usage: None,
            purge_in_flight: HashSet::new(),
        }
    }

    /// `start()`：幂等置运行标记。实际 1h 调度由宿主负责。
    pub fn start(&mut self) {
        self.running = true;
    }

    /// `stop()`：幂等清除运行标记。
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// `isRunning()`。
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// 上次自动清理时间（ms；初值 0）。
    pub fn last_auto_cleanup_at(&self) -> i64 {
        self.last_auto_cleanup_at
    }

    /// `tick`（service.ts:40-51）：到时自动清理 + 采样用量缓存（供快读）。
    ///
    /// 1. `now - lastAutoCleanupAt >= 24h` → 执行 L1 清理并记录时间；
    ///    删除总数 > 0 则先置缓存失效（随本次 tick 重新采样）；
    /// 2. 无条件重采样用量缓存。
    pub fn tick<S: StorageBackend>(&mut self, storage: &mut S, now_ms: i64) -> Result<()> {
        if now_ms - self.last_auto_cleanup_at >= AUTO_CLEANUP_MIN_INTERVAL_MS {
            let result = run_auto_cleanup(storage, now_ms);
            self.last_auto_cleanup_at = now_ms;
            if result.total_deleted() > 0 {
                self.cached_usage = None;
            }
        }
        self.cached_usage = Some(collect_data_usage(
            storage,
            self.disk_path.as_deref(),
            now_ms,
        )?);
        Ok(())
    }

    /// `runCleanupNow`（service.ts:54-59）：立即执行 L1 自动清理（"立即清理"入口）。
    ///
    /// 坑 #12 如实复刻：清理后缓存置 `None`，**不立即重采样**——下次 `get_usage` 现算。
    pub fn run_cleanup_now<S: StorageBackend>(
        &mut self,
        storage: &mut S,
        now_ms: i64,
    ) -> AutoCleanupResult {
        let result = run_auto_cleanup(storage, now_ms);
        self.last_auto_cleanup_at = now_ms;
        self.cached_usage = None;
        result
    }

    /// `getUsage`（service.ts:62-67）：缓存优先，`None` 时现算并回填。
    pub fn get_usage<S: StorageBackend>(
        &mut self,
        storage: &S,
        now_ms: i64,
    ) -> Result<DataUsageReport> {
        if let Some(cached) = &self.cached_usage {
            return Ok(cached.clone());
        }
        let report = collect_data_usage(storage, self.disk_path.as_deref(), now_ms)?;
        self.cached_usage = Some(report.clone());
        Ok(report)
    }

    /// `invalidateUsage`（service.ts:73-75）：仅置 `None`，供绕过门面的写路径
    /// （如直调 [`purge_domain_docs`]）调用。
    pub fn invalidate_usage(&mut self) {
        self.cached_usage = None;
    }

    /// `data-purge-preview` 核心（ipc/data.ts:73-86）：预览清理影响面。
    ///
    /// 坑 #7 如实复刻：**preview 不鉴权管理员**——任何组织成员可预览影响面，
    /// `isCurrentUserAdmin` 仅作为返回字段供 UI 决定是否放行下一步（org 解析
    /// 与管理员字段由调用方负责）。
    pub fn preview_purge<S: StorageBackend>(
        &self,
        storage: &S,
        domain: &str,
        before_ts: i64,
    ) -> Result<PurgePreview> {
        preview_purge_domain_docs(
            storage,
            &PurgeOptions {
                domain: domain.to_string(),
                before_ts,
                collection: None,
            },
        )
    }

    /// `data-purge-execute` 核心（ipc/data.ts:88-126）：校验后执行全域清理。
    ///
    /// 校验顺序固定，任一失败即返回错误：
    /// **管理员 → confirmExported → P2P 启动 → 副本充足 → in-flight**。
    /// （TS 的 `resolveOrg` 由调用方完成，`domain` 为调用方扫描定位的组织数据域。）
    ///
    /// - 坑 #6 如实复刻：`confirm_exported` 仅为调用方传入的布尔确认，
    ///   **无导出事实核验**；
    /// - 坑 #8 如实复刻：execute 恒为全域清理（`collection` 不传），
    ///   模块层的单集合清理能力经本路径不可达；
    /// - 成功后 `invalidate_usage`（purge 直调绕过门面，手动失效用量缓存）；
    ///   in-flight 标记无论成败都移除（对齐 TS `finally`）。
    #[allow(clippy::too_many_arguments)] // 7 个注入参数对齐 IPC handler 形参 + 依赖注入
    pub fn execute_purge<S: StorageBackend>(
        &mut self,
        storage: &mut S,
        domain: &str,
        before_ts: i64,
        confirm_exported: bool,
        is_admin: bool,
        replica: Option<ReplicaStatus>,
        now_ms: i64,
    ) -> Result<PurgeResult> {
        if !is_admin {
            return Err(DataMgmtError::NotOrgAdmin);
        }
        if !confirm_exported {
            return Err(DataMgmtError::ExportNotConfirmed);
        }
        let replica = replica.ok_or(DataMgmtError::P2PNotStarted)?;
        if replica.synced_peers < replica.replica_target {
            return Err(DataMgmtError::ReplicaInsufficient {
                synced: replica.synced_peers,
                target: replica.replica_target,
            });
        }
        if self.purge_in_flight.contains(domain) {
            return Err(DataMgmtError::PurgeInFlight(domain.to_string()));
        }
        self.purge_in_flight.insert(domain.to_string());
        let result = purge_domain_docs(
            storage,
            &PurgeOptions {
                domain: domain.to_string(),
                before_ts,
                collection: None,
            },
            now_ms,
        );
        self.purge_in_flight.remove(domain);
        let result = result?;
        self.invalidate_usage();
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
