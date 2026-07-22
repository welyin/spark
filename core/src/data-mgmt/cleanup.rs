//! L1 级自动清理（cleanup.ts；core/spec/data-mgmt.md §3）。
//!
//! 只清"可重建 / 已终结"的状态，业务文档一律不动。三类对象，共用判定式
//! `now - <时间字段> > <保留期>`（**严格 `>`**，恰好满 90 天不删）：
//!
//! 1. lww 删除标记 tombstone（`meta:*` 值含 `tombstone:true` 且 `ts` 超期）——
//!    收敛依赖存活副本持有的 tombstone：若全网副本都 GC 了同一 tombstone，离线超期
//!    节点重推旧 doc 会因本地 meta 缺失（LWW 判 remote 胜）使文档网络级复活。
//!    90 天保留期即"最大离线窗口"的取舍（cleanup.ts:14-19 注释）；
//! 2. p2p 节点活跃记录（`p2p:peer:record:*` `lastSeenAt` 超期）——纯本地状态；
//! 3. p2p 组织同步记账（`p2p:org-sync-state:*` `lastSyncedAt` 超期）——
//!    K 副本 30 天新鲜窗口早已不计入，删除无感。
//!
//! 坑 #3 如实复刻：JSON 解析失败的行三类过滤器均跳过（损坏行永不被清理）。

use serde::Serialize;
use serde_json::Value;

use crate::org::sync_state::ORG_SYNC_STATE_PREFIX;
use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

use super::constants::{
    ORG_SYNC_STATE_RETENTION_MS, PEER_RECORD_RETENTION_MS, TOMBSTONE_RETENTION_MS,
};
use super::Result;

/// p2p 节点活跃记录前缀（p2p/constants.ts:30 `P2P_PEER_RECORD_PREFIX`；
/// Rust p2p 模块尚未实现，常量暂定义于此）。
pub const P2P_PEER_RECORD_PREFIX: &str = "p2p:peer:record:";

/// 一轮 L1 自动清理的结果（cleanup.ts:25-30）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AutoCleanupResult {
    /// 本轮清理时间（ms；`now` 取一次共用）。
    #[serde(rename = "ranAt")]
    pub ran_at: i64,
    /// 删除的 tombstone 数。
    pub tombstones: u64,
    /// 删除的 p2p 节点记录数。
    #[serde(rename = "peerRecords")]
    pub peer_records: u64,
    /// 删除的组织同步记账数。
    #[serde(rename = "orgSyncStates")]
    pub org_sync_states: u64,
}

impl AutoCleanupResult {
    /// 删除总数。
    pub fn total_deleted(&self) -> u64 {
        self.tombstones + self.peer_records + self.org_sync_states
    }
}

/// `scanRows`（cleanup.ts:37-46）：前缀扫描并逐行 JSON.parse；
/// 解析失败的行 value 置 `None`（三类过滤器均跳过）。
fn scan_rows<S: StorageBackend + ?Sized>(
    storage: &S,
    prefix: &str,
) -> Result<Vec<(String, Option<Value>)>> {
    let rows = storage.scan(&ScanOptions::prefix(prefix))?;
    Ok(rows
        .into_iter()
        .map(|(key, value)| (key, serde_json::from_str::<Value>(&value).ok()))
        .collect())
}

/// JSON number 提取（对齐 TS `typeof x === 'number'`；含浮点）。
fn as_number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// 批量删除；无过期项跳过 batch（cleanup.ts:58-60）。
fn delete_batch<S: StorageBackend>(storage: &mut S, keys: Vec<String>) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    storage.batch(keys.into_iter().map(BatchOperation::delete).collect())?;
    Ok(())
}

/// `cleanupTombstones`（cleanup.ts:49-62）：清理过期 tombstone，返回删除数量。
fn cleanup_tombstones<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<u64> {
    let rows = scan_rows(storage, "meta:")?;
    let expired: Vec<String> = rows
        .into_iter()
        .filter(|(_, value)| {
            let Some(value) = value else { return false };
            value.get("tombstone") == Some(&Value::Bool(true))
                && as_number(value.get("ts"))
                    .is_some_and(|ts| now_ms as f64 - ts > TOMBSTONE_RETENTION_MS as f64)
        })
        .map(|(key, _)| key)
        .collect();
    let count = expired.len() as u64;
    delete_batch(storage, expired)?;
    Ok(count)
}

/// `cleanupPeerRecords`（cleanup.ts:65-77）：清理过期 p2p 节点活跃记录。
fn cleanup_peer_records<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<u64> {
    let rows = scan_rows(storage, P2P_PEER_RECORD_PREFIX)?;
    let expired: Vec<String> = rows
        .into_iter()
        .filter(|(_, value)| {
            as_number(value.as_ref().and_then(|v| v.get("lastSeenAt")))
                .is_some_and(|last_seen_at| {
                    now_ms as f64 - last_seen_at > PEER_RECORD_RETENTION_MS as f64
                })
        })
        .map(|(key, _)| key)
        .collect();
    let count = expired.len() as u64;
    delete_batch(storage, expired)?;
    Ok(count)
}

/// `cleanupOrgSyncStates`（cleanup.ts:80-92）：清理过期组织同步记账。
fn cleanup_org_sync_states<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<u64> {
    let rows = scan_rows(storage, ORG_SYNC_STATE_PREFIX)?;
    let expired: Vec<String> = rows
        .into_iter()
        .filter(|(_, value)| {
            as_number(value.as_ref().and_then(|v| v.get("lastSyncedAt")))
                .is_some_and(|last_synced_at| {
                    now_ms as f64 - last_synced_at > ORG_SYNC_STATE_RETENTION_MS as f64
                })
        })
        .map(|(key, _)| key)
        .collect();
    let count = expired.len() as u64;
    delete_batch(storage, expired)?;
    Ok(count)
}

/// `runAutoCleanup`（cleanup.ts:95-119）：执行一轮 L1 自动清理。
///
/// `now_ms` 取一次三类共用。三类各自独立容错：单类失败仅告警
/// （对齐 TS try/catch + `console.warn`），不影响其余类别；失败类别计数为 0。
pub fn run_auto_cleanup<S: StorageBackend>(storage: &mut S, now_ms: i64) -> AutoCleanupResult {
    let mut result = AutoCleanupResult { ran_at: now_ms, ..AutoCleanupResult::default() };

    match cleanup_tombstones(storage, now_ms) {
        Ok(n) => result.tombstones = n,
        Err(error) => eprintln!("[data-management] tombstone cleanup failed: {error}"),
    }
    match cleanup_peer_records(storage, now_ms) {
        Ok(n) => result.peer_records = n,
        Err(error) => eprintln!("[data-management] peer record cleanup failed: {error}"),
    }
    match cleanup_org_sync_states(storage, now_ms) {
        Ok(n) => result.org_sync_states = n,
        Err(error) => eprintln!("[data-management] org sync-state cleanup failed: {error}"),
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{MemoryStorage, StorageError};

    const NOW: i64 = 100_000_000_000;

    fn tombstone_json(ts: i64) -> String {
        format!("{{\"vv\":{{\"n1\":1}},\"ts\":{ts},\"tombstone\":true}}")
    }

    #[test]
    fn tombstone_cleanup_strict_boundary_and_filters() {
        let mut s = MemoryStorage::new();
        // now - ts > 90d（超期 1ms）→ 删
        s.put("meta:d:c:expired", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS - 1))
            .unwrap();
        // now - ts == 90d（恰好满保留期，严格 > 不删）→ 留
        s.put("meta:d:c:exact", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS))
            .unwrap();
        // now - ts < 90d → 留
        s.put("meta:d:c:fresh", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS + 1))
            .unwrap();
        // 非 tombstone → 留
        s.put("meta:d:c:live", "{\"vv\":{\"n1\":1},\"ts\":1}").unwrap();
        // tombstone 非 true（false）→ 留
        s.put("meta:d:c:false", &format!("{{\"ts\":{},\"tombstone\":false}}", NOW - TOMBSTONE_RETENTION_MS - 1))
            .unwrap();
        // ts 非 number → 留
        s.put("meta:d:c:strts", "{\"ts\":\"old\",\"tombstone\":true}").unwrap();
        // ts 缺失 → 留
        s.put("meta:d:c:nots", "{\"tombstone\":true}").unwrap();
        // 损坏 JSON → 留（坑 #3：损坏行永不清理）
        s.put("meta:d:c:broken", "not json").unwrap();

        assert_eq!(cleanup_tombstones(&mut s, NOW).unwrap(), 1);
        assert!(s.get("meta:d:c:expired").unwrap().is_none());
        for kept in ["exact", "fresh", "live", "false", "strts", "nots", "broken"] {
            assert!(s.get(&format!("meta:d:c:{kept}")).unwrap().is_some(), "{kept} should be kept");
        }
    }

    #[test]
    fn peer_record_cleanup_boundaries() {
        let mut s = MemoryStorage::new();
        let old = NOW - PEER_RECORD_RETENTION_MS - 1;
        s.put("p2p:peer:record:old", &format!("{{\"peerId\":\"old\",\"lastSeenAt\":{old}}}"))
            .unwrap();
        s.put(
            "p2p:peer:record:exact",
            &format!("{{\"lastSeenAt\":{}}}", NOW - PEER_RECORD_RETENTION_MS),
        )
        .unwrap();
        s.put("p2p:peer:record:fresh", &format!("{{\"lastSeenAt\":{NOW}}}"))
            .unwrap();
        // lastSeenAt 非 number → 留
        s.put("p2p:peer:record:str", "{\"lastSeenAt\":\"x\"}").unwrap();
        // 损坏 JSON → 留
        s.put("p2p:peer:record:broken", "{").unwrap();
        // 非 record 前缀的 p2p key 不在扫描范围
        s.put("p2p:other:x", "{\"lastSeenAt\":1}").unwrap();

        assert_eq!(cleanup_peer_records(&mut s, NOW).unwrap(), 1);
        assert!(s.get("p2p:peer:record:old").unwrap().is_none());
        assert!(s.get("p2p:peer:record:exact").unwrap().is_some());
        assert!(s.get("p2p:peer:record:fresh").unwrap().is_some());
        assert!(s.get("p2p:peer:record:str").unwrap().is_some());
        assert!(s.get("p2p:peer:record:broken").unwrap().is_some());
        assert!(s.get("p2p:other:x").unwrap().is_some());
    }

    #[test]
    fn org_sync_state_cleanup_boundaries() {
        let mut s = MemoryStorage::new();
        let old = NOW - ORG_SYNC_STATE_RETENTION_MS - 1;
        s.put(
            "p2p:org-sync-state:p1:o1",
            &format!("{{\"versions\":{{\"summaryVersion\":1}},\"lastSyncedAt\":{old}}}"),
        )
        .unwrap();
        s.put(
            "p2p:org-sync-state:p1:o2",
            &format!("{{\"lastSyncedAt\":{}}}", NOW - ORG_SYNC_STATE_RETENTION_MS),
        )
        .unwrap();
        s.put("p2p:org-sync-state:p1:o3", "{\"lastSyncedAt\":\"x\"}").unwrap();

        assert_eq!(cleanup_org_sync_states(&mut s, NOW).unwrap(), 1);
        assert!(s.get("p2p:org-sync-state:p1:o1").unwrap().is_none());
        assert!(s.get("p2p:org-sync-state:p1:o2").unwrap().is_some());
        assert!(s.get("p2p:org-sync-state:p1:o3").unwrap().is_some());
    }

    #[test]
    fn run_auto_cleanup_all_categories_shared_now() {
        let mut s = MemoryStorage::new();
        s.put("meta:d:c:t", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS - 1))
            .unwrap();
        s.put(
            "p2p:peer:record:p",
            &format!("{{\"lastSeenAt\":{}}}", NOW - PEER_RECORD_RETENTION_MS - 1),
        )
        .unwrap();
        s.put(
            "p2p:org-sync-state:p:o",
            &format!("{{\"lastSyncedAt\":{}}}", NOW - ORG_SYNC_STATE_RETENTION_MS - 1),
        )
        .unwrap();

        let result = run_auto_cleanup(&mut s, NOW);
        assert_eq!(
            result,
            AutoCleanupResult { ran_at: NOW, tombstones: 1, peer_records: 1, org_sync_states: 1 }
        );
        assert_eq!(result.total_deleted(), 3);
        assert_eq!(s.len(), 0);
    }

    /// 指定前缀扫描失败、并统计 batch 调用次数的 fixture。
    struct FlakyStorage {
        inner: MemoryStorage,
        fail_scan_prefix: Option<String>,
        batch_calls: usize,
    }

    impl FlakyStorage {
        fn failing_on(prefix: &str) -> Self {
            Self {
                inner: MemoryStorage::new(),
                fail_scan_prefix: Some(prefix.to_string()),
                batch_calls: 0,
            }
        }
    }

    impl StorageBackend for FlakyStorage {
        fn get(&self, key: &str) -> crate::storage::Result<Option<String>> {
            self.inner.get(key)
        }
        fn put(&mut self, key: &str, value: &str) -> crate::storage::Result<()> {
            self.inner.put(key, value)
        }
        fn delete(&mut self, key: &str) -> crate::storage::Result<()> {
            self.inner.delete(key)
        }
        fn batch(&mut self, operations: Vec<BatchOperation>) -> crate::storage::Result<()> {
            self.batch_calls += 1;
            self.inner.batch(operations)
        }
        fn scan(&self, options: &ScanOptions) -> crate::storage::Result<Vec<(String, String)>> {
            if self.fail_scan_prefix.as_deref() == Some(options.prefix.as_str()) {
                return Err(StorageError::Backend("injected scan failure".to_string()));
            }
            self.inner.scan(options)
        }
    }

    #[test]
    fn single_category_failure_does_not_affect_others() {
        let mut s = FlakyStorage::failing_on("meta:");
        s.inner
            .put("meta:d:c:t", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS - 1))
            .unwrap();
        s.inner
            .put(
                "p2p:peer:record:p",
                &format!("{{\"lastSeenAt\":{}}}", NOW - PEER_RECORD_RETENTION_MS - 1),
            )
            .unwrap();

        let result = run_auto_cleanup(&mut s, NOW);
        // tombstone 扫描失败 → 0，其余类别照常
        assert_eq!(result.tombstones, 0);
        assert_eq!(result.peer_records, 1);
        assert_eq!(result.org_sync_states, 0);
        assert!(s.inner.get("meta:d:c:t").unwrap().is_some());
        assert!(s.inner.get("p2p:peer:record:p").unwrap().is_none());
    }

    #[test]
    fn empty_category_skips_batch() {
        let mut s = FlakyStorage::failing_on("__never__");
        // 仅 tombstone 有过期项 → 只触发 1 次 batch
        s.inner
            .put("meta:d:c:t", &tombstone_json(NOW - TOMBSTONE_RETENTION_MS - 1))
            .unwrap();
        let result = run_auto_cleanup(&mut s, NOW);
        assert_eq!(result.total_deleted(), 1);
        assert_eq!(s.batch_calls, 1);

        // 全部新鲜 → 0 次 batch
        let mut s2 = FlakyStorage::failing_on("__never__");
        s2.inner.put("meta:d:c:f", &tombstone_json(NOW)).unwrap();
        let result2 = run_auto_cleanup(&mut s2, NOW);
        assert_eq!(result2.total_deleted(), 0);
        assert_eq!(s2.batch_calls, 0);
    }
}
