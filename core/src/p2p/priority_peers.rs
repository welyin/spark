//! 优先类目 peer 集合（自设备 + 好友）——重新寻址设计 §4.4。
//!
//! 移动端在 WiFi ↔ 蜂窝切换后 IP 变化，自设备与个人空间好友这两类关系
//! **没有任何组织级冗余**，断开后的 DHT 竞速是唯一恢复手段。本 store 用
//! sled 持久化"谁是我的设备 / 好友"这一有界目标集，供 p2p 层在
//! `ConnectionClosed` 时判断是否触发优先竞速。
//!
//! 集合仅存本地，不上网、不同步——"谁是我的设备和好友"这一关系图谱不出本机。

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;
use super::constants::P2P_PRIORITY_PEER_PREFIX;

/// 优先类目 peer 集合（peerId → 成员标记，值为 1；便于扫描删除）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PriorityMember {
    #[serde(rename = "v")]
    value: u8,
}

/// 优先类目 peer 集合（sled 持久化，复用 StorageBackend）。
pub struct PriorityPeerStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> PriorityPeerStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    fn key(peer_id: &str) -> String {
        format!("{P2P_PRIORITY_PEER_PREFIX}{peer_id}")
    }

    /// 判断 peer 是否属于优先类目（自设备 / 好友）。
    pub fn is_priority(&mut self, peer_id: &str) -> Result<bool> {
        Ok(self.storage.get(&Self::key(peer_id.trim()))?.is_some())
    }

    /// 加入优先类目（幂等）。
    pub fn add(&mut self, peer_id: &str) -> Result<()> {
        let normalized = peer_id.trim();
        if normalized.is_empty() {
            return Ok(());
        }
        self.storage.put(
            &Self::key(normalized),
            &serde_json::to_string(&PriorityMember { value: 1 })?,
        )?;
        Ok(())
    }

    /// 移出优先类目（幂等；不存在时无操作）。
    pub fn remove(&mut self, peer_id: &str) -> Result<()> {
        self.storage.delete(&Self::key(peer_id.trim()))?;
        Ok(())
    }

    /// 全量列出优先类目 peerId。
    pub fn list(&mut self) -> Result<Vec<String>> {
        let rows = self
            .storage
            .scan(&ScanOptions::prefix(P2P_PRIORITY_PEER_PREFIX))?;
        let mut peers = Vec::new();
        for (key, _) in rows {
            if let Some(id) = key.strip_prefix(P2P_PRIORITY_PEER_PREFIX) {
                if !id.is_empty() {
                    peers.push(id.to_string());
                }
            }
        }
        Ok(peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn priority_peer_store_crud() {
        let mut storage = MemoryStorage::default();
        {
            let mut store = PriorityPeerStore::new(&mut storage);
            // 初始为空
            assert!(store.list().unwrap().is_empty());
            assert!(!store.is_priority("peerA").unwrap());
            // add 后 is_priority 为真、list 包含
            store.add("peerA").unwrap();
            store.add("peerB").unwrap();
            assert!(store.is_priority("peerA").unwrap());
            assert!(store.is_priority("peerB").unwrap());
            let mut list = store.list().unwrap();
            list.sort();
            assert_eq!(list, vec!["peerA".to_string(), "peerB".to_string()]);
            // remove 幂等
            store.remove("peerA").unwrap();
            assert!(!store.is_priority("peerA").unwrap());
            store.remove("peerA").unwrap(); // 再删不报错
            // 空 peerId 忽略
            store.add("  ").unwrap();
            assert!(!store.is_priority("  ").unwrap());
        }
        // 重新用同一 storage 打开，验证持久化往返
        {
            let mut store = PriorityPeerStore::new(&mut storage);
            assert!(store.is_priority("peerB").unwrap());
            assert_eq!(store.list().unwrap(), vec!["peerB".to_string()]);
        }
    }
}
