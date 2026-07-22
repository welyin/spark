//! 覆盖网邻居池（对齐 overlay-peer-store.ts 与 core/spec/p2p-messages.md §10.1）。
//!
//! 组织无关的长期 peer 地址簿：记录网络层见过的一切 Spark 节点，为 keepalive
//! 提供拨号候选、为 peer-exchange / org-recovery 提供抽样与应答数据。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::constants::{MAX_ADDRESSES_PER_PEER, OVERLAY_POOL_MAX, P2P_OVERLAY_PEER_PREFIX};
use super::Result;

/// 覆盖网邻居来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPeerSource {
    /// 曾经直连成功。
    Connect,
    /// peer-exchange 换来的第三方线索。
    Exchange,
    /// node-announce 签名通告（已验签）。
    Announce,
    /// 组织成员表回填。
    Org,
    /// 局域网发现。
    Mdns,
}

/// 覆盖网邻居记录（TS `OverlayPeerRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayPeerRecord {
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub source: OverlayPeerSource,
    /// announce 验签通过即 true；只升不降（sticky）。
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dial_result: Option<String>,
}

/// 覆盖网邻居池。
pub struct OverlayPeerStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> OverlayPeerStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    fn key(peer_id: &str) -> String {
        format!("{P2P_OVERLAY_PEER_PREFIX}{peer_id}")
    }

    /// 读取单个邻居记录。
    pub fn get(&mut self, peer_id: &str) -> Result<Option<OverlayPeerRecord>> {
        let Some(raw) = self.storage.get(&Self::key(peer_id))? else {
            return Ok(None);
        };
        let parsed: OverlayPeerRecord = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        Ok(Some(parsed))
    }

    fn save(&mut self, record: &OverlayPeerRecord) -> Result<()> {
        self.storage
            .put(&Self::key(&record.peer_id), &serde_json::to_string(record)?)?;
        Ok(())
    }

    /// 记录邻居：按 peerId 合并地址并刷新 lastSeenAt；verified 只升不降。
    pub fn remember(
        &mut self,
        peer_id: &str,
        addresses: &[String],
        source: OverlayPeerSource,
        verified: bool,
        now_ms: i64,
    ) -> Result<()> {
        let normalized = peer_id.trim();
        if normalized.is_empty() {
            return Ok(());
        }
        let existing = self.get(normalized)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        for addr in existing
            .iter()
            .flat_map(|r| r.addresses.iter().cloned())
            .chain(addresses.iter().map(|a| a.trim().to_string()))
        {
            if !addr.is_empty() && seen.insert(addr.clone()) {
                merged.push(addr);
            }
        }
        merged.truncate(MAX_ADDRESSES_PER_PEER);

        self.save(&OverlayPeerRecord {
            peer_id: normalized.to_string(),
            addresses: merged,
            first_seen_at: existing.as_ref().map_or(now_ms, |r| r.first_seen_at),
            last_seen_at: now_ms,
            source,
            verified: existing.as_ref().is_some_and(|r| r.verified) || verified,
            last_dial_result: existing.and_then(|r| r.last_dial_result),
        })?;
        self.evict_if_needed()?;
        Ok(())
    }

    /// 记录一次拨号结果（仅影响排序提示，不触发淘汰）。
    pub fn mark_dial_result(&mut self, peer_id: &str, success: bool) -> Result<()> {
        let Some(mut existing) = self.get(peer_id)? else {
            return Ok(());
        };
        existing.last_dial_result = Some(if success { "success" } else { "failure" }.to_string());
        self.save(&existing)
    }

    /// 全量列出。
    pub fn list_all(&mut self) -> Result<Vec<OverlayPeerRecord>> {
        let rows = self.storage.scan(&ScanOptions::prefix(P2P_OVERLAY_PEER_PREFIX))?;
        let mut records = Vec::new();
        for (_, value) in rows {
            if let Ok(record) = serde_json::from_str::<OverlayPeerRecord>(&value) {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// 抽取拨号候选：verified 优先，其余按 lastSeenAt 降序；排除给定 peerId。
    pub fn sample_dial_candidates(
        &mut self,
        exclude: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<OverlayPeerRecord>> {
        let mut all = self.list_all()?;
        all.retain(|r| !exclude.contains(&r.peer_id) && !r.addresses.is_empty());
        sort_for_sample(&mut all);
        all.truncate(limit);
        Ok(all)
    }

    /// peer-exchange 应答抽样：排除请求方与陈旧条目（14 天窗口）。
    pub fn sample_for_exchange(
        &mut self,
        exclude_peer_id: Option<&str>,
        want: usize,
        now_ms: i64,
        max_age_ms: i64,
    ) -> Result<Vec<OverlayPeerRecord>> {
        let cutoff = now_ms - max_age_ms;
        let mut all = self.list_all()?;
        all.retain(|r| {
            Some(r.peer_id.as_str()) != exclude_peer_id
                && !r.addresses.is_empty()
                && r.last_seen_at >= cutoff
        });
        sort_for_sample(&mut all);
        all.truncate(want);
        Ok(all)
    }

    /// 容量淘汰：超限时优先淘汰最久未见的未验证条目；全部已验证才淘汰验证条目。
    fn evict_if_needed(&mut self) -> Result<()> {
        let mut all = self.list_all()?;
        if all.len() <= OVERLAY_POOL_MAX {
            return Ok(());
        }
        let excess = all.len() - OVERLAY_POOL_MAX;
        // 淘汰序：未验证在前，同组内最久未见在前
        all.sort_by(|a, b| match (a.verified, b.verified) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => a.last_seen_at.cmp(&b.last_seen_at),
        });
        for victim in all.into_iter().take(excess) {
            self.storage.delete(&Self::key(&victim.peer_id))?;
        }
        Ok(())
    }
}

/// 抽样排序：verified 优先、其余按 lastSeenAt 降序。
fn sort_for_sample(records: &mut [OverlayPeerRecord]) {
    records.sort_by(|a, b| match (a.verified, b.verified) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => b.last_seen_at.cmp(&a.last_seen_at),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn addr(i: usize) -> Vec<String> {
        vec![format!("/ip4/10.0.0.{i}/tcp/15002/ws")]
    }

    #[test]
    fn remember_merges_and_caps_addresses() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        store.remember("p1", &addr(1), OverlayPeerSource::Connect, false, 100).unwrap();
        store.remember("p1", &addr(2), OverlayPeerSource::Exchange, false, 200).unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.first_seen_at, 100);
        assert_eq!(rec.last_seen_at, 200);
        assert_eq!(rec.addresses.len(), 2);
        assert_eq!(rec.source, OverlayPeerSource::Exchange);
        // 重复地址去重
        store.remember("p1", &addr(1), OverlayPeerSource::Connect, false, 300).unwrap();
        assert_eq!(store.get("p1").unwrap().unwrap().addresses.len(), 2);
    }

    #[test]
    fn verified_is_sticky() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        store.remember("p1", &addr(1), OverlayPeerSource::Announce, true, 100).unwrap();
        store.remember("p1", &addr(2), OverlayPeerSource::Exchange, false, 200).unwrap();
        assert!(store.get("p1").unwrap().unwrap().verified);
    }

    #[test]
    fn sample_prefers_verified_then_recency() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        store.remember("old-verified", &addr(1), OverlayPeerSource::Announce, true, 100).unwrap();
        store.remember("new-unverified", &addr(2), OverlayPeerSource::Exchange, false, 900).unwrap();
        store.remember("old-unverified", &addr(3), OverlayPeerSource::Exchange, false, 200).unwrap();
        let sample = store.sample_dial_candidates(&HashSet::new(), 10).unwrap();
        let order: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
        assert_eq!(order, vec!["old-verified", "new-unverified", "old-unverified"]);
        // 排除 + 无地址条目
        store.remember("no-addr", &[], OverlayPeerSource::Exchange, false, 1000).unwrap();
        let sample = store
            .sample_dial_candidates(&HashSet::from(["old-verified".to_string()]), 10)
            .unwrap();
        let order: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
        assert_eq!(order, vec!["new-unverified", "old-unverified"]);
    }

    #[test]
    fn exchange_sample_respects_age_window() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        let now = 1_000_000i64;
        store.remember("fresh", &addr(1), OverlayPeerSource::Connect, false, now - 1000).unwrap();
        store.remember("stale", &addr(2), OverlayPeerSource::Connect, false, now - 15 * 24 * 60 * 60 * 1000).unwrap();
        let sample = store
            .sample_for_exchange(Some("fresh"), 16, now, 14 * 24 * 60 * 60 * 1000)
            .unwrap();
        assert!(sample.is_empty()); // fresh 被排除（请求方），stale 超窗
    }

    #[test]
    fn eviction_drops_unverified_first() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        // 填满 200：199 个未验证 + 1 个最旧的已验证
        store.remember("verified-oldest", &addr(0), OverlayPeerSource::Announce, true, 1).unwrap();
        for i in 1..200usize {
            store
                .remember(&format!("peer-{i}"), &addr(i), OverlayPeerSource::Exchange, false, (i * 10) as i64)
                .unwrap();
        }
        assert_eq!(store.list_all().unwrap().len(), 200);
        // 再插一个 → 淘汰最久未见的未验证（peer-1，lastSeenAt=10）
        store.remember("newcomer", &addr(999), OverlayPeerSource::Exchange, false, 1_000_000).unwrap();
        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 200);
        assert!(all.iter().any(|r| r.peer_id == "newcomer"));
        assert!(all.iter().any(|r| r.peer_id == "verified-oldest"));
        assert!(!all.iter().any(|r| r.peer_id == "peer-1"));
    }

    #[test]
    fn eviction_falls_back_to_verified_when_all_verified() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        for i in 0..201usize {
            store
                .remember(
                    &format!("peer-{i:03}"),
                    &addr(i),
                    OverlayPeerSource::Announce,
                    true,
                    (i * 10) as i64,
                )
                .unwrap();
        }
        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 200);
        assert!(!all.iter().any(|r| r.peer_id == "peer-000")); // 最旧被淘汰
    }
}
