//! 决议产物（wiki/protocol/community/affair.md §6）。
//!
//! resolution 是显式钉入日志的操作：其内容全体副本可对**同一操作集合**
//! 确定性复算（§6.1）；复算不符 → 该 resolution 无效。生效语义两态
//! （§6.2）：落日志即「待确认决议」（公示期结束前任何副本不得呈现为已生效），
//! 公示期内无阈值异议 → 「生效决议」。

use serde_json::Value;

use super::close::{CloseCtx, CountedOp, evaluate_close_condition};
use super::op::sort_op_hashes;
use super::rules::{DEFAULT_PUB_PERIOD_MS, MIN_PUB_PERIOD_MS, parse_close_condition, rules_hash};

/// resolution payload（§6.1 线形）。
#[derive(Clone, Debug, PartialEq)]
pub struct ResolutionPayload {
    /// 结果标签（如 "passed"）。
    pub result: String,
    /// 被满足的 §5.2 关闭条件原文。
    pub condition: Value,
    /// 计入关闭判定的有效操作 opHash 列表（§8 排序键升序）。
    pub counted_ops: Vec<String>,
    /// 计票结果（插件结构，内核只承诺字节）。
    pub tally: Value,
    /// 法定人数快照操作 opHash（可空）。
    pub quorum_snapshot: Option<String>,
    /// 判定所用规则文档版本哈希。
    pub rules_hash: String,
    /// 公示期毫秒。
    pub pub_period_ms: i64,
}

/// 解析 resolution payload（结构校验；复算见 [`replay_resolution`]）。
pub fn parse_resolution_payload(payload: &Value) -> Result<ResolutionPayload, &'static str> {
    let obj = payload.as_object().ok_or("bad-resolution-payload")?;
    let result = obj
        .get("result")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 32)
        .ok_or("bad-resolution-payload")?
        .to_string();
    let condition = obj
        .get("condition")
        .filter(|v| v.is_object())
        .cloned()
        .ok_or("bad-resolution-payload")?;
    let counted_ops = obj
        .get("countedOps")
        .and_then(Value::as_array)
        .ok_or("bad-resolution-payload")?
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| super::actor::is_valid_identity_id(s))
                .map(str::to_string)
                .ok_or("bad-resolution-payload")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tally = obj.get("tally").cloned().unwrap_or(Value::Null);
    let quorum_snapshot = match obj.get("quorumSnapshot") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|s| super::actor::is_valid_identity_id(s))
                .ok_or("bad-resolution-payload")?
                .to_string(),
        ),
    };
    let rules_hash = obj
        .get("rulesHash")
        .and_then(Value::as_str)
        .filter(|s| super::actor::is_valid_identity_id(s))
        .ok_or("bad-resolution-payload")?
        .to_string();
    let pub_period_ms = obj
        .get("pubPeriod")
        .and_then(|v| v.get("delayMs"))
        .and_then(Value::as_i64)
        .ok_or("bad-resolution-payload")?;
    // 公示期是规则参数（§6.2，下限 24h）：决议方自声明的零/短公示期
    // 在入站解析期即拒，不得籍自声明值把生效态即时化（评审阻塞 1）
    if pub_period_ms < MIN_PUB_PERIOD_MS {
        return Err("bad-pub-period");
    }
    Ok(ResolutionPayload {
        result,
        condition,
        counted_ops,
        tally,
        quorum_snapshot,
        rules_hash,
        pub_period_ms,
    })
}

/// 决议状态（§6.2 两态 + 打回）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionState {
    /// 待确认决议（公示期内；任何副本不得呈现为已生效）。
    Pending,
    /// 生效决议（公示期过且无阈值异议）。
    Effective,
    /// 公示期内达到异议阈值 → 打回复核。
    Vetoed,
}

/// 决议生效求值（§6.2）：异议达阈值（默认任一有效异议）即打回；否则公示期
/// （≥24h）满后生效。`anchored_ms` = resolution 操作的锚定时刻，`now_ms` 注入。
pub fn resolution_state(
    anchored_ms: i64,
    pub_period_ms: i64,
    objection_count: u64,
    veto_count: u64,
    now_ms: i64,
) -> ResolutionState {
    if objection_count >= veto_count {
        return ResolutionState::Vetoed;
    }
    if now_ms >= anchored_ms + pub_period_ms {
        ResolutionState::Effective
    } else {
        ResolutionState::Pending
    }
}

/// 复算结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// 复算一致。
    Ok,
    /// rulesHash 与所给规则文档版本不符。
    RulesHashMismatch,
    /// condition 非 §5.2 可判定形式。
    BadCondition,
    /// 同一操作集合上该条件未满足。
    ConditionNotSatisfied,
    /// countedOps 复算不符（含未按 §8 升序）。
    CountedOpsMismatch,
    /// 自声明 pubPeriod 与判定所用 rules 版本的公示期不符（评审阻塞 1）。
    PubPeriodMismatch,
}

impl ReplayOutcome {
    /// 稳定 reason 字符串。
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::RulesHashMismatch => Some("rules-hash-mismatch"),
            Self::BadCondition => Some("bad-condition"),
            Self::ConditionNotSatisfied => Some("condition-not-satisfied"),
            Self::CountedOpsMismatch => Some("counted-ops-mismatch"),
            Self::PubPeriodMismatch => Some("pub-period-mismatch"),
        }
    }
}

/// resolution 复算（§6.1）：对同一操作集合重算关闭判定，逐项比对
/// rulesHash / condition / countedOps（须 §8 升序）/ pubPeriod（与判定所用
/// rules 版本的公示期一致，§6.2）。`rules` 为判定所用规则文档版本原文；
/// 求值上下文（基数/锚定时间）由调用方注入。
pub fn replay_resolution(
    rules: &Value,
    payload: &ResolutionPayload,
    valid_ops: &[CountedOp],
    ctx: &CloseCtx,
) -> ReplayOutcome {
    if rules_hash(rules) != payload.rules_hash {
        return ReplayOutcome::RulesHashMismatch;
    }
    let rules_pub_period_ms = rules
        .get("pubPeriod")
        .and_then(|p| p.get("delayMs"))
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_PUB_PERIOD_MS);
    if rules_pub_period_ms != payload.pub_period_ms {
        return ReplayOutcome::PubPeriodMismatch;
    }
    let Ok(condition) = parse_close_condition(&payload.condition) else {
        return ReplayOutcome::BadCondition;
    };
    if sort_op_hashes(&payload.counted_ops) != payload.counted_ops {
        return ReplayOutcome::CountedOpsMismatch;
    }
    match evaluate_close_condition(&condition, valid_ops, ctx) {
        None => ReplayOutcome::ConditionNotSatisfied,
        Some(counted) if counted != payload.counted_ops => ReplayOutcome::CountedOpsMismatch,
        Some(_) => ReplayOutcome::Ok,
    }
}
