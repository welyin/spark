//! 驱逐选择器与执行 + 无引用 GC 机制（A2，协议 §15.4–15.5）。
//!
//! **位次规则**（并发收敛的核心，§15.4）：完整持有者按 deviceUid 升序，
//! 位次 `rank ≥ K` 的持有者是富余副本——各自独立得出「我该驱逐」，无需
//! 时序协调即收敛到 K 副本；`rank < K` 或账本未覆盖本机 → 永不驱逐。
//! 两条硬规则在选择器内强制，不靠调用方自觉：
//! 1. `|H| ≤ K` 永不驱逐（驱逐不杀最后副本）；
//! 2. 本机位次 `rank < K` 永不驱逐（即使自身超配额）。
//!
//! GC（§15.5）：无引用 chunk 清理不占驱逐通道、不受硬规则约束（记录已删
//! 则副本语义随记录生命周期结束）。**引用收集器的完备性依赖 A4**（当前
//! 无任何记录引用 blob 层 cid，空引用集会误清全部——故 A2 只交付机制，
//! 不挂周期触发）。

use serde_json::json;

use super::quota::{self, blob_access_key};
use super::presence::{bitmap_decode, list_presence};
use super::{SyncResult, blob_meta_key, drop_local_chunks, held_chunk_indices};
use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

/// 驱逐节流键（本地键，十进制 ASCII ms）。
pub const EVICT_LAST_KEY: &str = "blob:evict:last";
/// 驱逐检查节流间隔（周期触发点挂在 pdsync hello 调和，P6 gc_blobs 同位置）。
pub const EVICT_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// 驱逐标记键（`blob:evicted:{cid}`；本地键，协议 §16.4）：配额驱逐设置，
/// P6 调和据此停手（防驱逐—重拉死循环）；显式读取/回补/重写解除。
pub fn blob_evicted_key(cid: &str) -> String {
    format!("blob:evicted:{cid}")
}

/// 是否带驱逐标记（P6 `missing_blobs` 跳过依据）。
pub fn is_evicted<S: StorageBackend>(storage: &S, cid: &str) -> bool {
    storage
        .get(&blob_evicted_key(cid))
        .ok()
        .flatten()
        .is_some()
}

/// 置驱逐标记（仅配额驱逐路径调用）。
pub fn mark_evicted<S: StorageBackend>(storage: &mut S, cid: &str) -> SyncResult<()> {
    storage.put(&blob_evicted_key(cid), "1")?;
    Ok(())
}

/// 解除驱逐标记（显式意图：mark_want/save_blob/回补落块/GC 清理）。
pub fn clear_evicted<S: StorageBackend>(storage: &mut S, cid: &str) -> SyncResult<()> {
    storage.delete(&blob_evicted_key(cid))?;
    Ok(())
}

// ── 选择器（纯逻辑，向量锁定）──────────────────────────────────────

/// 一个本机完整持有 blob 的驱逐判定输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionState {
    /// blob cid。
    pub cid: String,
    /// 字节数（manifest.size，精确）。
    pub bytes: u64,
    /// 最近访问 ms（无记录按 0=最老）。
    pub access_ts: i64,
    /// 完整持有者 deviceUid 列表（**升序**，位次规则的输入）。
    pub holders: Vec<String>,
}

/// 本机在完整持有者集合中的位次（不在集合 → None）。
pub fn holder_rank(holders: &[String], my_uid: &str) -> Option<usize> {
    holders.iter().position(|u| u == my_uid)
}

/// 驱逐选择器：返回按驱逐顺序排列的 cid 列表（累计 bytes ≥ `needed_bytes`
/// 即停；`needed_bytes == 0` 直接返回空）。
///
/// 候选过滤（两条硬规则）：完整持有者数 `|H| > K` 且本机位次
/// `rank >= K`；排序 `access_ts` 升序、同值 cid 字典序。纯函数——
/// 任何设备凭相同输入算出相同结果（§15.4 向量锁定）。
pub fn plan_eviction(
    states: &[EvictionState],
    my_uid: &str,
    k: usize,
    needed_bytes: u64,
) -> Vec<String> {
    if needed_bytes == 0 {
        return Vec::new();
    }
    let mut candidates: Vec<&EvictionState> = states
        .iter()
        .filter(|s| {
            // 硬规则 1：副本保护；硬规则 2：本机必须是富余副本
            s.holders.len() > k && holder_rank(&s.holders, my_uid).is_some_and(|r| r >= k)
        })
        .collect();
    candidates.sort_by(|a, b| a.access_ts.cmp(&b.access_ts).then_with(|| a.cid.cmp(&b.cid)));
    let mut out = Vec::new();
    let mut freed = 0u64;
    for s in candidates {
        out.push(s.cid.clone());
        freed += s.bytes;
        if freed >= needed_bytes {
            break;
        }
    }
    out
}

// ── 执行 ───────────────────────────────────────────────────────────

/// 收集本机驱逐判定输入：本机**完整持有**（全部 chunk 在库）的 blob；
/// 部分持有是回补瞬时态，不做候选（§15.4）。
pub fn collect_eviction_states<S: StorageBackend>(
    storage: &S,
    my_uid: &str,
) -> SyncResult<Vec<EvictionState>> {
    let mut states = Vec::new();
    for (_key, raw) in storage.scan(&ScanOptions::prefix("blob:meta:"))? {
        let Some(manifest) = super::BlobManifest::from_json(&raw) else {
            continue;
        };
        let held = held_chunk_indices(storage, &manifest)?;
        if held.iter().any(|h| !h) {
            continue; // 部分持有：回补瞬时态，不做候选
        }
        let records = list_presence(storage, &manifest.cid)?;
        let mut holders: Vec<String> = records
            .iter()
            .filter(|r| {
                bitmap_decode(&r.chunks).is_some_and(|d| {
                    manifest.chunk_count()
                        == d.iter().take(manifest.chunk_count()).filter(|h| **h).count()
                })
            })
            .map(|r| r.device_uid.clone())
            .collect();
        holders.sort();
        // 空 blob（0 块）：凡有 presence 记录即完整持有（同 full_replica_count 口径）
        if manifest.chunk_count() == 0 {
            holders = records.iter().map(|r| r.device_uid.clone()).collect();
            holders.sort();
        }
        // 本机 presence 未落账（异常态）时按本机完整持有补入——
        // 位次判定宁可保守（rank 可能变小 → 更不倾向驱逐）
        if !holders.iter().any(|u| u == my_uid) {
            holders.push(my_uid.to_string());
            holders.sort();
        }
        let access_ts = quota::get_access(storage, &manifest.cid)?.unwrap_or(0);
        states.push(EvictionState {
            cid: manifest.cid.clone(),
            bytes: manifest.size,
            access_ts,
            holders,
        });
    }
    Ok(states)
}

/// 驱逐执行报告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionReport {
    /// 生效配额（字节）。
    pub quota_bytes: u64,
    /// 执行前水位。
    pub used_before: u64,
    /// 执行后水位。
    pub used_after: u64,
    /// 实际驱逐（cid, 释放字节数）。
    pub evicted: Vec<(String, u64)>,
    /// 水位仍超配额（候选耗尽/受硬规则保护无法凑够——诚实上报）。
    pub still_over: bool,
}

/// 超配额驱逐：水位 → 候选 → 位次规则选择 → 逐个 `drop_local_chunks`
/// （只删 chunk 留 manifest，presence 全零墓碑化随 dlog 传播）。
/// 未超配额直接返回空报告。
pub fn evict_over_quota<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    now_ms: i64,
) -> SyncResult<EvictionReport> {
    let status = quota::quota_status(storage)?;
    let mut report = EvictionReport {
        quota_bytes: status.quota_bytes,
        used_before: status.used_bytes,
        used_after: status.used_bytes,
        evicted: Vec::new(),
        still_over: status.over_bytes > 0,
    };
    if status.over_bytes == 0 {
        return Ok(report);
    }
    let k = quota::k_target(quota::active_device_count(storage));
    let states = collect_eviction_states(storage, device_uid)?;
    let plan = plan_eviction(&states, device_uid, k, status.over_bytes);
    for cid in &plan {
        let bytes = states
            .iter()
            .find(|s| &s.cid == cid)
            .map(|s| s.bytes)
            .unwrap_or(0);
        drop_local_chunks(storage, node_id, device_uid, cid, now_ms)?;
        storage.delete(&blob_access_key(cid))?;
        // 驱逐标记：P6 调和停手（协议 §16.4 防驱逐—重拉死循环）
        mark_evicted(storage, cid)?;
        report.evicted.push((cid.clone(), bytes));
    }
    report.used_after = quota::blob_usage(storage)?;
    report.still_over = report.used_after > report.quota_bytes;
    Ok(report)
}

/// 节流判定：距上次驱逐检查不足 [`EVICT_INTERVAL_MS`] → false。
/// 通过则记录本次时间。
pub fn throttle_evict<S: StorageBackend>(storage: &mut S, now_ms: i64) -> SyncResult<bool> {
    if let Some(raw) = storage.get(EVICT_LAST_KEY)?
        && let Ok(last) = raw.parse::<i64>()
        && now_ms - last < EVICT_INTERVAL_MS
    {
        return Ok(false);
    }
    storage.put(EVICT_LAST_KEY, &now_ms.to_string())?;
    Ok(true)
}

// ── 无引用 GC（机制；周期接线待 A4，见模块头注）────────────────────

/// GC 规划（纯函数）：本机持有 − 被引用 = 无引用清理集（确定性升序）。
pub fn plan_gc(held: &[String], referenced: &[String]) -> Vec<String> {
    let mut out: Vec<String> = held
        .iter()
        .filter(|cid| !referenced.contains(cid))
        .cloned()
        .collect();
    out.sort();
    out
}

/// 本机有 manifest 的全部 blob cid（升序）。
pub fn list_local_blobs<S: StorageBackend>(storage: &S) -> SyncResult<Vec<String>> {
    let mut out = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix("blob:meta:"))? {
        if super::BlobManifest::from_json(&raw).is_some() {
            out.push(key.trim_start_matches("blob:meta:").to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// 无引用 GC 执行：对 `referenced` 之外的每个本机 blob 删除 chunk +
/// manifest + access 键，presence 墓碑化（随 dlog 传播）。
///
/// **不受驱逐硬规则约束**（§15.5：记录已删则副本语义随记录生命周期
/// 结束）；**不占驱逐通道**（独立入口）。
///
/// 调用方必须保证 `referenced` 完备（覆盖全部引用形态）——当前引用
/// 形态由 A4 定义，故本函数在 A4 前不应接周期触发（机制 + 单测交付）。
pub fn gc_unreferenced<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    referenced: &[String],
    now_ms: i64,
) -> SyncResult<Vec<String>> {
    let held = list_local_blobs(storage)?;
    let plan = plan_gc(&held, referenced);
    for cid in &plan {
        drop_local_chunks(storage, node_id, device_uid, cid, now_ms)?;
        storage.batch(vec![
            BatchOperation::delete(blob_meta_key(cid)),
            BatchOperation::delete(blob_access_key(cid)),
            BatchOperation::delete(blob_evicted_key(cid)),
        ])?;
    }
    Ok(plan)
}

/// 供日志/健康度展示的报告形态（serde camelCase，未接线，备 A3）。
#[allow(dead_code)]
pub fn report_to_json(report: &EvictionReport) -> serde_json::Value {
    json!({
        "quotaBytes": report.quota_bytes,
        "usedBefore": report.used_before,
        "usedAfter": report.used_after,
        "evicted": report.evicted,
        "stillOver": report.still_over,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn state(cid: &str, bytes: u64, access: i64, holders: &[&str]) -> EvictionState {
        EvictionState {
            cid: cid.to_string(),
            bytes,
            access_ts: access,
            holders: holders.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn plan_eviction_hard_rules_and_order() {
        let holders4 = ["uid-a", "uid-b", "uid-c", "uid-d"];
        let states = vec![
            state("c-new", 100, 3000, &holders4),
            state("a-old", 500, 1000, &holders4),
            state("b-mid", 200, 2000, &holders4),
        ];
        // rank 3（uid-d 富余），K=3，需要 600：按龄 a-old(500) + b-mid(200) 凑够
        let plan = plan_eviction(&states, "uid-d", 3, 600);
        assert_eq!(plan, vec!["a-old", "b-mid"]);
        // 需要 100：只取最老一个
        let plan = plan_eviction(&states, "uid-d", 3, 100);
        assert_eq!(plan, vec!["a-old"]);
        // rank 0（保留位）：无候选
        assert!(plan_eviction(&states, "uid-a", 3, 9999).is_empty());
        // 副本 =K：无候选
        let states_k = vec![state("x", 100, 0, &["uid-a", "uid-b", "uid-c"])];
        assert!(plan_eviction(&states_k, "uid-c", 3, 1).is_empty());
        // 账本不含本机：无候选（保守）
        assert!(plan_eviction(&states, "uid-zz", 3, 1).is_empty());
        // needed == 0
        assert!(plan_eviction(&states, "uid-d", 3, 0).is_empty());
        // 同龄按 cid 字典序
        let tie = vec![
            state("b-tie", 100, 1000, &holders4),
            state("a-tie", 100, 1000, &holders4),
        ];
        assert_eq!(plan_eviction(&tie, "uid-d", 3, 200), vec!["a-tie", "b-tie"]);
    }

    /// 三设备（K=3 退化全量）驱逐恒无操作；四设备位次规则端到端。
    #[test]
    fn evict_over_quota_end_to_end() {
        let mut s = MemoryStorage::new();
        // 本机 uid-d；域内 4 台未撤销设备 → K=3
        for (peer, uid) in [
            ("peer-a", "uid-a"),
            ("peer-b", "uid-b"),
            ("peer-c", "uid-c"),
            ("peer-d", "uid-d"),
        ] {
            s.put(
                &format!("device:{peer}"),
                &serde_json::to_string(&device_record(peer, uid)).unwrap(),
            )
            .unwrap();
        }
        let data = pattern(500_000);
        let m = super::super::save_blob(&mut s, "node-d", "uid-d", &data, 1000).unwrap();
        // 账本：a/b/c/d 四台完整持有
        for uid in ["uid-a", "uid-b", "uid-c"] {
            s.put(
                &super::super::presence::presence_key(&m.cid, uid),
                &super::super::presence::PresenceRecord {
                    v: 1,
                    cid: m.cid.clone(),
                    device_uid: uid.to_string(),
                    chunks: "AQ==".to_string(),
                }
                .to_json(),
            )
            .unwrap();
        }
        // 配额 0 → 全部待释放；本机 uid-d rank 3 = 富余 → 驱逐
        super::quota::set_blob_quota(&mut s, Some(0)).unwrap();
        let report = evict_over_quota(&mut s, "node-d", "uid-d", 2000).unwrap();
        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].0, m.cid);
        assert_eq!(report.used_after, 0);
        assert!(!report.still_over);
        assert!(
            super::super::get_manifest(&s, &m.cid).unwrap().is_some(),
            "manifest 不动"
        );
        assert!(
            super::quota::get_access(&s, &m.cid).unwrap().is_none(),
            "access 键随驱逐删除"
        );
    }

    fn device_record(peer: &str, uid: &str) -> crate::device::DeviceRecord {
        crate::device::DeviceRecord {
            peer_id: peer.to_string(),
            device_uid: Some(uid.to_string()),
            device_name: peer.to_string(),
            os: "Windows".to_string(),
            arch: "x86_64".to_string(),
            macs: Vec::new(),
            app_version: String::new(),
            os_version: String::new(),
            updated_at: 1000,
            last_seen_at: 1000,
            revoked_at: None,
            device_pub_key: None,
        }
    }

    #[test]
    fn keeper_rank_never_evicts_even_over_quota() {
        let mut s = MemoryStorage::new();
        for (peer, uid) in [
            ("peer-a", "uid-a"),
            ("peer-b", "uid-b"),
            ("peer-c", "uid-c"),
            ("peer-d", "uid-d"),
        ] {
            s.put(
                &format!("device:{peer}"),
                &serde_json::to_string(&device_record(peer, uid)).unwrap(),
            )
            .unwrap();
        }
        let data = pattern(1000);
        let m = super::super::save_blob(&mut s, "node-a", "uid-a", &data, 1000).unwrap();
        for uid in ["uid-b", "uid-c", "uid-d"] {
            s.put(
                &super::super::presence::presence_key(&m.cid, uid),
                &super::super::presence::PresenceRecord {
                    v: 1,
                    cid: m.cid.clone(),
                    device_uid: uid.to_string(),
                    chunks: "AQ==".to_string(),
                }
                .to_json(),
            )
            .unwrap();
        }
        super::quota::set_blob_quota(&mut s, Some(0)).unwrap();
        // 本机 uid-a rank 0：超配额也绝不驱逐（硬规则 2），诚实上报 still_over
        let report = evict_over_quota(&mut s, "node-a", "uid-a", 2000).unwrap();
        assert!(report.evicted.is_empty());
        assert!(report.still_over);
        assert!(super::super::has_chunk(&s, &m.chunk_cids[0]));
    }

    #[test]
    fn gc_clears_unreferenced_and_keeps_referenced() {
        let mut s = MemoryStorage::new();
        let d1 = pattern(1000);
        let d2 = pattern(2000);
        let m1 = super::super::save_blob(&mut s, "node-a", "uid-a", &d1, 1000).unwrap();
        let m2 = super::super::save_blob(&mut s, "node-a", "uid-a", &d2, 1001).unwrap();
        let plan = gc_unreferenced(&mut s, "node-a", "uid-a", &[m1.cid.clone()], 3000).unwrap();
        assert_eq!(plan, vec![m2.cid.clone()], "无引用的进 GC 集（确定性升序）");
        // m1 保留（被引用）；m2 全清（chunk + manifest + access + presence 墓碑）
        assert!(super::super::read_blob(&mut s, &m1.cid, 4000).unwrap().is_some());
        assert!(super::super::get_manifest(&s, &m2.cid).unwrap().is_none());
        assert!(super::quota::get_access(&s, &m2.cid).unwrap().is_none());
        assert!(
            super::super::list_presence(&s, &m2.cid).unwrap().is_empty(),
            "presence 已墓碑化移除"
        );
        // GC 不受「不杀最后副本」约束：m2 是唯一副本也被清（记录已删语义）
        // plan_gc 纯函数：held − referenced，升序
        assert_eq!(
            plan_gc(
                &["b".to_string(), "a".to_string()],
                &["a".to_string()]
            ),
            vec!["b".to_string()]
        );
    }
}
