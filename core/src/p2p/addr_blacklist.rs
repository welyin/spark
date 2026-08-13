//! WrongPeerId 地址黑名单（wrong-peer-id-address-pollution S4，独立存储）。
//!
//! 纯本地收敛状态：`key = p2p:overlay:blacklist:<base_addr>`，`value = 过期时刻`
//! （i64 ms）。base_addr 为剥掉 `/p2p` 段后的地址（与 M9 `base_addr` 同义），
//! raw 与带 peer 段形态互认。
//!
//! 独立 prefix、**不进 OverlayPeerRecord / pdsync / peer-exchange**，不触协议线形。
//! 设计要点（见 wrong-peer-id-address-pollution.md §2.7–2.9）：
//! - **TTL 自动过期**：10min（`NODE_ANNOUNCE_INTERVAL_MS × 2`），过期后地址可重新
//!   remember，是防永久误伤的核心。
//! - **成功证据解锁**：`ConnectionEstablished` 对目标 peer 真实连上该地址时调用
//!   `unblock`，立即移除——「地址真实转移」的强信号。
//! - **容量上限**：短暂 TTL 状态，超限清最旧（`BLACKLIST_MAX_ENTRIES`）。

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;
use super::constants::P2P_OVERLAY_BLACKLIST_PREFIX;

/// 黑名单容量上限（短暂 TTL 状态，防无限增长）。
const BLACKLIST_MAX_ENTRIES: usize = 500;

/// WrongPeerId 地址黑名单存储（独立 prefix，纯本地收敛）。
pub struct AddrBlacklistStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> AddrBlacklistStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    /// 剥掉 multiaddr 尾部 `/p2p/{peerId}` 段，得到存储键（raw 与带 peer 段互认）。
    fn base_addr(addr: &str) -> String {
        addr.split("/p2p/").next().unwrap_or(addr).to_string()
    }

    fn key(base_addr: &str) -> String {
        format!("{P2P_OVERLAY_BLACKLIST_PREFIX}{base_addr}")
    }

    /// 写入黑名单：`base_addr` 到 `now_ms + ttl_ms` 过期。
    pub fn block(&mut self, addr: &str, now_ms: i64, ttl_ms: i64) -> Result<()> {
        let base = Self::base_addr(addr);
        let expiry = now_ms + ttl_ms;
        self.storage.put(&Self::key(&base), &expiry.to_string())?;
        self.purge_if_needed()
    }

    /// 查询该地址是否在黑名单且未过期。`true` = 应跳过（不 remember / 不发布）。
    pub fn is_blocked(&mut self, addr: &str, now_ms: i64) -> Result<bool> {
        let base = Self::base_addr(addr);
        let Some(expiry) = self.storage.get(&Self::key(&base))? else {
            return Ok(false);
        };
        let Ok(expiry_ms) = expiry.parse::<i64>() else {
            // 坏数据（非数字）：视为已过期并清理
            let _ = self.storage.delete(&Self::key(&base));
            return Ok(false);
        };
        if expiry_ms <= now_ms {
            // TTL 过期：惰性清理，地址可重新 remember
            let _ = self.storage.delete(&Self::key(&base));
            return Ok(false);
        }
        Ok(true)
    }

    /// 成功证据解锁：`ConnectionEstablished` 真实连上该地址后移除黑名单条目。
    pub fn unblock(&mut self, addr: &str) -> Result<()> {
        let base = Self::base_addr(addr);
        self.storage.delete(&Self::key(&base))?;
        Ok(())
    }

    /// 容量上限：超限清最旧（BTreeMap 扫描天然按 key 升序，即 base_addr 字典序）。
    /// 最旧条目 ≈ 字典序最小，此处不严格按写入时刻，接受近似（短暂 TTL 状态）。
    fn purge_if_needed(&mut self) -> Result<()> {
        let rows = self
            .storage
            .scan(&ScanOptions::prefix(P2P_OVERLAY_BLACKLIST_PREFIX))?;
        if rows.len() <= BLACKLIST_MAX_ENTRIES {
            return Ok(());
        }
        let excess = rows.len() - BLACKLIST_MAX_ENTRIES;
        for (key, _) in rows.into_iter().take(excess) {
            self.storage.delete(&key)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn addr(ma: &str) -> String {
        ma.to_string()
    }

    #[test]
    fn block_and_query_until_expiry() {
        let mut storage = MemoryStorage::new();
        let mut bl = AddrBlacklistStore::new(&mut storage);
        assert!(!bl.is_blocked(&addr("/ip4/1.2.3.4/tcp/15002"), 0).unwrap());
        bl.block(&addr("/ip4/1.2.3.4/tcp/15002"), 1000, 10_000).unwrap();
        assert!(bl.is_blocked(&addr("/ip4/1.2.3.4/tcp/15002"), 5000).unwrap());
        // TTL 过期后不再拦截
        assert!(!bl.is_blocked(&addr("/ip4/1.2.3.4/tcp/15002"), 11_001).unwrap());
    }

    #[test]
    fn raw_and_p2p_variant_share_entry() {
        let mut storage = MemoryStorage::new();
        let mut bl = AddrBlacklistStore::new(&mut storage);
        let raw = addr("/ip4/1.2.3.4/tcp/15002");
        let with_peer = addr("/ip4/1.2.3.4/tcp/15002/p2p/12D3KooWExample");
        // raw 写入，带 peer 段形态也能查到
        bl.block(&raw, 0, 10_000).unwrap();
        assert!(bl.is_blocked(&with_peer, 1000).unwrap());
        assert!(bl.is_blocked(&raw, 1000).unwrap());
        // 带 peer 段解锁，raw 形态同步失效
        bl.unblock(&with_peer).unwrap();
        assert!(!bl.is_blocked(&raw, 1000).unwrap());
    }

    #[test]
    fn unblock_removes_entry() {
        let mut storage = MemoryStorage::new();
        let mut bl = AddrBlacklistStore::new(&mut storage);
        bl.block(&addr("/ip4/5.6.7.8/tcp/15002"), 0, 10_000).unwrap();
        assert!(bl.is_blocked(&addr("/ip4/5.6.7.8/tcp/15002"), 0).unwrap());
        bl.unblock(&addr("/ip4/5.6.7.8/tcp/15002")).unwrap();
        assert!(!bl.is_blocked(&addr("/ip4/5.6.7.8/tcp/15002"), 0).unwrap());
    }

    #[test]
    fn non_ip_segments_blocked_by_raw_form() {
        // 含 /p2p 段地址也能 block（剥 base 后存取）
        let mut storage = MemoryStorage::new();
        let mut bl = AddrBlacklistStore::new(&mut storage);
        bl.block(&addr("/ip4/9.9.9.9/tcp/15002/p2p/xyz"), 0, 10_000).unwrap();
        assert!(bl.is_blocked(&addr("/ip4/9.9.9.9/tcp/15002"), 0).unwrap());
    }
}
