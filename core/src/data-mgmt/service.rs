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
        self.cached_usage = Some(collect_data_usage(storage, self.disk_path.as_deref(), now_ms)?);
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
            &PurgeOptions { domain: domain.to_string(), before_ts, collection: None },
        )
    }

    /// `data-purge-execute` 核心（ipc/data.ts:88-126）：校验后执行全域清理。
    ///
    /// 校验顺序固定，任一失败即返回错误：
    /// **管理员 → confirmExported → P2P 启动 → 副本充足 → in-flight**。
    /// （TS 的 `resolveOrg` 由调用方完成，`domain` 即 `org.basePluginDomain`。）
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
            &PurgeOptions { domain: domain.to_string(), before_ts, collection: None },
            now_ms,
        );
        self.purge_in_flight.remove(domain);
        let result = result?;
        self.invalidate_usage();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_mgmt::watermark::get_purge_watermark;
    use crate::storage::{BatchOperation, MemoryStorage, ScanOptions, StorageError};

    const NOW: i64 = 100_000_000_000; // > 24h，首个 tick 必然触发清理

    fn tombstone_json(ts: i64) -> String {
        format!("{{\"ts\":{ts},\"tombstone\":true}}")
    }

    fn service() -> DataManagementService {
        DataManagementService::new(None)
    }

    #[test]
    fn start_stop_idempotent() {
        let mut svc = service();
        assert!(!svc.is_running());
        svc.start();
        assert!(svc.is_running());
        svc.start();
        assert!(svc.is_running());
        svc.stop();
        assert!(!svc.is_running());
        svc.stop();
        assert!(!svc.is_running());
    }

    #[test]
    fn first_tick_always_cleans_and_samples_usage() {
        let mut s = MemoryStorage::new();
        s.put("meta:d:c:old", &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1))
            .unwrap();
        s.put("doc:plugin:a:c:x", "v").unwrap();

        let mut svc = service();
        svc.tick(&mut s, NOW).unwrap();
        // last_auto_cleanup_at 初值 0 → 首个 tick 必然清理（坑 #11）
        assert_eq!(svc.last_auto_cleanup_at(), NOW);
        assert!(s.get("meta:d:c:old").unwrap().is_none());
        // tick 末尾无条件重采样：缓存反映清理后的用量（totalKeys=1）
        let usage = svc.get_usage(&s, NOW + 1).unwrap();
        assert_eq!(usage.total_keys, 1);
        assert_eq!(usage.scanned_at, NOW); // 缓存来自 tick，scanned_at 是 tick 时刻
    }

    #[test]
    fn tick_within_24h_skips_cleanup_but_resamples() {
        let mut s = MemoryStorage::new();
        let mut svc = service();
        svc.tick(&mut s, NOW).unwrap();
        assert_eq!(svc.last_auto_cleanup_at(), NOW);

        // 24h 内再次 tick：不清理（过期数据原样保留），但重采样缓存
        s.put("meta:d:c:old", &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1))
            .unwrap();
        let later = NOW + 60 * 60 * 1000; // +1h
        svc.tick(&mut s, later).unwrap();
        assert_eq!(svc.last_auto_cleanup_at(), NOW); // 未推进
        assert!(s.get("meta:d:c:old").unwrap().is_some()); // 未清理
        let usage = svc.get_usage(&s, later + 1).unwrap();
        assert_eq!(usage.scanned_at, later); // 缓存已重采样
        assert_eq!(usage.total_keys, 1);
    }

    #[test]
    fn run_cleanup_now_invalidates_without_resample() {
        let mut s = MemoryStorage::new();
        s.put("meta:d:c:old", &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1))
            .unwrap();

        let mut svc = service();
        svc.get_usage(&s, NOW).unwrap(); // 建立缓存
        assert!(svc.cached_usage.is_some());

        let result = svc.run_cleanup_now(&mut s, NOW);
        assert_eq!(result.tombstones, 1);
        assert_eq!(svc.last_auto_cleanup_at(), NOW);
        // 坑 #12：不立即重采样，缓存置 None
        assert!(svc.cached_usage.is_none());
        // 下次 get_usage 现算回填（读到的是清理后的现算结果）
        let usage = svc.get_usage(&s, NOW + 1).unwrap();
        assert_eq!(usage.total_keys, 0);
        assert_eq!(usage.scanned_at, NOW + 1);
    }

    #[test]
    fn get_usage_cache_semantics_and_invalidate() {
        let mut s = MemoryStorage::new();
        s.put("a", "1").unwrap();
        let mut svc = service();

        let first = svc.get_usage(&s, NOW).unwrap();
        assert_eq!(first.total_keys, 1);
        // 缓存期间写入新数据：读到的仍是缓存（陈旧）
        s.put("b", "2").unwrap();
        let second = svc.get_usage(&s, NOW + 1).unwrap();
        assert_eq!(second.total_keys, 1);
        assert_eq!(second.scanned_at, NOW);
        // invalidate 后现算
        svc.invalidate_usage();
        let third = svc.get_usage(&s, NOW + 2).unwrap();
        assert_eq!(third.total_keys, 2);
        assert_eq!(third.scanned_at, NOW + 2);
    }

    fn purge_fixture() -> MemoryStorage {
        let mut s = MemoryStorage::new();
        s.put("doc:plugin:app:c:id1", "{}").unwrap();
        s.put("meta:plugin:app:c:id1", "{\"ts\":100}").unwrap();
        s
    }

    fn replica_ok() -> Option<ReplicaStatus> {
        Some(ReplicaStatus { synced_peers: 3, replica_target: 3 })
    }

    #[test]
    fn execute_purge_rejection_order() {
        let domain = "plugin:app";
        // 1. 非管理员：其余条件全齐也拒绝，且消息对齐 TS
        let err = service()
            .execute_purge(&mut purge_fixture(), domain, 200, true, false, replica_ok(), NOW)
            .unwrap_err();
        assert_eq!(err.to_string(), "Only organization admins can purge historical data");

        // 2. confirmExported 未确认（管理员通过；即便 P2P 未启动也先报此错 → 顺序固定）
        let err = service()
            .execute_purge(&mut purge_fixture(), domain, 200, false, true, None, NOW)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Export backup first: confirmExported must be true before purging"
        );

        // 3. P2P 未启动（replica 为 None；即便副本参数不可能充足也先报此错）
        let err = service()
            .execute_purge(&mut purge_fixture(), domain, 200, true, true, None, NOW)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "P2P network is not started; cannot verify replica sufficiency, purge refused"
        );

        // 4. 副本不足（synced < target）
        let err = service()
            .execute_purge(
                &mut purge_fixture(),
                domain,
                200,
                true,
                true,
                Some(ReplicaStatus { synced_peers: 2, replica_target: 3 }),
                NOW,
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Replica insufficient (2/3): purging local copies now may lose organization data. \
             Wait for replicas to replenish or add disk space instead."
        );

        // 5. 同域 in-flight：前序条件全齐，仅护栏拦截
        let mut svc = service();
        svc.purge_in_flight.insert(domain.to_string());
        let err = svc
            .execute_purge(&mut purge_fixture(), domain, 200, true, true, replica_ok(), NOW)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "A purge for domain plugin:app is already running; wait for it to finish"
        );

        // 6. 非 plugin 域的拒绝（模块层校验）在全部 IPC 校验之后
        let err = service()
            .execute_purge(&mut purge_fixture(), "chat", 200, true, true, replica_ok(), NOW)
            .unwrap_err();
        assert!(matches!(err, DataMgmtError::NonPluginDomain(_)));
    }

    #[test]
    fn execute_purge_happy_path_and_invalidate() {
        let mut s = purge_fixture();
        let mut svc = service();
        svc.get_usage(&s, NOW).unwrap(); // 建立缓存
        assert!(svc.cached_usage.is_some());

        let result = svc
            .execute_purge(&mut s, "plugin:app", 200, true, true, replica_ok(), NOW)
            .unwrap();
        assert_eq!(result.removed_docs, 1);
        assert!(s.get("doc:plugin:app:c:id1").unwrap().is_none());
        // 水位线已抬升、审计日志已写
        assert_eq!(
            get_purge_watermark(&s, "plugin:app", "c").unwrap().unwrap().purged_before,
            200
        );
        assert!(s.get(&format!("doc:system:purge-log:{NOW}")).unwrap().is_some());
        // 用量缓存已失效；in-flight 已释放（可再次执行，空 purge 成功返回 0）
        assert!(svc.cached_usage.is_none());
        let again = svc
            .execute_purge(&mut s, "plugin:app", 200, true, true, replica_ok(), NOW + 1)
            .unwrap();
        assert_eq!(again.removed_docs, 0);
    }

    /// batch 恒失败的 fixture：验证 in-flight 在失败路径同样释放（TS finally）。
    struct FailBatchStorage {
        inner: MemoryStorage,
    }

    impl StorageBackend for FailBatchStorage {
        fn get(&self, key: &str) -> crate::storage::Result<Option<String>> {
            self.inner.get(key)
        }
        fn put(&mut self, key: &str, value: &str) -> crate::storage::Result<()> {
            self.inner.put(key, value)
        }
        fn delete(&mut self, key: &str) -> crate::storage::Result<()> {
            self.inner.delete(key)
        }
        fn batch(&mut self, _operations: Vec<BatchOperation>) -> crate::storage::Result<()> {
            Err(StorageError::Backend("injected batch failure".to_string()))
        }
        fn scan(&self, options: &ScanOptions) -> crate::storage::Result<Vec<(String, String)>> {
            self.inner.scan(options)
        }
    }

    #[test]
    fn execute_purge_releases_in_flight_on_failure() {
        let mut s = FailBatchStorage { inner: purge_fixture() };
        let mut svc = service();
        let err = svc
            .execute_purge(&mut s, "plugin:app", 200, true, true, replica_ok(), NOW)
            .unwrap_err();
        assert!(matches!(err, DataMgmtError::Storage(_)));
        // finally 语义：失败后 in-flight 已释放，可重试（报存储错而非护栏错）
        assert!(!svc.purge_in_flight.contains("plugin:app"));
        let err = svc
            .execute_purge(&mut s, "plugin:app", 200, true, true, replica_ok(), NOW)
            .unwrap_err();
        assert!(matches!(err, DataMgmtError::Storage(_)));
    }

    #[test]
    fn preview_purge_no_admin_check() {
        // 坑 #7：preview 不涉及管理员参数，任何成员可预览
        let s = purge_fixture();
        let svc = service();
        let preview = svc.preview_purge(&s, "plugin:app", 200).unwrap();
        assert_eq!(preview.affected_docs, 1);
        assert_eq!(preview.collections, ["c"]);
        // preview 不写：doc/meta 原样保留
        assert!(s.get("doc:plugin:app:c:id1").unwrap().is_some());
    }
}
