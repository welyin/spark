//! keepalive 拨号候选与恢复触发的纯逻辑（core/spec/p2p-messages.md §8.4/§12）。
//!
//! tick 内的网络动作由 `P2pNode` 事件循环执行；这里只放可单测的判定逻辑。

use std::collections::HashSet;

use super::constants::{
    OVERLAY_DIAL_TARGET, OVERLAY_TICK_DIAL_BUDGET, RECOVERY_COOLDOWN_MS,
    RECOVERY_SEARCH_DISPLAY_MS, RECOVERY_TRIGGER_CONSECUTIVE_TICKS,
};
use super::peer_targets::PeerNodeInfo;

/// 恢复模式对外状态（网络状态 UI 的数据源，org.md §12 扩展）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecoveryState {
    /// 未在恢复（组织可达，或从未触发恢复查询）。
    #[default]
    Idle,
    /// 恢复查找中（DHT/恢复查询已发起，限时显示）。
    Recovering {
        /// 本轮恢复查询发起时间。
        since: i64,
    },
    /// 自动恢复无果（超过显示窗口未再查询）。
    Failed {
        /// 最近一轮恢复查询发起时间。
        since: i64,
    },
}

impl RecoveryState {
    /// 线形字符串（DTO/前端展示分支用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            RecoveryState::Idle => "idle",
            RecoveryState::Recovering { .. } => "recovering",
            RecoveryState::Failed { .. } => "failed",
        }
    }

    /// 恢复查询发起时间（Idle 为 `None`）。
    pub fn since(&self) -> Option<i64> {
        match self {
            RecoveryState::Idle => None,
            RecoveryState::Recovering { since } | RecoveryState::Failed { since } => Some(*since),
        }
    }
}

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
pub fn pick_exchange_target(
    connected: &HashSet<String>,
    self_peer_id: &str,
    cursor: u64,
) -> Option<String> {
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

    /// 只读状态快照（网络状态 UI 用）：最近一轮恢复查询距今一个显示窗口
    /// （[`RECOVERY_SEARCH_DISPLAY_MS`]`）内视为「恢复中」；超过窗口视为
    /// 「自动恢复无果」；从未发起查询为 `Idle`。
    pub fn state(&self, now_ms: i64) -> RecoveryState {
        match self.last_query_at {
            Some(since) if now_ms - since <= RECOVERY_SEARCH_DISPLAY_MS => {
                RecoveryState::Recovering { since }
            }
            Some(since) => RecoveryState::Failed { since },
            None => RecoveryState::Idle,
        }
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
