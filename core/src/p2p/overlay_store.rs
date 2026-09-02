//! 覆盖网邻居池（对齐 overlay-peer-store.ts 与 core/spec/p2p-messages.md §10.1）。
//!
//! 组织无关的长期 peer 地址簿：记录网络层见过的一切 Spark 节点，为 keepalive
//! 提供拨号候选、为 peer-exchange / org-recovery 提供抽样与应答数据。
//!
//! 地址记分卡（M9）：每条地址附带 `AddrScore`（success/valid 证据），拨号目标
//! 按其排序、超限按排序淘汰垫底、确定性错误硬删。`addresses: Vec<String>` 保持
//! 原样不动（协议守护：FriendRecord/邻居池记录经 pdsync 同步），记分卡以可选
//! 附加字段 `addr_meta` 携带——旧数据缺省为零分自然迁入；pdsync 对端为旧版本
//! 时转发/覆盖会丢 `addr_meta`（serde 忽略未知字段后重写），接受该降级，零分
//! 重建即可。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;
use super::addr_blacklist::AddrBlacklistStore;
use super::constants::{
    MAX_ADDRESSES_PER_PEER, OVERLAY_DIAL_CANDIDATE_MAX_AGE_MS, OVERLAY_POOL_MAX,
    P2P_OVERLAY_PEER_PREFIX,
};
use super::peer_targets::filter_dial_candidate;

/// 单条地址的记分卡（M9）：成功/有效两类证据，供拨号目标排序与淘汰。
///
/// - **success**：该地址经 dialer 方向真实连上（`ConnectionEstablished`）。
/// - **valid**：没通过它连上、但证据表明活着（签名 node-announce / 签名 DHT
///   节点记录 / peer-exchange 采样中出现）。
///
/// 失败不计分不压序（不搞失败惩罚，靠自然淘汰）；确定性错误（WrongPeerId /
/// 命中本机监听地址）直接硬删地址本身。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddrScore {
    /// 该地址成功拨号连上次数。
    #[serde(default)]
    pub success_count: u32,
    /// 最近一次成功连上时刻（i64 ms）。
    #[serde(default)]
    pub last_success_at: i64,
    /// 该地址被验证「活着」的次数（签名通告/DHT 记录/peer-exchange 采样）。
    #[serde(default)]
    pub valid_count: u32,
    /// 最近一次被验证「活着」的时刻（i64 ms）。
    #[serde(default)]
    pub last_valid_at: i64,
}

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
    /// 地址记分卡（M9）：address → 该地址的 success/valid 证据。可选附加字段：
    /// 旧数据缺省为零分自然迁入；pdsync 对端为旧版本时转发/覆盖会丢此字段，
    /// 接受降级（零分重建）。`addresses` 线形保持不变。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub addr_meta: HashMap<String, AddrScore>,
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
    ///
    /// 自过滤（M9）：`self_id` 为本机 peerId、`self_addrs` 为本机当前监听地址
    /// 集合。二者在写入时被排除——本机 peerId 的记录不应入池，本机监听地址
    /// （含多实例同机开发的 ::1 / 本机 LAN IP）不得记为对端地址（脏数据源）。
    /// 传入 `None`/空则不做过滤（测试或无需过滤的场景）。
    pub fn remember(
        &mut self,
        peer_id: &str,
        addresses: &[String],
        source: OverlayPeerSource,
        verified: bool,
        now_ms: i64,
        self_id: Option<&str>,
        self_addrs: &HashSet<String>,
    ) -> Result<()> {
        let normalized = peer_id.trim();
        if normalized.is_empty() {
            return Ok(());
        }
        // 自过滤：本机 peerId 不入池
        if self_id.is_some_and(|s| s == normalized) {
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
            if addr.is_empty() || self_addrs.contains(&addr) {
                continue;
            }
            // WrongPeerId 黑名单（S5）：命中且在 TTL 内 → 跳过该地址，防「未升级
            // 对端仍广播污染」remember 回灌（wrong-peer-id-address-pollution §2.9）。
            // 集中到 remember 一处，覆盖所有 remember 调用点。
            if AddrBlacklistStore::new(self.storage).is_blocked(&addr, now_ms)? {
                continue;
            }
            if seen.insert(addr.clone()) {
                merged.push(addr);
            }
        }
        // 超限淘汰垫底：先按记分卡排序（成功>有效>零分，同级静态优先级），
        // 再截断——新增/低分地址在满额时先被挤出。排序只决定保留次序，
        // 记录内 `addresses` 的最终顺序在拨号目标构建时另行确定。
        let existing_meta = existing
            .as_ref()
            .map(|r| &r.addr_meta)
            .cloned()
            .unwrap_or_default();
        sort_by_addr_rank(&mut merged, &existing_meta);
        merged.truncate(MAX_ADDRESSES_PER_PEER);
        // 合并 addr_meta（保留既有记分卡；新增地址缺省零分）
        let mut addr_meta = existing
            .as_ref()
            .map(|r| r.addr_meta.clone())
            .unwrap_or_default();
        addr_meta.retain(|k, _| merged.iter().any(|a| a == k));

        self.save(&OverlayPeerRecord {
            peer_id: normalized.to_string(),
            addresses: merged,
            first_seen_at: existing.as_ref().map_or(now_ms, |r| r.first_seen_at),
            last_seen_at: now_ms,
            source,
            verified: existing.as_ref().is_some_and(|r| r.verified) || verified,
            last_dial_result: existing.and_then(|r| r.last_dial_result),
            addr_meta,
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

    /// 记分卡：某地址 dialer 方向成功连上（`ConnectionEstablished`）。地址不在
    /// 记录中则忽略（只对已知地址记账）。
    pub fn mark_addr_success(&mut self, peer_id: &str, addr: &str, now_ms: i64) -> Result<()> {
        let Some(mut existing) = self.get(peer_id)? else {
            return Ok(());
        };
        let Some(score) = existing.addr_meta.get_mut(addr) else {
            // 该地址此前未入池（如监听方向连上但未记录）：仍记一笔 success
            existing.addr_meta.insert(
                addr.to_string(),
                AddrScore {
                    success_count: 1,
                    last_success_at: now_ms,
                    ..Default::default()
                },
            );
            return self.save(&existing);
        };
        score.success_count = score.success_count.saturating_add(1);
        score.last_success_at = now_ms;
        self.save(&existing)
    }

    /// 记分卡：一组地址被签名 node-announce / 签名 DHT 节点记录 / peer-exchange
    /// 采样验证「活着」（valid 证据）。仅对记录中已存在的地址记账。
    pub fn mark_addrs_valid(&mut self, peer_id: &str, addrs: &[String], now_ms: i64) -> Result<()> {
        let Some(mut existing) = self.get(peer_id)? else {
            return Ok(());
        };
        for addr in addrs {
            if !existing.addresses.iter().any(|a| a == addr) {
                continue;
            }
            let score = existing.addr_meta.entry(addr.clone()).or_default();
            score.valid_count = score.valid_count.saturating_add(1);
            score.last_valid_at = now_ms;
        }
        self.save(&existing)
    }

    /// 记分卡硬删：确定性错误（WrongPeerId / 命中本机监听地址）从该 peer 记录
    /// 删除此地址（连同记分卡）。删除后地址为空时同时清理该地址条目。
    pub fn remove_addr(&mut self, peer_id: &str, addr: &str) -> Result<()> {
        let Some(mut existing) = self.get(peer_id)? else {
            return Ok(());
        };
        let before = existing.addresses.len();
        existing.addresses.retain(|a| a != addr);
        if existing.addresses.len() == before {
            return Ok(()); // 地址本不存在，无需保存
        }
        existing.addr_meta.remove(addr);
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

    /// 抽取拨号候选（补拨/自举用）：verified 优先，其余按 lastSeenAt 降序；
    /// 排除给定 peerId；过滤超过 [`OVERLAY_DIAL_CANDIDATE_MAX_AGE_MS`] 新鲜度
    /// 窗口的陈旧条目；复用 [`filter_dial_candidate`] 对候选地址做与拨号路径
    /// 一致的清洗（剔通配/无效地址——本机通配监听 0.0.0.0 因此不会当选）。
    pub fn sample_dial_candidates(
        &mut self,
        exclude: &HashSet<String>,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<OverlayPeerRecord>> {
        let cutoff = now_ms - OVERLAY_DIAL_CANDIDATE_MAX_AGE_MS;
        let is_android = cfg!(target_os = "android");
        let mut all = self.list_all()?;
        all.retain(|r| {
            if exclude.contains(&r.peer_id) || r.last_seen_at < cutoff {
                return false;
            }
            // 地址清洗与拨号路径收敛（filter_dial_candidate）：剔除通配等
            // 不可拨地址；若该 peer 无任何可拨地址则整体剔除。
            let dialable = r
                .addresses
                .iter()
                .any(|a| filter_dial_candidate(a, is_android).is_some());
            dialable
        });
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

/// 抽样排序（M8 排序制）：verified 优先 → 最近一次拨号非失败优先（失败的
/// 沉底）→ 其余按 lastSeenAt 降序。去重不靠失败记忆：无周期触发，失败的
/// 候选沉底后自然不会被后续事件轮优先捞到；该 peer 有新信息（地址更新/
/// 重新被看到）刷新 lastSeenAt 后重新浮上来。
fn sort_for_sample(records: &mut [OverlayPeerRecord]) {
    fn dial_failed(r: &OverlayPeerRecord) -> bool {
        r.last_dial_result.as_deref() == Some("failure")
    }
    records.sort_by(|a, b| {
        (b.verified, !dial_failed(b), b.last_seen_at).cmp(&(
            a.verified,
            !dial_failed(a),
            a.last_seen_at,
        ))
    });
}

/// 某地址是否具备成功/有效证据（非零分）。
fn has_evidence(score: &AddrScore) -> bool {
    score.success_count > 0 || score.valid_count > 0
}

/// 按记分卡证据 + 静态优先级排序地址（M9）：记分卡证据优先（`last_success_at`
/// 降序 → `success_count` 降序 → `last_valid_at` 降序）；零分/同分级内按静态
/// 优先级（IPv6 公网 tcp > IPv6 公网 ws > IPv4 tcp > IPv4 ws > loopback/link-local）。
/// 用于超限淘汰（保留高证据地址）与拨号目标构建（高证据地址在前）。
pub(crate) fn sort_by_addr_rank(addrs: &mut [String], meta: &HashMap<String, AddrScore>) {
    let score = |addr: &str| meta.get(addr).cloned().unwrap_or_default();
    let static_rank = crate::p2p::peer_targets::addr_static_rank;
    // 降序：高证据 / 高静态优先级在前
    addrs.sort_by(|a, b| {
        let sa = score(a);
        let sb = score(b);
        let ea = has_evidence(&sa);
        let eb = has_evidence(&sb);
        match (ea, eb) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                // 分数部分降序（last_success_at → success_count → last_valid_at），
                // 静态优先级升序（rank 低 = 优先）作同分 tiebreak。
                let score_cmp = (sa.last_success_at, sa.success_count, sa.last_valid_at)
                    .cmp(&(sb.last_success_at, sb.success_count, sb.last_valid_at))
                    .reverse();
                score_cmp.then_with(|| static_rank(a).cmp(&static_rank(b)))
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn score_sort_prefers_success_over_valid_over_zero() {
        let mut addrs = vec![
            "/ip4/1.1.1.1/tcp/15002".to_string(), // 零分
            "/ip4/2.2.2.2/tcp/15002".to_string(), // valid
            "/ip4/3.3.3.3/tcp/15002".to_string(), // success
        ];
        let mut meta = HashMap::new();
        meta.insert(
            "/ip4/2.2.2.2/tcp/15002".to_string(),
            AddrScore {
                valid_count: 1,
                last_valid_at: 200,
                ..Default::default()
            },
        );
        meta.insert(
            "/ip4/3.3.3.3/tcp/15002".to_string(),
            AddrScore {
                success_count: 1,
                last_success_at: 300,
                ..Default::default()
            },
        );
        sort_by_addr_rank(&mut addrs, &meta);
        assert_eq!(addrs[0], "/ip4/3.3.3.3/tcp/15002", "success 优先");
        assert_eq!(addrs[1], "/ip4/2.2.2.2/tcp/15002", "valid 其次");
        assert_eq!(addrs[2], "/ip4/1.1.1.1/tcp/15002", "零分垫底");
    }

    #[test]
    fn zero_score_sorted_by_static_priority_v6_tcp_over_v4_ws() {
        let mut addrs = vec![
            "/ip4/1.1.1.1/tcp/15002/ws".to_string(),     // IPv4 ws
            "/ip6/2408:8207:1::1/tcp/15002".to_string(), // IPv6 tcp
            "/ip4/2.2.2.2/tcp/15002".to_string(),        // IPv4 tcp
        ];
        sort_by_addr_rank(&mut addrs, &HashMap::new());
        assert_eq!(
            addrs[0], "/ip6/2408:8207:1::1/tcp/15002",
            "IPv6 公网 tcp 优先"
        );
        assert_eq!(addrs[1], "/ip4/2.2.2.2/tcp/15002", "IPv4 tcp 其次");
        assert_eq!(addrs[2], "/ip4/1.1.1.1/tcp/15002/ws", "IPv4 ws 最后");
    }

    #[test]
    fn over_limit_evicts_lowest_scored() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        // 填满 MAX_ADDRESSES_PER_PEER 且每个带 score：高分的保留
        let addrs: Vec<String> = (1..=MAX_ADDRESSES_PER_PEER)
            .map(|i| format!("/ip4/10.0.0.{i}/tcp/15002"))
            .collect();
        store
            .remember(
                "p1",
                &addrs,
                OverlayPeerSource::Connect,
                false,
                0,
                None,
                &HashSet::new(),
            )
            .unwrap();
        // 高分为最后一个地址（10.0.0.20）
        let high = "/ip4/10.0.0.20/tcp/15002";
        store.mark_addr_success("p1", high, 1000).unwrap();
        // 再记一个地址，超限 → 淘汰最低分（未标记的新地址）
        store
            .remember(
                "p1",
                &["/ip4/10.9.9.9/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                0,
                None,
                &HashSet::new(),
            )
            .unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.addresses.len(), MAX_ADDRESSES_PER_PEER, "超限截断");
        assert!(
            rec.addresses.iter().any(|a| a == high),
            "高证据地址被保留（淘汰垫底）"
        );
        assert!(
            !rec.addresses
                .contains(&"/ip4/10.9.9.9/tcp/15002".to_string()),
            "新零分地址在满额时被挤出"
        );
    }

    #[test]
    fn mark_addr_success_and_remove_addr() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        store
            .remember(
                "p1",
                &["/ip4/1.1.1.1/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                0,
                None,
                &HashSet::new(),
            )
            .unwrap();
        store
            .mark_addr_success("p1", "/ip4/1.1.1.1/tcp/15002", 500)
            .unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        let score = rec.addr_meta.get("/ip4/1.1.1.1/tcp/15002").unwrap();
        assert_eq!(score.success_count, 1);
        assert_eq!(score.last_success_at, 500);
        // WrongPeerId 硬删
        store.remove_addr("p1", "/ip4/1.1.1.1/tcp/15002").unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert!(rec.addresses.is_empty());
        assert!(rec.addr_meta.is_empty());
    }

    #[test]
    fn remember_skips_blacklisted_addr() {
        // S5：黑名单命中且在 TTL 内的地址 remember 跳过（防未升级对端回灌）
        let mut storage = MemoryStorage::new();
        // 先写黑名单（模拟 M9 删除污染地址时写入）
        {
            let mut bl = AddrBlacklistStore::new(&mut storage);
            bl.block("/ip4/192.168.31.218/tcp/15002", 0, 10_000)
                .unwrap();
        }
        let mut store = OverlayPeerStore::new(&mut storage);
        store
            .remember(
                "p1",
                &[
                    "/ip4/192.168.31.218/tcp/15002".to_string(), // 黑名单 → 跳过
                    "/ip4/8.8.8.8/tcp/15002".to_string(),        // 正常 → 保留
                ],
                OverlayPeerSource::Announce,
                true,
                5_000,
                None,
                &HashSet::new(),
            )
            .unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.addresses, vec!["/ip4/8.8.8.8/tcp/15002".to_string()]);
    }

    #[test]
    fn remember_reaccepts_after_blacklist_expiry() {
        // TTL 过期后黑名单不再拦截，地址可重新 remember（防永久误伤）
        let mut storage = MemoryStorage::new();
        {
            let mut bl = AddrBlacklistStore::new(&mut storage);
            bl.block("/ip4/1.1.1.1/tcp/15002", 0, 10_000).unwrap();
        }
        let mut store = OverlayPeerStore::new(&mut storage);
        // TTL 内：跳过
        store
            .remember(
                "p1",
                &["/ip4/1.1.1.1/tcp/15002".to_string()],
                OverlayPeerSource::Announce,
                true,
                5_000,
                None,
                &HashSet::new(),
            )
            .unwrap();
        assert!(store.get("p1").unwrap().unwrap().addresses.is_empty());
        // 超过 TTL：重新接受
        store
            .remember(
                "p1",
                &["/ip4/1.1.1.1/tcp/15002".to_string()],
                OverlayPeerSource::Announce,
                true,
                10_001,
                None,
                &HashSet::new(),
            )
            .unwrap();
        let rec = store.get("p1").unwrap().unwrap();
        assert_eq!(rec.addresses, vec!["/ip4/1.1.1.1/tcp/15002".to_string()]);
    }

    #[test]
    fn self_filter_excludes_self_id_and_self_addrs() {
        let mut storage = MemoryStorage::new();
        let mut store = OverlayPeerStore::new(&mut storage);
        let self_addrs: HashSet<String> = ["/ip4/192.168.1.5/tcp/15002".to_string()]
            .into_iter()
            .collect();
        // 本机 peerId 不入池
        store
            .remember(
                "self-peer",
                &["/ip4/9.9.9.9/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                0,
                Some("self-peer"),
                &self_addrs,
            )
            .unwrap();
        assert!(
            store.get("self-peer").unwrap().is_none(),
            "本机 peerId 不入池"
        );
        // 本机监听地址不得记为对端地址
        store
            .remember(
                "other",
                &[
                    "/ip4/192.168.1.5/tcp/15002".to_string(),
                    "/ip4/8.8.8.8/tcp/15002".to_string(),
                ],
                OverlayPeerSource::Connect,
                false,
                0,
                Some("self-peer"),
                &self_addrs,
            )
            .unwrap();
        let rec = store.get("other").unwrap().unwrap();
        assert_eq!(rec.addresses, vec!["/ip4/8.8.8.8/tcp/15002".to_string()]);
    }
}
