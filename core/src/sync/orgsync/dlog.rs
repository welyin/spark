//! orgsync 复制组判定 + org 域删除日志（dlog）辅助与 GC。
//!
//! 复制组判定：all-members（全体成员）/ data-accounts（数据账号集）。
//! org 域 dlog 的 seq 空间按设备维护（架构裁决 B6）：seen/wm 键含 peerId 段，
//! 读写方传入口岸 ctx 的 remote_peer_id。

use std::collections::BTreeMap;

use crate::org::roles::{data_account_set, is_data_account};
use crate::org::OrganizationRecord;
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;
use crate::plugindata::{
    Accounts, org_dlog_entry_prefix, org_dlog_seq_key, org_dlog_wm_key, org_dlog_seen_key,
    org_dlog_wm_prefix,
};

// ── 复制组判定 ───────────────────────────────────────────────────────────

/// 判定 `from` 是否属于集合 `(orgId, collection)` 的复制组。
///
/// - `accounts: all-members` → 全体成员；
/// - `accounts: data-accounts` → 当前数据账号集（含缺省推导）。
///
/// `from` 不在成员表 → false（已在调用方前置校验）。
pub fn is_in_replication_group(
    record: &OrganizationRecord,
    from: &str,
    accounts: Accounts,
) -> bool {
    match accounts {
        Accounts::AllMembers => record.find_member(from).is_some(),
        Accounts::DataAccounts => is_data_account(record, from),
    }
}

/// 获取集合的复制组成员 rootId 列表（用于 GC 等待集合等）。
pub fn replication_group_members(
    record: &OrganizationRecord,
    accounts: Accounts,
) -> Vec<String> {
    match accounts {
        Accounts::AllMembers => record.members.iter().map(|m| m.root_id.clone()).collect(),
        Accounts::DataAccounts => data_account_set(record),
    }
}

// ── org 域 dlog 辅助 ────────────────────────────────────────────────────

/// 读取 org 域 dlog 的已收序号（我对对端**设备**已收讫的日志条目）。
///
/// dlog seq 空间按设备维护（架构裁决）：同 rootId 的每台设备各自确认，
/// seen/wm 键含 peerId 段，读写方传入口岸 ctx 的 remote_peer_id。
pub fn org_dlog_get_seen<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
) -> SyncResult<u64> {
    let key = org_dlog_seen_key(org_id, name, version, root_id, peer_id);
    let raw = storage.get(&key)?;
    Ok(raw.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0))
}

/// 推进 org 域 dlog 已收序号（只增不减；按 (rootId, peerId) 设备粒度）。
pub fn org_dlog_set_seen<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
    seq: u64,
) -> SyncResult<()> {
    let current = org_dlog_get_seen(storage, org_id, name, version, root_id, peer_id)?;
    if seq > current {
        let key = org_dlog_seen_key(org_id, name, version, root_id, peer_id);
        storage.put(&key, &seq.to_string())?;
    }
    Ok(())
}

/// 读取 org 域 dlog 确认水位（按 (rootId, peerId) 设备粒度）。
pub fn org_dlog_get_watermark<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
) -> SyncResult<u64> {
    let key = org_dlog_wm_key(org_id, name, version, root_id, peer_id);
    Ok(storage
        .get(&key)?
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0))
}

/// 推进 org 域 dlog 确认水位（只增不减；按 (rootId, peerId) 设备粒度）。
pub fn org_dlog_set_watermark<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
    ack: u64,
) -> SyncResult<u64> {
    let current = org_dlog_get_watermark(storage, org_id, name, version, root_id, peer_id)?;
    let ack = ack.max(current);
    if ack > current {
        let key = org_dlog_wm_key(org_id, name, version, root_id, peer_id);
        storage.put(&key, &ack.to_string())?;
    }
    Ok(ack)
}

/// 读取 org 域 dlog 当前最大序号。
pub fn org_dlog_current_seq<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> SyncResult<u64> {
    let key = org_dlog_seq_key(org_id, name, version);
    let raw = storage.get(&key)?;
    Ok(raw.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0))
}

/// 追加 org 域删除日志条目。返回 (seq, ops)。
pub fn org_dlog_append_ops<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    record_key: &str,
) -> SyncResult<(u64, Vec<crate::storage::BatchOperation>)> {
    let seq = org_dlog_current_seq(storage, org_id, name, version)? + 1;
    let entry_key = format!("{}{:016}", org_dlog_entry_prefix(org_id, name, version), seq);
    let seq_key = org_dlog_seq_key(org_id, name, version);
    Ok((
        seq,
        vec![
            crate::storage::BatchOperation::put(entry_key, record_key),
            crate::storage::BatchOperation::put(seq_key, seq.to_string()),
        ],
    ))
}

/// 读取 org 域 dlog 中 `seq > after` 的条目（升序）。
pub fn org_dlog_entries_after<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    after: u64,
) -> SyncResult<Vec<(u64, String)>> {
    let prefix = org_dlog_entry_prefix(org_id, name, version);
    let mut out = Vec::new();
    for (key, value) in storage.scan(&ScanOptions::prefix(&prefix))? {
        let seq_str = key.strip_prefix(&prefix).unwrap_or(&key);
        let Ok(seq) = seq_str.parse::<u64>() else {
            continue;
        };
        if seq > after {
            out.push((seq, value));
        }
    }
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}

/// org 域 dlog GC：清理 `seq <= threshold` 的条目。
/// 等待集合 = 该集合复制组成员（账号口径，区别于 pdsync 的设备清单口径）。
pub fn org_dlog_gc<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    name: &str,
    version: &str,
    threshold: u64,
) -> SyncResult<usize> {
    if threshold == 0 {
        return Ok(0);
    }
    let prefix = org_dlog_entry_prefix(org_id, name, version);
    let mut removed = 0usize;
    let mut ops = Vec::new();
    for (key, _) in storage.scan(&ScanOptions::prefix(&prefix))? {
        let seq_str = key.strip_prefix(&prefix).unwrap_or(&key);
        let Ok(seq) = seq_str.parse::<u64>() else {
            continue;
        };
        if seq <= threshold {
            ops.push(crate::storage::BatchOperation::delete(key));
            removed += 1;
        }
    }
    if !ops.is_empty() {
        storage.batch(ops)?;
    }
    Ok(removed)
}

/// 计算 org 域 dlog GC 阈值 = 复制组中除本机外各成员**已知设备**确认水位的
/// 最小值（dlog seq 空间按设备维护，架构裁决 B6）。
///
/// - 等待集合 = 复制组中除本机 rootId 外的成员；
/// - 每个等待成员的设备水位从 `wm:{rootId}:{peerId}` 键域扫描获得（该成员
///   任意设备的水位都算其确认进度）；
/// - **任一等待成员没有任何已知设备水位记录 → 阻塞不清**（返回 0，防删除日志
///   被先清、未确认设备后续无法追偿）；
/// - 全部有记录 → threshold = **仅等待集合成员**设备水位的最小值。
///
/// F8 修正：min 只覆盖等待集合（复制组成员）内的键，避免已退出成员/漂移
/// peerId 的残留 wm 键永久压低阈值（GC 停摆）。成员移除时其 wm 键的清理
/// 挂账（后续成员操作清理，本函数不做额外扫描）。
pub fn org_dlog_gc_threshold<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    replication_members: &[String],
    self_root_id: &str,
) -> SyncResult<u64> {
    let wait_set: Vec<&String> = replication_members
        .iter()
        .filter(|r| r.as_str() != self_root_id)
        .collect();
    if wait_set.is_empty() {
        return org_dlog_current_seq(storage, org_id, name, version);
    }
    // 扫描该集合全部设备水位：`wm:{rootId}:{peerId}` → wm
    let wm_prefix = org_dlog_wm_prefix(org_id, name, version);
    let mut wm_by_root: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(&wm_prefix))? {
        let Some(rest) = key.strip_prefix(&wm_prefix) else {
            continue;
        };
        let Some(root_id) = rest.split(':').next() else {
            continue;
        };
        let Ok(wm) = raw.trim().parse::<u64>() else {
            continue;
        };
        wm_by_root.entry(root_id.to_string()).or_default().push(wm);
    }
    // 任一等待成员无设备水位记录 → 阻塞不清
    for root_id in &wait_set {
        if !wm_by_root.contains_key(root_id.as_str()) {
            return Ok(0);
        }
    }
    // F8：min 只覆盖等待集合成员的水位（不取漂移/已退出成员的残留键）
    let mut threshold = u64::MAX;
    for root_id in &wait_set {
        if let Some(wms) = wm_by_root.get(root_id.as_str()) {
            for wm in wms {
                threshold = threshold.min(*wm);
            }
        }
    }
    Ok(if threshold == u64::MAX { 0 } else { threshold })
}
