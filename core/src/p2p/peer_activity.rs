//! 节点活跃度记录（对齐 peer-activity-store.ts 与 core/spec/p2p-messages.md §10.2）。
//!
//! 记录每个 peer 的连接成功/失败与在线累计时长，为组织候选拨号提供打分排序。

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::constants::{
    PEER_ACTIVITY_FAILURE_PURGE_THRESHOLD, PEER_ACTIVITY_FAILURE_WEIGHT_MS,
    PEER_ACTIVITY_SUCCESS_WEIGHT_MS, P2P_PEER_RECORD_PREFIX,
};
use super::Result;
use super::peer_targets::{PeerNodeInfo, extract_peer_id};

/// 节点活跃度记录（TS `PeerActivityRecord`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerActivityRecord {
    pub peer_id: String,
    #[serde(default)]
    pub addresses: Vec<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub last_connected_at: Option<i64>,
    pub last_disconnected_at: Option<i64>,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failure_count: Option<u64>,
    #[serde(default)]
    pub cumulative_connected_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_session_connected_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 观察结果类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeObservation {
    /// 仅刷地址与 lastSeenAt。
    Seen,
    /// 连接成功。
    Success,
    /// 连接失败（附错误文本）。
    Failure,
}

/// 打分公式：`cumulativeConnectedMs + success*60000 - failure*30000 - max(0, now-lastSeenAt)`。
pub fn compute_priority(record: &PeerActivityRecord, now_ms: i64) -> i64 {
    let recency_penalty = (now_ms - record.last_seen_at).max(0);
    record.cumulative_connected_ms
        + record.success_count as i64 * PEER_ACTIVITY_SUCCESS_WEIGHT_MS
        - record.failure_count as i64 * PEER_ACTIVITY_FAILURE_WEIGHT_MS
        - recency_penalty
}

/// 无记录候选的最低分（JS `Number.MIN_SAFE_INTEGER`）。
pub const NO_RECORD_PRIORITY: i64 = -9_007_199_254_740_991;

/// 节点活跃度仓库。
pub struct PeerActivityStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> PeerActivityStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    fn key(peer_id: &str) -> String {
        format!("{P2P_PEER_RECORD_PREFIX}{peer_id}")
    }

    pub fn get(&mut self, peer_id: &str) -> Result<Option<PeerActivityRecord>> {
        if peer_id.is_empty() {
            return Ok(None);
        }
        let Some(raw) = self.storage.get(&Self::key(peer_id))? else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&raw).ok())
    }

    fn save(&mut self, record: &PeerActivityRecord) -> Result<()> {
        self.storage
            .put(&Self::key(&record.peer_id), &serde_json::to_string(record)?)?;
        Ok(())
    }

    fn new_record(peer_id: &str, now_ms: i64) -> PeerActivityRecord {
        PeerActivityRecord {
            peer_id: peer_id.to_string(),
            first_seen_at: now_ms,
            last_seen_at: now_ms,
            ..Default::default()
        }
    }

    /// 兼容旧数据：缺 consecutiveFailureCount 时按历史统计推断基线。
    fn resolve_failure_streak(record: &PeerActivityRecord) -> u64 {
        if let Some(streak) = record.consecutive_failure_count {
            return streak;
        }
        if record.success_count == 0 { record.failure_count } else { 0 }
    }

    /// "完全不活跃"：无成功历史、无在线时长、无当前会话、无 lastConnectedAt。
    fn is_completely_inactive(record: &PeerActivityRecord) -> bool {
        record.success_count == 0
            && record.cumulative_connected_ms == 0
            && record.current_session_connected_at.is_none()
            && record.last_connected_at.is_none()
    }

    /// 记录节点观察结果；连续失败 ≥10 且完全不活跃时整条删除。
    pub fn remember_node_info(
        &mut self,
        node_info: &PeerNodeInfo,
        result: NodeObservation,
        error: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        let Some(peer_id) = extract_peer_id(node_info) else {
            return Ok(());
        };
        let mut next = self
            .get(&peer_id)?
            .unwrap_or_else(|| Self::new_record(&peer_id, now_ms));

        // 合并地址（去重、trim、滤空，不截断——TS 未截断）
        for addr in &node_info.addresses {
            let addr = addr.trim();
            if !addr.is_empty() && !next.addresses.iter().any(|a| a == addr) {
                next.addresses.push(addr.to_string());
            }
        }
        next.last_seen_at = now_ms;

        match result {
            NodeObservation::Seen => {}
            NodeObservation::Success => {
                next.success_count += 1;
                next.last_connected_at = Some(now_ms);
                next.consecutive_failure_count = Some(0);
            }
            NodeObservation::Failure => {
                let streak = Self::resolve_failure_streak(&next);
                next.failure_count += 1;
                next.consecutive_failure_count = Some(streak + 1);
                next.last_error = error.map(ToString::to_string);
                if streak + 1 >= u64::from(PEER_ACTIVITY_FAILURE_PURGE_THRESHOLD)
                    && Self::is_completely_inactive(&next)
                {
                    self.storage.delete(&Self::key(&peer_id))?;
                    return Ok(());
                }
            }
        }
        self.save(&next)
    }

    /// 标记已连接：写入当前会话起点（幂等）。
    pub fn mark_connected(&mut self, peer_id: &str, now_ms: i64) -> Result<()> {
        let mut next = self
            .get(peer_id)?
            .unwrap_or_else(|| Self::new_record(peer_id, now_ms));
        next.last_seen_at = now_ms;
        next.last_connected_at = Some(now_ms);
        if next.current_session_connected_at.is_none() {
            next.current_session_connected_at = Some(now_ms);
        }
        self.save(&next)
    }

    /// 标记已断开：累计在线时长并清除会话起点。
    pub fn mark_disconnected(&mut self, peer_id: &str, now_ms: i64) -> Result<()> {
        let Some(mut existing) = self.get(peer_id)? else {
            return Ok(());
        };
        if let Some(session_start) = existing.current_session_connected_at.take() {
            existing.cumulative_connected_ms += (now_ms - session_start).max(0);
        }
        existing.last_seen_at = now_ms;
        existing.last_disconnected_at = Some(now_ms);
        self.save(&existing)
    }

    /// 全量列出。
    pub fn list_all(&mut self) -> Result<Vec<PeerActivityRecord>> {
        let rows = self.storage.scan(&ScanOptions::prefix(P2P_PEER_RECORD_PREFIX))?;
        Ok(rows
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str(&v).ok())
            .collect())
    }

    /// 清空全部节点活跃度记录（peer-activity-store.ts:280-295 `clearAllRecords`；
    /// ipc `p2p-clear-peer-records` 用语）。返回删除条数。
    pub fn clear_all_records(&mut self) -> Result<usize> {
        let rows = self.storage.scan(&ScanOptions::prefix(P2P_PEER_RECORD_PREFIX))?;
        let count = rows.len();
        for (key, _) in rows {
            self.storage.delete(&key)?;
        }
        Ok(count)
    }

    /// 按活跃度对候选排序（高分优先；无记录候选排最后）。
    pub fn sort_candidates_by_priority(
        &mut self,
        candidates: &[PeerNodeInfo],
        now_ms: i64,
    ) -> Result<Vec<PeerNodeInfo>> {
        let records = self.list_all()?;
        let score = |info: &PeerNodeInfo| -> i64 {
            extract_peer_id(info)
                .and_then(|pid| records.iter().find(|r| r.peer_id == pid).map(|r| compute_priority(r, now_ms)))
                .unwrap_or(NO_RECORD_PRIORITY)
        };
        let mut sorted = candidates.to_vec();
        sorted.sort_by_key(|info| std::cmp::Reverse(score(info)));
        Ok(sorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn info(peer_id: &str, i: usize) -> PeerNodeInfo {
        PeerNodeInfo {
            peer_id: Some(peer_id.to_string()),
            addresses: vec![format!("/ip4/10.0.0.{i}/tcp/15002/ws")],
        }
    }

    #[test]
    fn remember_seen_success_failure() {
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        store.remember_node_info(&info("p1", 1), NodeObservation::Seen, None, 100).unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.success_count, 0);
        assert_eq!(rec.last_seen_at, 100);

        store.remember_node_info(&info("p1", 1), NodeObservation::Success, None, 200).unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.success_count, 1);
        assert_eq!(rec.last_connected_at, Some(200));
        assert_eq!(rec.consecutive_failure_count, Some(0));

        store
            .remember_node_info(&info("p1", 1), NodeObservation::Failure, Some("boom"), 300)
            .unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.failure_count, 1);
        assert_eq!(rec.consecutive_failure_count, Some(1));
        assert_eq!(rec.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn failure_streak_baseline_for_legacy_records() {
        // 旧数据缺 consecutiveFailureCount：successCount==0 → 基线=failureCount
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        let mut legacy = PeerActivityStore::new_record("p1", 0);
        legacy.failure_count = 3;
        legacy.consecutive_failure_count = None;
        store.save(&legacy).unwrap();
        store.remember_node_info(&info("p1", 1), NodeObservation::Failure, None, 100).unwrap();
        assert_eq!(store.get("p1").unwrap().unwrap().consecutive_failure_count, Some(4));
    }

    #[test]
    fn purge_after_ten_failures_when_completely_inactive() {
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        for i in 0..9 {
            store
                .remember_node_info(&info("p1", 1), NodeObservation::Failure, None, 100 + i)
                .unwrap();
            assert!(store.get("p1").unwrap().is_some());
        }
        store
            .remember_node_info(&info("p1", 1), NodeObservation::Failure, None, 200)
            .unwrap();
        assert!(store.get("p1").unwrap().is_none()); // 第 10 次失败触发清除
    }

    #[test]
    fn no_purge_when_previously_active() {
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        store.remember_node_info(&info("p1", 1), NodeObservation::Success, None, 1).unwrap();
        store.mark_connected("p1", 2).unwrap();
        store.mark_disconnected("p1", 100).unwrap();
        for i in 0..10 {
            store
                .remember_node_info(&info("p1", 1), NodeObservation::Failure, None, 200 + i)
                .unwrap();
        }
        assert!(store.get("p1").unwrap().is_some()); // 有成功历史，不清除
    }

    #[test]
    fn session_settlement() {
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        store.mark_connected("p1", 1000).unwrap();
        store.mark_connected("p1", 1500).unwrap(); // 幂等，不重置会话起点
        store.mark_disconnected("p1", 4000).unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.cumulative_connected_ms, 3000);
        assert_eq!(rec.last_disconnected_at, Some(4000));
        assert!(rec.current_session_connected_at.is_none());
    }

    #[test]
    fn priority_formula() {
        let rec = PeerActivityRecord {
            cumulative_connected_ms: 10_000,
            success_count: 2,
            failure_count: 1,
            last_seen_at: 900,
            ..Default::default()
        };
        // 10000 + 2*60000 - 1*30000 - max(0, 1000-900) = 99900
        assert_eq!(compute_priority(&rec, 1000), 99_900);
        // 未来 lastSeenAt 不加分
        assert_eq!(compute_priority(&rec, 500), 100_000);
    }

    #[test]
    fn sort_candidates() {
        let mut storage = MemoryStorage::new();
        let mut store = PeerActivityStore::new(&mut storage);
        store.remember_node_info(&info("good", 1), NodeObservation::Success, None, 1000).unwrap();
        store.remember_node_info(&info("bad", 2), NodeObservation::Failure, None, 1000).unwrap();
        let candidates = vec![info("unknown", 3), info("bad", 2), info("good", 1)];
        let sorted = store.sort_candidates_by_priority(&candidates, 1000).unwrap();
        let order: Vec<String> = sorted.iter().filter_map(|c| c.peer_id.clone()).collect();
        assert_eq!(order, vec!["good", "bad", "unknown"]);
    }
}
