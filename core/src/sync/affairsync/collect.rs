//! affairsync 摘要折叠、增量采集与 diff 裁决（affair-sync §3/§7）。
//!
//! 折叠：扫描 `affair:rec:{affairId}` 与 `affair:op:{affairId}:*` 全部记录的
//! pmeta 逐条 merge 取 max（口径同 orgsync collect_org_collection_vv）。
//! 增量：按 knownVv 采集本地领先/并发记录。affair 域无墓碑面，不采集墓碑。

use crate::affair::{AFFAIR_OP_PREFIX, affair_record_key};
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::{
    CompareResult, VersionVector, compare_version_vectors, merge_version_vectors,
};

use super::envelope::AffairsyncRecord;

/// `affair:op:{affairId}:` 数据键域（本模块与 apply 共用）。
pub(super) fn affair_ops_data_prefix(affair_id: &str) -> String {
    format!("{AFFAIR_OP_PREFIX}{affair_id}:")
}

/// 收集某事务的合并折叠 vv：创世记录 + 全部操作条目的 pmeta 逐条 merge。
pub fn collect_affair_vv<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> SyncResult<VersionVector> {
    let mut folded = VersionVector::new();
    let data_prefixes = [
        affair_record_key(affair_id),
        affair_ops_data_prefix(affair_id),
    ];
    for prefix in data_prefixes {
        let meta_prefix = format!("pmeta:{prefix}");
        for (meta_key, raw) in storage.scan(&ScanOptions::prefix(&meta_prefix))? {
            let Some(record_key) = meta_key.strip_prefix("pmeta:") else {
                continue;
            };
            if !record_key.starts_with(&prefix) {
                continue;
            }
            if let Ok(meta) = serde_json::from_str::<crate::sync::meta::DocMeta>(&raw) {
                folded = merge_version_vectors(Some(&folded), Some(&meta.vv));
            }
        }
    }
    Ok(folded)
}

/// 一个事务的 diff 结论（口径同 orgsync OrgDiffOutcome）。
#[derive(Clone, Debug)]
pub enum AffairDiffOutcome {
    /// 本地落后：回 affairsync-need。
    LocalBehind {
        /// 本地折叠 vv（作 knownVv 用）。
        local_vv: VersionVector,
    },
    /// 本地领先：推 affairsync-data。
    LocalAhead,
    /// 并发：need + data 双向。
    Concurrent {
        /// 本地折叠 vv（作 knownVv 用）。
        local_vv: VersionVector,
    },
    /// 相等：不动。
    Equal,
}

/// 对比本地折叠 vv 与对端 hello 摘要中的折叠 vv。
pub fn diff_affair(local_vv: &VersionVector, remote_vv: &VersionVector) -> AffairDiffOutcome {
    match compare_version_vectors(Some(local_vv), Some(remote_vv)) {
        CompareResult::Remote => AffairDiffOutcome::LocalBehind {
            local_vv: local_vv.clone(),
        },
        CompareResult::Local => AffairDiffOutcome::LocalAhead,
        CompareResult::Concurrent => AffairDiffOutcome::Concurrent {
            local_vv: local_vv.clone(),
        },
        CompareResult::Equal => AffairDiffOutcome::Equal,
    }
}

/// 按 `knownVv` 采集事务增量（need 的处理与 hello 本地领先分支）：
/// 创世记录 + 全部操作条目中本地 pmeta 相对 knownVv 不是 Remote/Equal 的纳入返回。
pub fn collect_affair_records<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
    known_vv: &VersionVector,
) -> SyncResult<Vec<AffairsyncRecord>> {
    let mut records = Vec::new();
    let rec_key = affair_record_key(affair_id);
    let op_prefix = affair_ops_data_prefix(affair_id);
    let mut data_keys = vec![rec_key];
    for (key, _) in storage.scan(&ScanOptions::prefix(&op_prefix))? {
        data_keys.push(key);
    }
    data_keys.sort();
    for key in data_keys {
        let Some(raw_value) = storage.get(&key)? else {
            continue;
        };
        let meta = match crate::sync::get_personal_meta(storage, &key)? {
            Some(m) => m,
            None => continue,
        };
        match compare_version_vectors(Some(&meta.vv), Some(known_vv)) {
            CompareResult::Remote | CompareResult::Equal => continue,
            _ => {
                let value = match serde_json::from_str(&raw_value) {
                    Ok(v) => v,
                    Err(error) => {
                        eprintln!("[affairsync] skip corrupted record {key}: {error}");
                        continue;
                    }
                };
                records.push(AffairsyncRecord { key, value, meta });
            }
        }
    }
    Ok(records)
}

/// vv 折叠的便捷观测（测试/调试）：某事务是否已有任何本地记录。
pub fn affair_has_any_records<S: StorageBackend>(storage: &S, affair_id: &str) -> SyncResult<bool> {
    Ok(!collect_affair_vv(storage, affair_id)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::meta::DocMeta;

    fn id(byte: &str) -> String {
        byte.repeat(32)
    }

    fn put_record(
        storage: &mut MemoryStorage,
        affair_id: &str,
        op_hash: &str,
        node: &str,
        counter: i64,
    ) {
        let key = crate::affair::affair_op_key(affair_id, op_hash);
        let meta = DocMeta {
            vv: VersionVector::from([(node.to_string(), counter)]),
            ts: 1000,
            ..DocMeta::default()
        };
        storage.put(&key, "{\"opV\":1}").unwrap();
        storage
            .put(
                &crate::sync::personal_meta_key(&key),
                &serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn fold_vv_merges_rec_and_ops() {
        let mut storage = MemoryStorage::default();
        let affair_id = id("ab");
        // 创世（node-a 分量 1）+ 两条操作（node-a 分量 2 / node-b 分量 1）
        let rec_key = affair_record_key(&affair_id);
        let rec_meta = DocMeta {
            vv: VersionVector::from([("node-a".to_string(), 1)]),
            ts: 900,
            ..DocMeta::default()
        };
        storage.put(&rec_key, "{}").unwrap();
        storage
            .put(
                &crate::sync::personal_meta_key(&rec_key),
                &serde_json::to_string(&rec_meta).unwrap(),
            )
            .unwrap();
        put_record(&mut storage, &affair_id, &id("c1"), "node-a", 2);
        put_record(&mut storage, &affair_id, &id("c2"), "node-b", 1);
        let folded = collect_affair_vv(&storage, &affair_id).unwrap();
        assert_eq!(folded.get("node-a"), Some(&2));
        assert_eq!(folded.get("node-b"), Some(&1));
    }

    #[test]
    fn diff_outcomes() {
        let mut local = VersionVector::new();
        local.insert("n".to_string(), 1);
        let mut remote = VersionVector::new();
        remote.insert("n".to_string(), 2);
        assert!(matches!(
            diff_affair(&local, &remote),
            AffairDiffOutcome::LocalBehind { .. }
        ));
        assert!(matches!(
            diff_affair(&remote, &local),
            AffairDiffOutcome::LocalAhead
        ));
        let mut remote2 = VersionVector::new();
        remote2.insert("m".to_string(), 1);
        assert!(matches!(
            diff_affair(&local, &remote2),
            AffairDiffOutcome::Concurrent { .. }
        ));
        assert!(matches!(
            diff_affair(&local, &local),
            AffairDiffOutcome::Equal
        ));
    }

    #[test]
    fn incremental_collects_only_leading() {
        let mut storage = MemoryStorage::default();
        let affair_id = id("ab");
        put_record(&mut storage, &affair_id, &id("c1"), "node-a", 1);
        put_record(&mut storage, &affair_id, &id("c2"), "node-a", 2);
        // knownVv 已含 c1 的版本 → 只采集 c2
        let known = VersionVector::from([("node-a".to_string(), 1)]);
        let records = collect_affair_records(&storage, &affair_id, &known).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].key.ends_with(&id("c2")));
        // 空 knownVv → 全量
        let all = collect_affair_records(&storage, &affair_id, &VersionVector::new()).unwrap();
        assert_eq!(all.len(), 2);
    }
}
