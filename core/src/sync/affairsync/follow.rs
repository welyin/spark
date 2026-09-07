//! affair 关注关系簿记（wiki/protocol/community/affair-sync.md §5）。
//!
//! `affair:follow:{affairId}` 是本地键（affair.md §3.3：不进任何同步流量）——
//! 关注不广播、取关不删除数据；复制组 = 动态关注者集合的「本机半集」，
//! 对端半集经覆盖网线索（dir.rs）与 indexer 目录发现（affair-sync §6）。

use serde_json::{Value, json};

use crate::affair::{affair_follow_key, is_valid_identity_id};
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;

/// 关注状态记录线形：`{"v":1,"followedAt":<ms>}`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowState {
    /// 关注时刻（本机毫秒）。
    pub followed_at: i64,
}

/// 关注某事务（幂等：已关注则只刷新不透传旧值之外的状态）。
pub fn follow_affair<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    now_ms: i64,
) -> SyncResult<()> {
    if !is_valid_identity_id(affair_id) {
        return Err(crate::sync::SyncError::Adapter(
            "invalid affairId".to_string(),
        ));
    }
    if is_following(storage, affair_id)? {
        return Ok(());
    }
    storage.put(
        &affair_follow_key(affair_id),
        &json!({ "v": 1, "followedAt": now_ms }).to_string(),
    )?;
    Ok(())
}

/// 取关：删除本地关注状态（不删除已复制的数据；affair 数据 append-only）。
pub fn unfollow_affair<S: StorageBackend>(storage: &mut S, affair_id: &str) -> SyncResult<()> {
    storage.delete(&affair_follow_key(affair_id))?;
    Ok(())
}

/// 是否已关注某事务。
pub fn is_following<S: StorageBackend>(storage: &S, affair_id: &str) -> SyncResult<bool> {
    Ok(storage.get(&affair_follow_key(affair_id))?.is_some())
}

/// 读取关注状态（未关注返回 None；损坏记录按未关注处理并留日志）。
pub fn follow_state<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> SyncResult<Option<FollowState>> {
    let Some(raw) = storage.get(&affair_follow_key(affair_id))? else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        log::warn!("[AFFAIRSYNC] corrupted follow record affair={affair_id}");
        return Ok(None);
    };
    Ok(value
        .get("followedAt")
        .and_then(Value::as_i64)
        .map(|followed_at| FollowState { followed_at }))
}

/// 列出本机关注的全部事务 id（`affair:follow:` 键域扫描）。
pub fn list_followed_affairs<S: StorageBackend>(storage: &S) -> SyncResult<Vec<String>> {
    let prefix = crate::affair::AFFAIR_FOLLOW_PREFIX;
    let mut out = Vec::new();
    for (key, _) in storage.scan(&ScanOptions::prefix(prefix))? {
        if let Some(affair_id) = key.strip_prefix(prefix) {
            out.push(affair_id.to_string());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn id(byte: &str) -> String {
        byte.repeat(32)
    }

    #[test]
    fn follow_unfollow_roundtrip() {
        let mut storage = MemoryStorage::default();
        let affair_id = id("ab");
        assert!(!is_following(&storage, &affair_id).unwrap());
        follow_affair(&mut storage, &affair_id, 1000).unwrap();
        assert!(is_following(&storage, &affair_id).unwrap());
        assert_eq!(
            follow_state(&storage, &affair_id).unwrap(),
            Some(FollowState { followed_at: 1000 })
        );
        // 幂等：重复关注不报错、不改动时刻
        follow_affair(&mut storage, &affair_id, 2000).unwrap();
        assert_eq!(
            follow_state(&storage, &affair_id).unwrap(),
            Some(FollowState { followed_at: 1000 })
        );
        unfollow_affair(&mut storage, &affair_id).unwrap();
        assert!(!is_following(&storage, &affair_id).unwrap());
        assert_eq!(follow_state(&storage, &affair_id).unwrap(), None);
    }

    #[test]
    fn list_followed_sorted() {
        let mut storage = MemoryStorage::default();
        let b = id("bb");
        let a = id("aa");
        follow_affair(&mut storage, &b, 1).unwrap();
        follow_affair(&mut storage, &a, 2).unwrap();
        assert_eq!(list_followed_affairs(&storage).unwrap(), vec![a, b]);
    }

    #[test]
    fn follow_rejects_bad_affair_id() {
        let mut storage = MemoryStorage::default();
        assert!(follow_affair(&mut storage, "not-hex", 1).is_err());
    }
}
