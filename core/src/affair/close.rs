//! 关闭条件求值（wiki/protocol/community/affair.md §5.2/§7.2/§8）。
//!
//! 可判定性纪律（§3.3）：求值只依赖操作集合内容与注入的时间参数，不依赖
//! 到达顺序与本地时钟即时值。wall-clock 的时间源 = 关注者各副本存证链上的
//! 锚定时间（§7.2），由调用方注入 `anchored_ms`；±10 min 容忍带内（
//! [`WALL_CLOCK_TOLERANCE_MS`]，对齐 declaredAt 新鲜度先例）判为 Ambiguous
//! ——不关闭、等更晚的锚定，由公示期吸收残余分歧（§7.2 诚实标注：秒级精确
//! 「到点自动关闭且全网同时一致」做不到）。

use super::op::OpType;
use super::rules::{CloseCondition, ThresholdBase};

/// wall-clock 容忍带：±10 min（nodeInfoClaim 新鲜度先例，§7.2）。
pub const WALL_CLOCK_TOLERANCE_MS: i64 = 10 * 60 * 1000;

/// wall-clock 条件求值三态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallClockEval {
    /// 锚定时间明显未到 notBefore。
    NotReached,
    /// 容忍带内（|anchored − notBefore| < 容忍）：时钟不确定区，判未满足。
    Ambiguous,
    /// 锚定时间越过 notBefore + 容忍带：各副本可一致判定为满足。
    Reached,
}

/// wall-clock 求值：`anchored_ms >= notBefore + 容忍` 才算满足。容忍带吸收
/// 副本间 ≤±10 min 的锚定偏差——带内一律视为未满足，公示期兜底（§7.2）。
pub fn evaluate_wall_clock(not_before: i64, anchored_ms: i64) -> WallClockEval {
    if anchored_ms >= not_before + WALL_CLOCK_TOLERANCE_MS {
        WallClockEval::Reached
    } else if anchored_ms >= not_before - WALL_CLOCK_TOLERANCE_MS {
        WallClockEval::Ambiguous
    } else {
        WallClockEval::NotReached
    }
}

/// 有效操作视图（求值输入：已过滤验签失败/资格不符/静态检查不过者，§8）。
#[derive(Clone, Debug, PartialEq)]
pub struct CountedOp {
    /// 操作 opHash。
    pub op_hash: String,
    /// 操作类型。
    pub op_type: OpType,
    /// 操作者身份（threshold 按人去重计数用）。
    pub actor_identity: String,
    /// 载荷原文（op-count 的 filter 匹配 `payload.kind`）。
    pub payload: serde_json::Value,
}

/// 关闭求值上下文（基数/时间均由调用方注入）。
pub struct CloseCtx {
    /// threshold 基数 = `snapshot:<opHash>` 形态时该快照名册的大小。
    pub snapshot_roster_size: Option<u64>,
    /// threshold 基数 = `ladder:voters` 时的名册大小（推导属 C6，此处注入）。
    pub ladder_voters_size: Option<u64>,
    /// wall-clock 形态：锚定时间（本副本存证链，§7.2）。
    pub anchored_ms: Option<i64>,
}

impl CloseCtx {
    /// 构造空上下文（按条件形态填字段）。
    pub fn empty() -> Self {
        Self {
            snapshot_roster_size: None,
            ladder_voters_size: None,
            anchored_ms: None,
        }
    }
}

/// op-count / threshold 的操作匹配：opType 一致且 filter（若有）匹配
/// `payload.kind`（filter 为插件命名串，§5.2）。
fn matches_filter(cond_op_type: OpType, cond_filter: Option<&str>, op: &CountedOp) -> bool {
    if op.op_type != cond_op_type {
        return false;
    }
    match cond_filter {
        None => true,
        Some(filter) => op.payload.get("kind").and_then(serde_json::Value::as_str) == Some(filter),
    }
}

/// 求值单条关闭条件。满足时返回计入判定的有效操作 opHash 列表
/// （按 §8 排序键升序；wall-clock 无计入操作，返回空列表）。
pub fn evaluate_close_condition(
    cond: &CloseCondition,
    valid_ops: &[CountedOp],
    ctx: &CloseCtx,
) -> Option<Vec<String>> {
    match cond {
        CloseCondition::OpCount {
            op_type,
            filter,
            count,
        } => {
            let mut matched: Vec<&CountedOp> = valid_ops
                .iter()
                .filter(|op| matches_filter(*op_type, filter.as_deref(), op))
                .collect();
            matched.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
            if (matched.len() as u64) < *count {
                return None;
            }
            Some(
                matched[..*count as usize]
                    .iter()
                    .map(|op| op.op_hash.clone())
                    .collect(),
            )
        }
        CloseCondition::Threshold { base, ratio } => {
            let base_size = match base {
                ThresholdBase::Snapshot(_) => ctx.snapshot_roster_size?,
                ThresholdBase::LadderVoters => ctx.ladder_voters_size?,
            };
            // threshold 计入语义：有效 content 操作按操作者身份去重（一人一次，
            // 防单人刷屏满足比例；基数为快照名册，本来就是人头口径），每人计其
            // opHash 字典序最小的操作（确定性，与到达顺序无关）。
            let mut matched: Vec<&CountedOp> = valid_ops
                .iter()
                .filter(|op| matches_filter(OpType::Content, None, op))
                .collect();
            matched.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
            let mut seen: Vec<&str> = Vec::new();
            let mut counted: Vec<String> = Vec::new();
            for op in matched {
                if seen.contains(&op.actor_identity.as_str()) {
                    continue;
                }
                seen.push(&op.actor_identity);
                counted.push(op.op_hash.clone());
            }
            if ratio.reached(counted.len() as u64, base_size) {
                Some(counted)
            } else {
                None
            }
        }
        CloseCondition::WallClock { not_before } => {
            let anchored_ms = ctx.anchored_ms?;
            if evaluate_wall_clock(*not_before, anchored_ms) == WallClockEval::Reached {
                Some(Vec::new())
            } else {
                None
            }
        }
    }
}

/// 依序求值关闭条件数组（任一满足即关闭，§5.1）。返回（条件下标，计入操作）。
pub fn evaluate_close_conditions(
    conditions: &[CloseCondition],
    valid_ops: &[CountedOp],
    ctx: &CloseCtx,
) -> Option<(usize, Vec<String>)> {
    conditions
        .iter()
        .enumerate()
        .find_map(|(i, cond)| evaluate_close_condition(cond, valid_ops, ctx).map(|ops| (i, ops)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(hash: &str, identity: &str) -> CountedOp {
        CountedOp {
            op_hash: hash.repeat(64 / hash.len()),
            op_type: OpType::Content,
            actor_identity: identity.repeat(64 / identity.len()),
            payload: json!({"kind": "post"}),
        }
    }

    #[test]
    fn wall_clock_tolerance_band() {
        let not_before = 1_730_000_000_000i64;
        assert_eq!(
            evaluate_wall_clock(not_before, not_before - WALL_CLOCK_TOLERANCE_MS - 1),
            WallClockEval::NotReached
        );
        assert_eq!(
            evaluate_wall_clock(not_before, not_before),
            WallClockEval::Ambiguous
        );
        assert_eq!(
            evaluate_wall_clock(not_before, not_before + WALL_CLOCK_TOLERANCE_MS),
            WallClockEval::Reached
        );
    }

    #[test]
    fn op_count_takes_nth_by_sort_key() {
        let ops = vec![op("cc", "aa"), op("aa", "bb"), op("bb", "cc")];
        let cond = CloseCondition::OpCount {
            op_type: OpType::Content,
            filter: None,
            count: 2,
        };
        let counted = evaluate_close_condition(&cond, &ops, &CloseCtx::empty()).unwrap();
        assert_eq!(counted, vec!["aa".repeat(32), "bb".repeat(32)]);
        // 第 4 个不存在 → 不关闭
        let cond4 = CloseCondition::OpCount {
            op_type: OpType::Content,
            filter: None,
            count: 4,
        };
        assert_eq!(
            evaluate_close_condition(&cond4, &ops, &CloseCtx::empty()),
            None
        );
    }

    #[test]
    fn threshold_counts_distinct_actors() {
        // 名册 3 人、阈值 2/3：同一人的两条操作只计一次，需两个不同身份
        let ops = vec![op("aa", "aa"), op("bb", "aa"), op("cc", "bb")];
        let cond = CloseCondition::Threshold {
            base: ThresholdBase::LadderVoters,
            ratio: super::super::rules::Fraction { num: 2, den: 3 },
        };
        let ctx = CloseCtx {
            ladder_voters_size: Some(3),
            ..CloseCtx::empty()
        };
        assert_eq!(
            evaluate_close_condition(&cond, &ops, &ctx),
            Some(vec!["aa".repeat(32), "cc".repeat(32)])
        );
        let ctx4 = CloseCtx {
            ladder_voters_size: Some(4),
            ..CloseCtx::empty()
        };
        assert_eq!(evaluate_close_condition(&cond, &ops, &ctx4), None);
    }
}
