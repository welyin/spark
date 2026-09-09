//! relay Unsupported 黑名单（A44：relay 选径 tier4 收敛，relay-implementation
//! R3 选择纪律的本地优化）。
//!
//! 选择纪律 tier4「已连接全集兜底」会对**无 relay hop 能力**的 peer 周期性发起
//! 预约（每 tick 一次，对端回 Unsupported 即失败）。本模块把「预约子流协商失败
//! （`ReserveError::Unsupported`）= 无 hop 能力」这一确定性信号记为本地黑名单，
//! TTL 内不再对其发起预约。其它失败类型（配额拒绝、超时等）不进黑名单，
//! 保持既有重试语义。
//!
//! 诚实口径：这是**本地观测优化，不是协议惩罚**——
//! - 独立 prefix、纯本地存储，不进 announce / pdsync / peer-exchange，不触协议线形；
//! - 各节点只按自己的观测记取，同一 peer 在不同节点上的黑名单状态可以不同；
//! - TTL 自动过期（[`RELAY_UNSUPPORTED_TTL_MS`]，24h）防永久误伤：对端升级开启
//!   relay server 后自然恢复候选资格；
//! - 成功证据解锁：一旦对该 peer 的预约成功（`on_reservation_accepted`），立即
//!   移除条目（`unblock`）。
//!
//! 键：`p2p:relay:unsupported:<peerId base58>`，值 = 过期时刻（i64 ms）。
//! 同型先例：overlay WrongPeerId 地址黑名单（addr_blacklist.rs）。

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;
use super::constants::{
    P2P_RELAY_UNSUPPORTED_PREFIX, RELAY_UNSUPPORTED_MAX_ENTRIES, RELAY_UNSUPPORTED_TTL_MS,
};

/// relay Unsupported 黑名单存储（独立 prefix，纯本地观测状态）。
pub struct RelayUnsupportedStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> RelayUnsupportedStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    fn key(peer_id: &str) -> String {
        format!("{P2P_RELAY_UNSUPPORTED_PREFIX}{peer_id}")
    }

    /// 记取黑名单：`peer_id` 到 `now_ms + TTL` 过期（重复记取刷新过期时刻）。
    pub fn block(&mut self, peer_id: &str, now_ms: i64) -> Result<()> {
        let expiry = now_ms + RELAY_UNSUPPORTED_TTL_MS;
        self.storage.put(&Self::key(peer_id), &expiry.to_string())?;
        self.purge_if_needed(now_ms)
    }

    /// 查询该 peer 是否在黑名单且未过期。`true` = 候选选择应抑制（不发起预约）。
    pub fn is_blocked(&mut self, peer_id: &str, now_ms: i64) -> Result<bool> {
        let Some(expiry) = self.storage.get(&Self::key(peer_id))? else {
            return Ok(false);
        };
        let Ok(expiry_ms) = expiry.parse::<i64>() else {
            // 坏数据（非数字）：视为已过期并清理
            let _ = self.storage.delete(&Self::key(peer_id));
            return Ok(false);
        };
        if expiry_ms <= now_ms {
            // TTL 过期：惰性清理，peer 恢复候选资格
            let _ = self.storage.delete(&Self::key(peer_id));
            return Ok(false);
        }
        Ok(true)
    }

    /// 成功证据解锁：预约成功 = hop 能力已就位，立即移出黑名单。
    pub fn unblock(&mut self, peer_id: &str) -> Result<()> {
        self.storage.delete(&Self::key(peer_id))?;
        Ok(())
    }

    /// 容量上限：先清已过期条目，仍超限则逐出最早过期者（最早过期的条目
    /// 观测价值最低，且新条目 TTL 最长、不会被自己逐出）。
    fn purge_if_needed(&mut self, now_ms: i64) -> Result<()> {
        let rows = self
            .storage
            .scan(&ScanOptions::prefix(P2P_RELAY_UNSUPPORTED_PREFIX))?;
        let mut entries: Vec<(String, i64)> = rows
            .into_iter()
            .filter_map(|(key, v)| v.parse::<i64>().ok().map(|e| (key, e)))
            .collect();
        // 先清过期（is_blocked 是惰性清理，此处兜底防只写不查的条目堆积）
        let expired: Vec<(String, i64)> = entries
            .iter()
            .filter(|(_, expiry)| *expiry <= now_ms)
            .cloned()
            .collect();
        for (key, _) in &expired {
            self.storage.delete(key)?;
        }
        entries.retain(|(_, expiry)| *expiry > now_ms);
        if entries.len() <= RELAY_UNSUPPORTED_MAX_ENTRIES {
            return Ok(());
        }
        entries.sort_by_key(|(_, expiry)| *expiry);
        let excess = entries.len() - RELAY_UNSUPPORTED_MAX_ENTRIES;
        for (key, _) in entries.into_iter().take(excess) {
            self.storage.delete(&key)?;
        }
        Ok(())
    }
}

/// 判定电路监听关闭原因是否为「对端无 relay hop 能力」。
///
/// libp2p-relay 0.21 client 没有 ReservationReqFailed 事件：预约子流协商失败
/// （对端未注册 hop 协议）→ 处理器报 `ReserveError::Unsupported` → transport
/// 以该错误关闭电路监听，`SwarmEvent::ListenerClosed.reason` 携带
/// `io::Error::other(client::transport::Error::Reservation(ReserveError))`。
/// 沿 source 链下钻 `ReserveError` 精确匹配 `Unsupported`；配额拒绝
/// （`ResourceLimitExceeded`）、IO/超时等其它原因一律返回 false。
pub fn is_unsupported_hop_close(reason: &std::result::Result<(), std::io::Error>) -> bool {
    use libp2p::relay::outbound::hop::ReserveError;
    let Err(err) = reason else {
        return false;
    };
    let mut cur: Option<&(dyn std::error::Error + 'static)> =
        err.get_ref().map(|e| e as &(dyn std::error::Error + 'static));
    while let Some(e) = cur {
        if let Some(reserve_err) = e.downcast_ref::<ReserveError>() {
            return matches!(reserve_err, ReserveError::Unsupported);
        }
        cur = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn block_and_hit_until_ttl_expiry() {
        let mut storage = MemoryStorage::new();
        let mut store = RelayUnsupportedStore::new(&mut storage);
        assert!(!store.is_blocked("peerA", 0).unwrap());
        store.block("peerA", 1000).unwrap();
        // TTL 内命中（含边界 expiry-1）
        assert!(store.is_blocked("peerA", 1000).unwrap());
        assert!(
            store.is_blocked("peerA", 1000 + RELAY_UNSUPPORTED_TTL_MS - 1)
                .unwrap()
        );
        // 到达过期时刻即失效，且条目被惰性清理
        assert!(
            !store
                .is_blocked("peerA", 1000 + RELAY_UNSUPPORTED_TTL_MS)
                .unwrap()
        );
        assert!(
            storage
                .get(&format!("{P2P_RELAY_UNSUPPORTED_PREFIX}peerA"))
                .unwrap()
                .is_none(),
            "过期查询应惰性清掉条目"
        );
    }

    #[test]
    fn block_refreshes_expiry() {
        let mut storage = MemoryStorage::new();
        let mut store = RelayUnsupportedStore::new(&mut storage);
        store.block("peerA", 0).unwrap();
        store.block("peerA", RELAY_UNSUPPORTED_TTL_MS).unwrap();
        // 以第二次记取时刻起算 TTL：第一次的过期点已过但仍命中
        assert!(store.is_blocked("peerA", RELAY_UNSUPPORTED_TTL_MS).unwrap());
        assert!(
            !store
                .is_blocked("peerA", 2 * RELAY_UNSUPPORTED_TTL_MS)
                .unwrap()
        );
    }

    #[test]
    fn unblock_removes_entry() {
        let mut storage = MemoryStorage::new();
        let mut store = RelayUnsupportedStore::new(&mut storage);
        store.block("peerA", 0).unwrap();
        store.unblock("peerA").unwrap();
        assert!(!store.is_blocked("peerA", 0).unwrap());
    }

    #[test]
    fn capacity_evicts_earliest_expiry_first() {
        let mut storage = MemoryStorage::new();
        let mut store = RelayUnsupportedStore::new(&mut storage);
        // 写入上限 + 1 条：now 递增 ⇒ 过期时刻递增，最早写入者最早过期
        let now = 1000i64;
        for i in 0..=RELAY_UNSUPPORTED_MAX_ENTRIES {
            store.block(&format!("peer{i}"), now + i as i64).unwrap();
        }
        // peer0 最早过期被逐出，其余（含最后写入者）保留
        assert!(
            !store.is_blocked("peer0", now + RELAY_UNSUPPORTED_MAX_ENTRIES as i64).unwrap(),
            "超限应逐出最早过期者"
        );
        for i in 1..=RELAY_UNSUPPORTED_MAX_ENTRIES {
            assert!(
                store
                    .is_blocked(&format!("peer{i}"), now + RELAY_UNSUPPORTED_MAX_ENTRIES as i64)
                    .unwrap(),
                "peer{i} 应保留"
            );
        }
    }

    #[test]
    fn purge_cleans_expired_entries_on_write() {
        let mut storage = MemoryStorage::new();
        {
            let mut store = RelayUnsupportedStore::new(&mut storage);
            store.block("stale", 0).unwrap();
            // 远超 TTL 后的新写入触发 purge：过期条目被清除
            store.block("fresh", 10 * RELAY_UNSUPPORTED_TTL_MS).unwrap();
            assert!(
                store
                    .is_blocked("fresh", 10 * RELAY_UNSUPPORTED_TTL_MS)
                    .unwrap()
            );
        }
        assert!(
            storage
                .get(&format!("{P2P_RELAY_UNSUPPORTED_PREFIX}stale"))
                .unwrap()
                .is_none(),
            "写入时 purge 应清掉已过期条目"
        );
    }

    #[test]
    fn unsupported_hop_close_detection() {
        use libp2p::relay::client::transport::Error as TransportError;
        use libp2p::relay::outbound::hop::ReserveError;

        // 生产路径形态：transport Error 包裹 ReserveError（经 boxed transport
        // io::Error::other 保留具体类型）
        let wrapped: std::result::Result<(), std::io::Error> = Err(std::io::Error::other(
            TransportError::Reservation(ReserveError::Unsupported),
        ));
        assert!(is_unsupported_hop_close(&wrapped));
        // 裸 ReserveError 形态（source 链单环）
        let bare: std::result::Result<(), std::io::Error> =
            Err(std::io::Error::other(ReserveError::Unsupported));
        assert!(is_unsupported_hop_close(&bare));

        // 配额拒绝（ResourceLimitExceeded）不是能力信号，不误伤
        let quota: std::result::Result<(), std::io::Error> = Err(std::io::Error::other(
            TransportError::Reservation(ReserveError::ResourceLimitExceeded),
        ));
        assert!(!is_unsupported_hop_close(&quota));
        // 预约被拒（Refused）/ IO / 普通文本错误 / 正常关闭都不是
        let refused: std::result::Result<(), std::io::Error> =
            Err(std::io::Error::other(ReserveError::Refused));
        assert!(!is_unsupported_hop_close(&refused));
        let io_err: std::result::Result<(), std::io::Error> =
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"));
        assert!(!is_unsupported_hop_close(&io_err));
        assert!(!is_unsupported_hop_close(&Ok(())));
    }
}
