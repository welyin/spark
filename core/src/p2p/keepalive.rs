//! keepalive 拨号候选与恢复触发的纯逻辑（core/spec/p2p-messages.md §8.4/§12）。
//!
//! tick 内的网络动作由 `P2pNode` 事件循环执行；这里只放可单测的判定逻辑。

use std::collections::HashSet;

use super::constants::{
    OVERLAY_DIAL_TARGET, OVERLAY_TICK_DIAL_BUDGET, RECOVERY_COOLDOWN_MS,
    RECOVERY_TRIGGER_CONSECUTIVE_TICKS,
};
use super::peer_targets::PeerNodeInfo;

/// keepalive tick 的组织拨号计划：按打分排序的候选 → (待拨号, 已连接)。
///
/// 对齐 maintainOrganizationNetwork（p2p-node.ts:396-418）：
/// 已连接候选直接归入 connected；未连接的每 tick 最多新拨 3 个（超出跳过，不失败记账）。
pub fn plan_organization_dials(
    sorted_candidates: &[PeerNodeInfo],
    connected_peers: &HashSet<String>,
    max_dials: usize,
) -> (Vec<PeerNodeInfo>, Vec<PeerNodeInfo>) {
    let mut to_dial = Vec::new();
    let mut connected = Vec::new();
    for candidate in sorted_candidates {
        let peer_id = super::peer_targets::extract_peer_id(candidate);
        if let Some(pid) = &peer_id
            && connected_peers.contains(pid)
        {
            connected.push(candidate.clone());
            continue;
        }
        if to_dial.len() < max_dials {
            to_dial.push(candidate.clone());
        }
    }
    (to_dial, connected)
}

/// 覆盖网拨号预算：活跃连接低于目标时补拨，每 tick 预算 2 次。
pub fn overlay_dial_budget(connected_count: usize) -> usize {
    let shortfall = OVERLAY_DIAL_TARGET.saturating_sub(connected_count);
    shortfall.min(OVERLAY_TICK_DIAL_BUDGET)
}

/// peer-exchange 轮选：已连接邻居排序后按游标轮转。
pub fn pick_exchange_target(connected: &HashSet<String>, self_peer_id: &str, cursor: u64) -> Option<String> {
    let mut neighbors: Vec<String> = connected
        .iter()
        .filter(|p| p.as_str() != self_peer_id)
        .cloned()
        .collect();
    if neighbors.is_empty() {
        return None;
    }
    neighbors.sort();
    Some(neighbors[(cursor as usize) % neighbors.len()].clone())
}

/// org-recovery 触发判定（p2p-node.ts:453-476）：
/// "全员不可达"连续 3 个 tick，且距上轮查询 ≥ 10 min（**冷却为全局单值**）。
pub struct RecoveryTrigger {
    dead_tick_count: u32,
    last_query_at: Option<i64>,
}

impl Default for RecoveryTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryTrigger {
    pub fn new() -> Self {
        Self {
            dead_tick_count: 0,
            last_query_at: None,
        }
    }

    /// 每个 keepalive tick 调用：返回本轮是否应发起恢复查询。
    /// 返回 true 时已记录本轮查询时间（调用方随后执行查询）。
    pub fn on_tick(&mut self, org_unreachable: bool, now_ms: i64) -> bool {
        if !org_unreachable {
            self.dead_tick_count = 0;
            return false;
        }
        self.dead_tick_count += 1;
        if self.dead_tick_count < RECOVERY_TRIGGER_CONSECUTIVE_TICKS {
            return false;
        }
        if let Some(last) = self.last_query_at
            && now_ms - last < RECOVERY_COOLDOWN_MS
        {
            return false;
        }
        self.last_query_at = Some(now_ms);
        true
    }

    /// 撤销本轮冷却记录：触发条件满足但实际未发起查询（无恢复视图/无邻居）
    /// 时调用，对齐 TS `lastRecoveryQueryAt` 仅在真正查询时才更新的语义
    /// （p2p-node.ts:479-483 的 view/neighbors 前置检查在赋值之前）。
    pub fn reset_cooldown(&mut self) {
        self.last_query_at = None;
    }
}

/// 恢复候选合并去重（每轮最多 16 条、最多拨号 4 个候选）。
pub fn plan_recovery_dials(candidates: &[PeerNodeInfo], max_dials: usize) -> Vec<PeerNodeInfo> {
    let mut attempted: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let key = candidate
            .peer_id
            .clone()
            .unwrap_or_else(|| candidate.addresses.join("|"));
        if key.is_empty() || attempted.contains(&key) || out.len() >= max_dials {
            continue;
        }
        attempted.insert(key);
        out.push(candidate.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(peer_id: &str) -> PeerNodeInfo {
        PeerNodeInfo {
            peer_id: Some(peer_id.to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/1/ws".to_string()],
        }
    }

    #[test]
    fn overlay_budget() {
        assert_eq!(overlay_dial_budget(0), 2);
        assert_eq!(overlay_dial_budget(3), 1);
        assert_eq!(overlay_dial_budget(4), 0);
        assert_eq!(overlay_dial_budget(10), 0);
    }

    #[test]
    fn org_dial_plan_caps_at_max() {
        let candidates = vec![info("a"), info("b"), info("c"), info("d"), info("e")];
        let connected = HashSet::from(["a".to_string()]);
        let (to_dial, conn) = plan_organization_dials(&candidates, &connected, 3);
        assert_eq!(conn.len(), 1);
        assert_eq!(to_dial.len(), 3);
        let ids: Vec<String> = to_dial.iter().filter_map(|c| c.peer_id.clone()).collect();
        assert_eq!(ids, vec!["b", "c", "d"]);
    }

    #[test]
    fn exchange_target_rotates() {
        let connected: HashSet<String> = ["a", "b", "c", "self"].iter().map(ToString::to_string).collect();
        assert_eq!(pick_exchange_target(&connected, "self", 0).as_deref(), Some("a"));
        assert_eq!(pick_exchange_target(&connected, "self", 1).as_deref(), Some("b"));
        assert_eq!(pick_exchange_target(&connected, "self", 3).as_deref(), Some("a"));
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(pick_exchange_target(&empty, "self", 0), None);
    }

    #[test]
    fn recovery_trigger_cadence() {
        let mut trigger = RecoveryTrigger::new();
        // 连续 2 个 tick 不触发
        assert!(!trigger.on_tick(true, 1000));
        assert!(!trigger.on_tick(true, 2000));
        // 第 3 个 tick 触发并记录查询时间
        assert!(trigger.on_tick(true, 3000));
        // 冷却期内不触发（即使连续 tick）
        assert!(!trigger.on_tick(true, 4000));
        // 可达时清零
        assert!(!trigger.on_tick(false, 5000));
        assert!(!trigger.on_tick(true, 6000));
        // 冷却过后 + 连续 3 tick 再次触发
        assert!(!trigger.on_tick(true, 7000));
        assert!(!trigger.on_tick(true, 8000));
        assert!(trigger.on_tick(true, 3000 + RECOVERY_COOLDOWN_MS + 1));
    }

    #[test]
    fn recovery_dial_plan_dedupes() {
        let candidates = vec![
            info("a"),
            info("a"),
            PeerNodeInfo { peer_id: None, addresses: vec!["/x".to_string()] },
            PeerNodeInfo { peer_id: None, addresses: vec!["/x".to_string()] },
            info("b"),
        ];
        let plan = plan_recovery_dials(&candidates, 4);
        assert_eq!(plan.len(), 3);
    }
}
