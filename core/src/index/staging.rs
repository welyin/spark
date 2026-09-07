//! 元数据暂存区（affair-metadata §5）：`affmeta:seen:{affairId}` 键域、
//! (metaSeq, updatedAt) 裁决（复用 affair::arbitrate_meta 纯函数）、体积
//! 卫生（上限 100k，最旧未验证先淘汰，verified 不淘汰；淘汰连带删除
//! `affmeta:idx:` 索引条目）。本地键不进同步。

use serde::{Deserialize, Serialize};

use crate::affair::{MetaArbitration, MetaSeen, arbitrate_meta};
use crate::storage::{ScanOptions, StorageBackend};

use super::IndexError;
use super::announce::MetaAnnounce;

/// 暂存区键前缀（§5）。
pub const SEEN_PREFIX: &str = "affmeta:seen:";
/// 暂存区条数上限（§5 体积卫生）。
pub const SEEN_MAX: usize = 100_000;

/// 暂存条目（暂存公告 + 验证状态 + 入库时刻）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedAnnounce {
    pub announce: MetaAnnounce,
    pub verified: bool,
    pub stored_at: i64,
}

/// 暂存裁决执行结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageOutcome {
    /// 新条目入库。
    Inserted,
    /// 新公告按裁决替换既有条目。
    Replaced,
    /// 裁决保留既有条目，本条丢弃。
    Kept,
}

/// 暂存区存储键（§5）。
pub fn seen_key(affair_id: &str) -> String {
    format!("{SEEN_PREFIX}{affair_id}")
}

/// 读取暂存条目（不存在或值损坏返回 None）。
pub fn get_staged<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> Result<Option<StagedAnnounce>, IndexError> {
    let Some(raw) = storage.get(&seen_key(affair_id))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// 入暂存区（§5 裁决）：同 affairId 多公告按 `(metaSeq, updatedAt)` 字典序
/// 大者胜，verified 条目不被 unverified 覆盖。新条目触发体积卫生检查。
pub fn stage_announcement<S: StorageBackend>(
    storage: &mut S,
    announce: &MetaAnnounce,
    verified: bool,
    now_ms: i64,
) -> Result<StageOutcome, IndexError> {
    let existing = get_staged(storage, &announce.affair_id)?;
    let outcome = match &existing {
        Some(prev) => {
            let decision = arbitrate_meta(
                &MetaSeen {
                    meta_seq: prev.announce.meta_seq,
                    updated_at: prev.announce.updated_at,
                    verified: prev.verified,
                },
                &MetaSeen {
                    meta_seq: announce.meta_seq,
                    updated_at: announce.updated_at,
                    verified,
                },
            );
            if decision == MetaArbitration::Keep {
                return Ok(StageOutcome::Kept);
            }
            StageOutcome::Replaced
        }
        None => StageOutcome::Inserted,
    };
    let staged = StagedAnnounce {
        announce: announce.clone(),
        verified,
        stored_at: existing.as_ref().map(|e| e.stored_at).unwrap_or(now_ms),
    };
    storage.put(
        &seen_key(&announce.affair_id),
        &serde_json::to_string(&staged).expect("staged announce serializable"),
    )?;
    if outcome == StageOutcome::Inserted {
        evict_to_cap(storage, SEEN_MAX)?;
    }
    Ok(outcome)
}

/// 体积卫生（§5）：超 `cap` 条时最旧（storedAt 升序，同刻按键序）未验证条目
/// 先淘汰；verified 条目一律保留——超限且全部 verified 时接受超限
/// （验证劳动不白费）。全量扫描仅在超限后触发。
///
/// 淘汰连带删除该公告的 `affmeta:idx:` 索引条目（local_index 头注：索引是
/// 查询服务的唯一事实来源）——否则被汰公告仍可被查、索引面无上限。
fn evict_to_cap<S: StorageBackend>(storage: &mut S, cap: usize) -> Result<(), IndexError> {
    let rows = storage.scan(&ScanOptions::prefix(SEEN_PREFIX))?;
    if rows.len() <= cap {
        return Ok(());
    }
    let mut entries: Vec<(i64, bool, String)> = rows
        .into_iter()
        .filter_map(|(key, raw)| {
            serde_json::from_str::<StagedAnnounce>(&raw)
                .ok()
                .map(|s| (s.stored_at, s.verified, key))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    let mut excess = entries.len() - cap;
    for (_, verified, key) in entries {
        if excess == 0 {
            break;
        }
        if verified {
            continue;
        }
        if let Some(affair_id) = key.strip_prefix(SEEN_PREFIX) {
            storage.delete(&super::local_index::index_key(affair_id))?;
        }
        storage.delete(&key)?;
        excess -= 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn announce(id_seed: char, meta_seq: u64, updated_at: i64) -> MetaAnnounce {
        MetaAnnounce {
            affair_id: id_seed.to_string().repeat(64),
            title: "t".to_string(),
            summary: String::new(),
            tags: Vec::new(),
            region: None,
            meta_seq,
            basis_op_hash: id_seed.to_string().repeat(64),
            contents: Vec::new(),
            updated_at,
        }
    }

    fn staged_ids(s: &MemoryStorage) -> Vec<String> {
        let mut ids: Vec<String> = s
            .scan(&ScanOptions::prefix(SEEN_PREFIX))
            .unwrap()
            .into_iter()
            .filter_map(|(_, raw)| {
                serde_json::from_str::<StagedAnnounce>(&raw)
                    .ok()
                    .map(|staged| staged.announce.affair_id)
            })
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn unverified_does_not_replace_verified() {
        let mut s = MemoryStorage::new();
        let a = announce('a', 1, 100);
        stage_announcement(&mut s, &a, true, 100).unwrap();
        let newer_unverified = MetaAnnounce {
            meta_seq: 9,
            updated_at: 999,
            ..a.clone()
        };
        let outcome = stage_announcement(&mut s, &newer_unverified, false, 200).unwrap();
        assert_eq!(outcome, StageOutcome::Kept);
        let staged = get_staged(&s, &a.affair_id).unwrap().unwrap();
        assert_eq!(staged.announce.meta_seq, 1);
        assert!(staged.verified);
    }

    #[test]
    fn higher_meta_seq_wins_over_newer_updated_at() {
        let mut s = MemoryStorage::new();
        let a = announce('a', 5, 100);
        stage_announcement(&mut s, &a, false, 100).unwrap();
        let lower_seq_newer_time = MetaAnnounce {
            meta_seq: 4,
            updated_at: 999,
            ..a.clone()
        };
        let outcome = stage_announcement(&mut s, &lower_seq_newer_time, false, 200).unwrap();
        assert_eq!(outcome, StageOutcome::Kept);
    }

    #[test]
    fn eviction_drops_oldest_unverified_first_and_keeps_verified() {
        let mut s = MemoryStorage::new();
        // 2 verified（最旧）+ 4 unverified（较新），压到 cap=4：
        // 淘汰 2 条最旧未验证（c/d），verified 的 a/b 保留。
        for (i, seed) in ['a', 'b'].into_iter().enumerate() {
            stage_announcement(&mut s, &announce(seed, 1, 100), true, 1000 + i as i64).unwrap();
        }
        for (i, seed) in ['c', 'd', 'e', 'f'].into_iter().enumerate() {
            stage_announcement(&mut s, &announce(seed, 1, 100), false, 2000 + i as i64).unwrap();
        }
        evict_to_cap(&mut s, 4).unwrap();
        assert_eq!(staged_ids(&s), {
            let mut ids = vec![
                "a".repeat(64),
                "b".repeat(64),
                "e".repeat(64),
                "f".repeat(64),
            ];
            ids.sort();
            ids
        });
        // 全部 verified 超限：接受超限只淘汰未验证——e/f 被淘汰，a/b 保留
        evict_to_cap(&mut s, 1).unwrap();
        assert_eq!(staged_ids(&s), vec!["a".repeat(64), "b".repeat(64)]);
    }

    #[test]
    fn eviction_also_drops_index_entry() {
        use crate::index::local_index::{entry_from_announce, index_key, upsert_entry};

        let mut s = MemoryStorage::new();
        // 两条 unverified：a 最旧先淘汰，b 保留；两者均建有索引条目
        for (i, seed) in ['a', 'b'].into_iter().enumerate() {
            let announce = announce(seed, 1, 100);
            stage_announcement(&mut s, &announce, false, 1000 + i as i64).unwrap();
            upsert_entry(&mut s, &entry_from_announce(&announce, false)).unwrap();
        }
        evict_to_cap(&mut s, 1).unwrap();
        assert_eq!(staged_ids(&s), vec!["b".repeat(64)]);
        // 淘汰连带删除索引：被汰公告不再可查，留存条目的索引仍在
        assert!(s.get(&index_key(&"a".repeat(64))).unwrap().is_none());
        assert!(s.get(&index_key(&"b".repeat(64))).unwrap().is_some());
    }
}
