//! 关注者目录（affair-sync §5/§6）：复制流量中学到的覆盖网线索。
//!
//! 入站 affairsync-hello / affairsync-need 的 `from`（rootId + 连接层 peerId +
//! 时刻）落账 `affairsync:dir:{affairId}:{rootId}`。目录条目是**线索而非信任
//! 根**——数据面安全边界始终是 apply 层逐条验签链（affair-sync §4/§6）。
//! indexer 目录（查询协议的 indexer 发现面）在 `crate::index::directory`
//! （affair-metadata §7.1 indexer-card），不在本面层。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;

use super::keys::{affair_dir_key, affair_dir_prefix};

/// 关注者目录条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowerHint {
    /// 关注者的域身份 rootId（64hex）。
    pub root_id: String,
    /// 最近一次复制流量观测时刻（ms）。
    pub last_seen_ms: i64,
    /// 最近一次观测的连接层 peerId（线索用，可漂移）。
    pub peer_id: Option<String>,
}

/// 记录一次复制流量观测（upsert：只增不降级，lastSeenMs 只前进）。
pub fn note_follower_seen<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    root_id: &str,
    peer_id: Option<&str>,
    now_ms: i64,
) -> SyncResult<()> {
    let key = affair_dir_key(affair_id, root_id);
    let existing = storage.get(&key)?;
    let hint = match existing.and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) {
        Some(value) => {
            let last_seen_ms = value
                .get("lastSeenMs")
                .and_then(Value::as_i64)
                .map(|prev| prev.max(now_ms))
                .unwrap_or(now_ms);
            FollowerHint {
                root_id: root_id.to_string(),
                last_seen_ms,
                peer_id: peer_id.map(ToString::to_string).or_else(|| {
                    value
                        .get("peerId")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                }),
            }
        }
        None => FollowerHint {
            root_id: root_id.to_string(),
            last_seen_ms: now_ms,
            peer_id: peer_id.map(ToString::to_string),
        },
    };
    storage.put(&key, &serde_json::to_string(&hint)?)?;
    Ok(())
}

/// 读取某事务的关注者目录（按 rootId 排序）。
pub fn follower_hints<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> SyncResult<Vec<FollowerHint>> {
    let prefix = affair_dir_prefix(affair_id);
    let mut out = Vec::new();
    for (_, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
        if let Ok(hint) = serde_json::from_str::<FollowerHint>(&raw) {
            out.push(hint);
        }
    }
    out.sort_by(|a, b| a.root_id.cmp(&b.root_id));
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
    fn note_and_list_followers() {
        let mut storage = MemoryStorage::default();
        let affair_id = id("ab");
        let root_b = id("bb");
        let root_a = id("aa");
        note_follower_seen(&mut storage, &affair_id, &root_b, Some("peer-1"), 1000).unwrap();
        note_follower_seen(&mut storage, &affair_id, &root_a, None, 2000).unwrap();
        // 同 rootId 再观测：时刻前进、peerId 更新
        note_follower_seen(&mut storage, &affair_id, &root_b, Some("peer-2"), 3000).unwrap();
        let hints = follower_hints(&storage, &affair_id).unwrap();
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].root_id, root_a);
        assert_eq!(hints[1].root_id, root_b);
        assert_eq!(hints[1].last_seen_ms, 3000);
        assert_eq!(hints[1].peer_id.as_deref(), Some("peer-2"));
    }
}
