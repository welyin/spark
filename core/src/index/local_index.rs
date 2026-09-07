//! indexer 本地索引（affair-metadata §1/§4）：订阅 `spark-affair-meta` 即
//! 元数据全量副本；收录条目按 affairId 键控，只索引标题/简介/标签（§1
//! 产品级承诺）+ 代际与验证状态。存储键 `affmeta:idx:{affairId}`，本地键
//! 不进同步。扫描按键升序 = affairId 字典序（确定性消费顺序）。

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::IndexError;
use super::announce::MetaAnnounce;

/// 本地索引键前缀。
pub const INDEX_PREFIX: &str = "affmeta:idx:";

/// 索引条目（查询消费的唯一事实来源：展示字段 + 代际 + 验证状态）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub affair_id: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub meta_seq: u64,
    pub basis_op_hash: String,
    pub verified: bool,
    pub updated_at: i64,
}

/// 索引存储键。
pub fn index_key(affair_id: &str) -> String {
    format!("{INDEX_PREFIX}{affair_id}")
}

/// 由暂存区胜方公告构造索引条目。
pub fn entry_from_announce(announce: &MetaAnnounce, verified: bool) -> IndexEntry {
    IndexEntry {
        affair_id: announce.affair_id.clone(),
        title: announce.title.clone(),
        summary: announce.summary.clone(),
        tags: announce.tags.clone(),
        region: announce.region.clone(),
        meta_seq: announce.meta_seq,
        basis_op_hash: announce.basis_op_hash.clone(),
        verified,
        updated_at: announce.updated_at,
    }
}

/// upsert 索引条目（只应由暂存区裁决胜方调用，见 ingest）。
pub fn upsert_entry<S: StorageBackend>(
    storage: &mut S,
    entry: &IndexEntry,
) -> Result<(), IndexError> {
    storage.put(
        &index_key(&entry.affair_id),
        &serde_json::to_string(entry).expect("index entry serializable"),
    )?;
    Ok(())
}

/// 全量索引条目（键升序 = affairId 字典序；值损坏条目跳过）。
pub fn list_entries<S: StorageBackend>(storage: &S) -> Result<Vec<IndexEntry>, IndexError> {
    let mut entries = Vec::new();
    for (_, raw) in storage.scan(&ScanOptions::prefix(INDEX_PREFIX))? {
        if let Ok(entry) = serde_json::from_str::<IndexEntry>(&raw) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn list_entries_sorted_by_affair_id() {
        let mut s = MemoryStorage::new();
        for seed in ['c', 'a', 'b'] {
            let id = seed.to_string().repeat(64);
            upsert_entry(
                &mut s,
                &IndexEntry {
                    affair_id: id.clone(),
                    title: seed.to_string(),
                    summary: String::new(),
                    tags: Vec::new(),
                    region: None,
                    meta_seq: 0,
                    basis_op_hash: id,
                    verified: false,
                    updated_at: 1,
                },
            )
            .unwrap();
        }
        let ids: Vec<String> = list_entries(&s)
            .unwrap()
            .into_iter()
            .map(|e| e.affair_id)
            .collect();
        assert_eq!(ids, vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)]);
    }
}
