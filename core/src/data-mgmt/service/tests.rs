//! 数据自动管理门面单测。

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
    s.put(
        "meta:d:c:old",
        &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1),
    )
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
    s.put(
        "meta:d:c:old",
        &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1),
    )
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
    s.put(
        "meta:d:c:old",
        &tombstone_json(NOW - 90 * 24 * 60 * 60 * 1000 - 1),
    )
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
    Some(ReplicaStatus {
        synced_peers: 3,
        replica_target: 3,
    })
}

#[test]
fn execute_purge_rejection_order() {
    let domain = "plugin:app";
    // 1. 非管理员：其余条件全齐也拒绝，且消息对齐 TS
    let err = service()
        .execute_purge(
            &mut purge_fixture(),
            domain,
            200,
            true,
            false,
            replica_ok(),
            NOW,
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Only organization admins can purge historical data"
    );

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
            Some(ReplicaStatus {
                synced_peers: 2,
                replica_target: 3,
            }),
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
        .execute_purge(
            &mut purge_fixture(),
            domain,
            200,
            true,
            true,
            replica_ok(),
            NOW,
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "A purge for domain plugin:app is already running; wait for it to finish"
    );

    // 6. 非 plugin 域的拒绝（模块层校验）在全部 IPC 校验之后
    let err = service()
        .execute_purge(
            &mut purge_fixture(),
            "chat",
            200,
            true,
            true,
            replica_ok(),
            NOW,
        )
        .unwrap_err();
    assert!(matches!(err, DataMgmtError::NonPluginDomain(_)));
    // 空域（组织无插件文档时 resolveOrg 定位为 ""）同样按非插件域拒绝；
    // kernel 单节点路径被副本护栏（K=3）先行拦截，此处直接覆盖模块层行为
    let err = service()
        .execute_purge(&mut purge_fixture(), "", 200, true, true, replica_ok(), NOW)
        .unwrap_err();
    assert!(matches!(err, DataMgmtError::NonPluginDomain(_)));
    assert_eq!(
        err.to_string(),
        "Refused to purge non-plugin domain \"\": only plugin domains can be purged"
    );
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
        get_purge_watermark(&s, "plugin:app", "c")
            .unwrap()
            .unwrap()
            .purged_before,
        200
    );
    assert!(
        s.get(&format!("doc:system:purge-log:{NOW}"))
            .unwrap()
            .is_some()
    );
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
    let mut s = FailBatchStorage {
        inner: purge_fixture(),
    };
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
