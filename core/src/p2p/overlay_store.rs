//! 覆盖网邻居池（对齐 overlay-peer-store.ts 与 core/spec/p2p-messages.md §10.1）。
//!
//! 组织无关的长期 peer 地址簿：记录网络层见过的一切 Spark 节点，为 keepalive
//! 提供拨号候选、为 peer-exchange / org-recovery 提供抽样与应答数据。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;
use super::constants::{MAX_ADDRESSES_PER_PEER, OVERLAY_POOL_MAX, P2P_OVERLAY_PEER_PREFIX};

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
        let rows = self
            .storage
            .scan(&ScanOptions::prefix(P2P_OVERLAY_PEER_PREFIX))?;
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
