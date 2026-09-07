//! 健康信号确定性推导（community-affairs §10 决策 4）：活跃参与者趋势、
//! 采纳集中度、异议/被申诉次数、最近活动时间、分叉谱系——全部从 affair
//! 日志（已接受操作集 + 存证链锚定时刻）推导，同一日志任何节点复算结果
//! 一致。纯函数：不碰存储（日志加载见 log.rs），输入按 opHash 序即与
//! 到达顺序无关。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::affair::{
    LadderInput, LadderOpView, LadderParams, Mechanism, MetaReviseEntry, OpType, derive_ladder,
    derive_meta_generations, parse_meta_revise_payload,
};
use crate::storage::StorageBackend;

use super::IndexError;
use super::announce::MetaAnnounce;
use super::log::{LoadedLog, objection_counts};

/// 趋势窗口数（近 6 个活跃窗口，最旧在前）。
pub const TREND_BUCKET_COUNT: usize = 6;

/// 健康推导用的操作视图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthOpView {
    pub op_hash: String,
    pub actor_identity: String,
    pub op_type: OpType,
    pub prev_op_hash: String,
    pub anchored_ms: Option<i64>,
    pub objection_target: Option<String>,
}

/// 采纳集中度（ladder 阶梯的采纳计数口径，affair §13）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptionConcentration {
    pub total_adoptions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_identity: Option<String>,
    /// 头部身份采纳份额（千分比；total=0 时为 0）。
    pub top_share_permille: u64,
}

/// 分叉谱系：日志是 DAG（op.prevOpHash 单父引用 → 树形分支），heads 为
/// 未被任何后续操作引用的叶子，maxDepth 为创世到最深叶子的操作数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkLineage {
    pub head_count: u64,
    pub max_depth_ops: u64,
    /// 头 opHash 字典序（确定性）。
    pub heads: Vec<String>,
}

/// 健康信号集（查询结果内嵌，affair-metadata §8）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSignals {
    /// 近 TREND_BUCKET_COUNT 个活跃窗口各窗口活跃身份数（链上时间，最旧在前）。
    pub active_participants_trend: Vec<u64>,
    pub adoption: AdoptionConcentration,
    /// affair 级异议操作数。
    pub objection_count: u64,
    /// 被异议指向的 distinct 操作数（被申诉口径）。
    pub appealed_count: u64,
    /// 最近链上活动时间（锚定时刻最大值；无锚定操作 = None）。
    pub last_activity_ms: Option<i64>,
    pub fork: ForkLineage,
}

/// 健康推导输入：操作视图（任意顺序——函数内部不依赖输入序）+ 阶梯采纳
/// 计数（derive_ladder 产物，调用方注入）+ 求值时刻与活跃窗口宽。
pub struct HealthInput<'a> {
    pub ops: &'a [HealthOpView],
    /// (identity, accepts) 列表（derive_ladder 的 roster.entries）。
    pub ladder_accepts: &'a [(String, u64)],
    pub now_ms: i64,
    pub active_window_ms: i64,
}

/// 确定性健康推导：纯函数，输入相同输出逐字节一致。
pub fn derive_health(input: &HealthInput) -> HealthSignals {
    let ops = input.ops;

    // 活跃趋势：窗口对齐 anchor = now - (now % w)；操作落在
    // [anchor-(i+1)w, anchor-iw) 计第 i 桶（i=0 为最近完整窗口）。
    let w = input.active_window_ms.max(1);
    let anchor = input.now_ms - input.now_ms.rem_euclid(w);
    let mut buckets: Vec<std::collections::HashSet<&str>> =
        vec![std::collections::HashSet::new(); TREND_BUCKET_COUNT];
    for op in ops {
        let Some(t) = op.anchored_ms else { continue };
        let d = anchor - t;
        if d >= 0 && d < (TREND_BUCKET_COUNT as i64) * w {
            buckets[(d / w) as usize].insert(op.actor_identity.as_str());
        }
    }
    let trend = buckets.into_iter().map(|set| set.len() as u64).collect();

    // 采纳集中度：头部身份取采纳数最大者（并列取 identity 字典序小者）。
    let total_adoptions: u64 = input.ladder_accepts.iter().map(|(_, a)| a).sum();
    let top = input
        .ladder_accepts
        .iter()
        .filter(|(_, accepts)| *accepts > 0)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));
    let (top_identity, top_share_permille) = match top {
        Some((identity, accepts)) => (
            Some(identity.clone()),
            if total_adoptions > 0 {
                accepts * 1000 / total_adoptions
            } else {
                0
            },
        ),
        None => (None, 0),
    };

    // 异议 / 被申诉：affair 级 objection 操作数与 distinct 目标数。
    let objection_count = ops
        .iter()
        .filter(|op| op.op_type == OpType::Objection)
        .count() as u64;
    let mut targets: Vec<&str> = ops
        .iter()
        .filter_map(|op| op.objection_target.as_deref())
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let appealed_count = targets.len() as u64;

    let last_activity_ms = ops.iter().filter_map(|op| op.anchored_ms).max();

    // 分叉谱系：单父引用 → 树。referenced = 被某条后续操作引用为 prev 的
    // opHash；heads = 未被引用者（字典序）；深度沿父链回溯（memo），
    // 与输入顺序无关。
    let hash_set: std::collections::HashSet<&str> =
        ops.iter().map(|op| op.op_hash.as_str()).collect();
    let referenced: std::collections::HashSet<&str> = ops
        .iter()
        .map(|op| op.prev_op_hash.as_str())
        .filter(|prev| hash_set.contains(prev))
        .collect();
    let mut heads: Vec<String> = ops
        .iter()
        .filter(|op| !referenced.contains(op.op_hash.as_str()))
        .map(|op| op.op_hash.clone())
        .collect();
    heads.sort();
    let by_hash: std::collections::HashMap<&str, &HealthOpView> =
        ops.iter().map(|op| (op.op_hash.as_str(), op)).collect();
    let mut depth_memo: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut max_depth = 0u64;
    for op in ops {
        let mut depth = 1u64;
        let mut cursor = op.prev_op_hash.as_str();
        let mut guard = 0usize;
        loop {
            if let Some(d) = depth_memo.get(cursor) {
                depth += *d;
                break;
            }
            match by_hash.get(cursor) {
                Some(parent) => {
                    depth += 1;
                    cursor = parent.prev_op_hash.as_str();
                    guard += 1;
                    if guard > ops.len() {
                        break; // 环（不应发生：prev 因果见证已在入站校验）
                    }
                }
                // 父引用不在操作集 = 创世（首操作 prevOpHash = affairId，创世
                // 不入 ops）：创世计入一层（maxDepth 口径为「创世到最深叶子」）。
                None => {
                    depth += 1;
                    break;
                }
            }
        }
        depth_memo.insert(op.op_hash.as_str(), depth);
        max_depth = max_depth.max(depth);
    }

    HealthSignals {
        active_participants_trend: trend,
        adoption: AdoptionConcentration {
            total_adoptions,
            top_identity,
            top_share_permille,
        },
        objection_count,
        appealed_count,
        last_activity_ms,
        fork: ForkLineage {
            head_count: heads.len() as u64,
            max_depth_ops: max_depth,
            heads,
        },
    }
}

/// 公告修订链复算（affair-metadata §4 验证分层）：本地有日志副本 →
/// Verified/Conflict；无副本 → Unverified。复用 affair 纯函数。
pub fn verify_against_log(
    log: Option<&LoadedLog>,
    announce: &MetaAnnounce,
    now_ms: i64,
) -> crate::affair::MetaVerify {
    let Some(log) = log else {
        return crate::affair::MetaVerify::Unverified;
    };
    let moderator = log.genesis.initiator.identity.clone();
    let objections = objection_counts(&log.ops);
    let revisions: Vec<MetaReviseEntry> = log
        .ops
        .iter()
        .filter(|op| op.parsed.op_type == OpType::MetaRevise)
        .filter_map(|op| {
            let payload = parse_meta_revise_payload(&op.parsed.payload).ok()?;
            // 与 derive_meta_generations 同口径：只 delayed-veto 形态递进代际
            if !matches!(payload.mechanism, Mechanism::DelayedVeto { .. }) {
                return None;
            }
            Some(MetaReviseEntry {
                op_hash: op.op_hash.clone(),
                actor_identity: op.parsed.actor.identity.clone(),
                payload,
                anchored_ms: op.anchored_ms.unwrap_or(i64::MIN),
                objection_count: objections.get(&op.op_hash).copied().unwrap_or(0),
            })
        })
        .collect();
    let genesis_meta = crate::affair::AffairMeta {
        title: log.genesis.title.clone(),
        summary: log.genesis.summary.clone(),
        tags: log.genesis.tags.clone(),
    };
    let generations = derive_meta_generations(
        &announce.affair_id,
        &genesis_meta,
        &moderator,
        &revisions,
        now_ms,
    );
    crate::affair::verify_meta_announce(
        announce.meta_seq,
        &announce.basis_op_hash,
        &crate::affair::AffairMeta {
            title: announce.title.clone(),
            summary: announce.summary.clone(),
            tags: announce.tags.clone(),
        },
        Some(&generations),
    )
}

/// 从本地日志推导健康信号；无本地日志副本返回 None（纯 indexer 条目
/// 的查询结果 health 缺席，由呈现层自行处理）。
pub fn health_from_log(log: &LoadedLog, now_ms: i64) -> Result<HealthSignals, IndexError> {
    let objections = objection_counts(&log.ops);
    let views: Vec<LadderOpView> = log
        .ops
        .iter()
        .map(|op| LadderOpView {
            op_hash: op.op_hash.clone(),
            actor_kind: op.parsed.actor.kind,
            actor_identity: op.parsed.actor.identity.clone(),
            op_type: op.parsed.op_type,
            payload: op.parsed.payload.clone(),
            anchored_ms: op.anchored_ms,
            objection_count: objections.get(&op.op_hash).copied().unwrap_or(0),
        })
        .collect();
    let params = LadderParams::from_participation(
        log.genesis_value
            .get("rules")
            .and_then(|rules| rules.get("participation"))
            .unwrap_or(&Value::Null),
    )
    .map_err(|_| IndexError::CorruptLog(log.affair_id.clone()))?;
    let genesis_anchored_ms = log
        .anchors
        .get(&("genesis".to_string(), log.affair_id.clone()))
        .copied();
    let roster = derive_ladder(&LadderInput {
        initial_voters: &log.genesis.initial_voters,
        genesis_anchored_ms,
        ops: &views,
        params,
        now_ms,
    });
    let ladder_accepts: Vec<(String, u64)> = roster
        .entries
        .iter()
        .map(|entry| (entry.identity.clone(), entry.accepts))
        .collect();
    let health_views: Vec<HealthOpView> = log
        .ops
        .iter()
        .map(|op| HealthOpView {
            op_hash: op.op_hash.clone(),
            actor_identity: op.parsed.actor.identity.clone(),
            op_type: op.parsed.op_type,
            prev_op_hash: op.parsed.prev_op_hash.clone(),
            anchored_ms: op.anchored_ms,
            objection_target: op
                .parsed
                .payload
                .get("target")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        })
        .collect();
    Ok(derive_health(&HealthInput {
        ops: &health_views,
        ladder_accepts: &ladder_accepts,
        now_ms,
        active_window_ms: params.active_window_ms,
    }))
}

/// 存储无关的健康加载：有本地日志副本 → Some(信号)；否则 None。
pub fn try_health<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
    now_ms: i64,
) -> Result<Option<HealthSignals>, IndexError> {
    let Some(log) = super::log::load_log(storage, affair_id)? else {
        return Ok(None);
    };
    Ok(Some(health_from_log(&log, now_ms)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        hash: &str,
        actor: &str,
        op_type: OpType,
        prev: &str,
        anchored: Option<i64>,
        target: Option<&str>,
    ) -> HealthOpView {
        HealthOpView {
            op_hash: hash.to_string(),
            actor_identity: actor.to_string(),
            op_type,
            prev_op_hash: prev.to_string(),
            anchored_ms: anchored,
            objection_target: target.map(ToString::to_string),
        }
    }

    const W: i64 = 90 * 24 * 60 * 60 * 1000;
    const NOW: i64 = 1_720_000_000_000;

    fn sample_ops() -> Vec<HealthOpView> {
        // 树：genesis(root) ← a1 ← b1（分支1）；a1 ← b2（分支2，分叉）。
        // 窗口 W=90 天：anchor = NOW - NOW%W；a1 锚在 2W 前（桶 1），
        // b1/b2 锚在近半个窗口内（桶 0）。
        vec![
            view(
                "a1",
                "alice",
                OpType::Content,
                "root",
                Some(NOW - 2 * W),
                None,
            ),
            view("b1", "bob", OpType::Content, "a1", Some(NOW - W / 2), None),
            view(
                "b2",
                "alice",
                OpType::Objection,
                "a1",
                Some(NOW - W / 4),
                Some("a1"),
            ),
        ]
    }

    fn health_input<'a>(ops: &'a [HealthOpView], accepts: &'a [(String, u64)]) -> HealthInput<'a> {
        HealthInput {
            ops,
            ladder_accepts: accepts,
            now_ms: NOW,
            active_window_ms: W,
        }
    }

    #[test]
    fn same_log_any_order_same_signals() {
        let accepts = vec![("alice".to_string(), 2u64), ("bob".to_string(), 1u64)];
        let a = derive_health(&health_input(&sample_ops(), &accepts));
        let mut shuffled = sample_ops();
        shuffled.reverse();
        let b = derive_health(&health_input(&shuffled, &accepts));
        assert_eq!(a, b);
    }

    #[test]
    fn signals_match_fixed_expectations() {
        let accepts = vec![("alice".to_string(), 2u64), ("bob".to_string(), 1u64)];
        let signals = derive_health(&HealthInput {
            ops: &sample_ops(),
            ladder_accepts: &accepts,
            now_ms: NOW,
            active_window_ms: W,
        });
        // 趋势：anchor = NOW - NOW%W；b1/b2 落在桶 0（alice、bob 各活跃），
        // a1 落在桶 1（alice）
        assert_eq!(signals.active_participants_trend.len(), TREND_BUCKET_COUNT);
        assert_eq!(signals.active_participants_trend[0], 2);
        assert_eq!(signals.active_participants_trend[1], 1);
        assert!(
            signals.active_participants_trend[2..]
                .iter()
                .all(|n| *n == 0)
        );
        // 采纳集中度：total=3，top=alice 2/3
        assert_eq!(signals.adoption.total_adoptions, 3);
        assert_eq!(signals.adoption.top_identity.as_deref(), Some("alice"));
        assert_eq!(signals.adoption.top_share_permille, 666);
        // 异议 1 条，被申诉 distinct 目标 1 个
        assert_eq!(signals.objection_count, 1);
        assert_eq!(signals.appealed_count, 1);
        // 最近活动 = b2 的锚定时刻
        assert_eq!(signals.last_activity_ms, Some(NOW - W / 4));
        // 分叉谱系：b1/b2 两个头，最深 root→a1→b* = 3 层
        assert_eq!(signals.fork.head_count, 2);
        assert_eq!(signals.fork.max_depth_ops, 3);
        assert_eq!(signals.fork.heads, vec!["b1".to_string(), "b2".to_string()]);
    }

    #[test]
    fn empty_log_yields_zeroed_signals() {
        let accepts = Vec::new();
        let signals = derive_health(&HealthInput {
            ops: &[],
            ladder_accepts: &accepts,
            now_ms: NOW,
            active_window_ms: W,
        });
        assert_eq!(signals.adoption.total_adoptions, 0);
        assert_eq!(signals.adoption.top_identity, None);
        assert_eq!(signals.last_activity_ms, None);
        assert_eq!(signals.fork.head_count, 0);
        assert_eq!(signals.fork.heads, Vec::<String>::new());
    }
}
