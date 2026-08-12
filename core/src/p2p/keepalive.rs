//! keepalive 拨号候选与恢复触发的纯逻辑（core/spec/p2p-messages.md §8.4/§12）。
//!
//! tick 内的网络动作由 `P2pNode` 事件循环执行；这里只放可单测的判定逻辑。

use std::collections::HashSet;

use super::constants::RECOVERY_SEARCH_DISPLAY_MS;
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

/// org-recovery 触发状态（connection-policy M6 简化）：不再有「连续 3 tick +
/// 冷却」的周期触发（tick 内主动外联已全删）。恢复查询改为**懒连接链的 DHT
/// 刷新环节**——仅在事件点（登录/网络恢复/org 写入推送/orgsync-hello 懒拨号）
/// 触发，每次按需查询、失败即沉默。本结构只记录最近一轮查询时间供 UI 展示。
pub struct RecoveryTrigger {
    last_query_at: Option<i64>,
}

impl Default for RecoveryTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryTrigger {
    pub fn new() -> Self {
        Self { last_query_at: None }
    }

    /// 记录一轮 DHT 刷新查询的发起时间（懒连接链 DHT 刷新环节实际发起查询时
    /// 调用；供 UI 展示恢复状态）。
    pub fn note_query(&mut self, now_ms: i64) {
        self.last_query_at = Some(now_ms);
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
