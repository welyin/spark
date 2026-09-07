//! 执行型事务（wiki/protocol/community/affair.md §6.2-3）：决议生效后的
//! 「待执行 → 执行回报 → 核查 → 关闭」状态机纯逻辑。
//!
//! 规格未逐字钉死处的实现判定（已回写规格 §6.3 登记）：
//!
//! - 执行回报 = 内核 opType `exec-report`（§4），payload 引用决议 opHash +
//!   状态（accepted / in-progress / done，done 必带 evidence 佐证引用）；
//!   决议引用未知时按 §4 乱序规则暂存待补（op.rs）；
//! - 执行方绑定在**推导层**判定（OpLog 不持有规则文档）：非声明执行方签署的
//!   回报、锚定时刻早于决议生效时刻（= 决议锚定 + 规则版本公示期，纯链上
//!   时间，不消费 declaredAt/now）的回报，一律不进入执行状态集——生效前
//!   发出的回报**永久惰性**，不随时间翻转，执行方须于生效后重发（fail-closed，
//!   乱序/重放同输出）；
//! - 多条有效回报按 (anchored_ms, opHash) 排序取**最新一条**为当前状态依据
//!   （§8 确定性排序键的扩展：先链上时刻后哈希字典序，平局可判）；
//! - 核查三形态求值：delayed-veto 复用 [`super::decide::evaluate_delayed_veto`]
//!   （核查期 = done 回报锚定 + delayMs，delayMs 静态检查下限 24h，同 §5.1
//!   公示期下限）；verifier-sign = 核查方以 `vote` 操作（proposal = done 回报
//!   opHash，choice = "yes"）确认，**全体**核查方确认方通过；vote 复用
//!   [`super::decide::evaluate_vote`] 阈值机制（名册由调用方注入，名册缺席
//!   → 恒核查中，fail-closed，防空名册平凡通过）；
//! - 核查不通过（delayed-veto 异议达阈值）→ 打回执行（returned），执行方可
//!   再回报重启核查；申诉不设平行机制，走 §10 `refs` rel=appeal 新事务；
//! - rules.exec == null（纯讨论/决议型）：决议即终态，本模块不参与（§6.2）。

use base64::Engine as _;
use serde_json::Value;
use sha2::Digest as _;

use super::actor::{Actor, ActorKind, is_valid_identity_id};
use super::decide::{Decision, VoteBallot, evaluate_delayed_veto, evaluate_vote};
use super::op::OpType;
use super::resolution::ResolutionState;
use super::rules::{Fraction, MIN_PUB_PERIOD_MS, Mechanism, StaticCheckReject, parse_mechanism};

/// 证据引用串长度上限（UTF-16 code unit 口径，对齐 objection.reason 先例 §4）。
const MAX_EVIDENCE_LEN_UTF16: usize = 256;

/// 核查方式（§5.1 `exec.verify` 三形态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecVerify {
    /// 延迟公示 + 阈值否决（默认；核查期下限 24h）。
    DelayedVeto {
        /// 核查公示期毫秒（≥ 24h）。
        delay_ms: i64,
        /// 否决阈值（有效异议数，缺省 1）。
        veto_count: u64,
    },
    /// 指定核查方签名确认（全体确认方通过）。
    VerifierSign {
        /// 核查方身份列表（非空、去重）。
        verifiers: Vec<String>,
    },
    /// 正式投票（复用 §5.3 vote 阈值机制；名册求值时注入）。
    Vote {
        /// 投票者集合声明（如 `ladder:voters`）。
        voter_set: String,
        /// 通过阈值。
        threshold: Fraction,
        /// 法定人数。
        quorum: Fraction,
        /// 快照要求声明（如 `required`）。
        snapshot: String,
    },
}

/// 执行方声明（§5.1 `exec.executor` Actor 摘要）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecExecutor {
    /// person / org。
    pub kind: ActorKind,
    /// 身份 id（64 hex 小写）。
    pub identity: String,
    /// 可选公钥钉扎（声明即须与身份满足 §2.2 绑定，且回报 actor 公钥逐字节相等）。
    pub public_key: Option<String>,
}

/// 执行型事务声明（§5.1 `exec`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecDecl {
    /// 执行方。
    pub executor: ExecExecutor,
    /// 核查方式。
    pub verify: ExecVerify,
}

/// 解析 exec 声明（§5.1/§5.6 静态检查口径，fail-closed）。`value` 为 null 的
/// 纯讨论/决议型由调用方先行分流，本函数只接受对象形态。
pub fn parse_exec_decl(value: &Value) -> Result<ExecDecl, StaticCheckReject> {
    let obj = value.as_object().ok_or(StaticCheckReject::InvalidExec)?;
    let executor_obj = obj
        .get("executor")
        .and_then(Value::as_object)
        .ok_or(StaticCheckReject::InvalidExec)?;
    let kind = match executor_obj.get("kind").and_then(Value::as_str) {
        Some("person") => ActorKind::Person,
        Some("org") => ActorKind::Org,
        _ => return Err(StaticCheckReject::InvalidExec),
    };
    let identity = executor_obj
        .get("identity")
        .and_then(Value::as_str)
        .filter(|s| is_valid_identity_id(s))
        .ok_or(StaticCheckReject::InvalidExec)?
        .to_string();
    let public_key = match executor_obj.get("publicKey") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let pk = v.as_str().ok_or(StaticCheckReject::InvalidExec)?;
            // 声明即校验 §2.2 公钥-身份绑定（同 parse_actor 口径）
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(pk.as_bytes())
                .map_err(|_| StaticCheckReject::InvalidExec)?;
            if hex::encode(sha2::Sha256::digest(&bytes)) != identity {
                return Err(StaticCheckReject::InvalidExec);
            }
            Some(pk.to_string())
        }
    };
    let verify_raw = obj
        .get("verify")
        .and_then(Value::as_object)
        .ok_or(StaticCheckReject::InvalidExec)?;
    let verify = match verify_raw.get("kind").and_then(Value::as_str) {
        Some("delayed-veto") => {
            let delay_ms = verify_raw
                .get("delayMs")
                .and_then(Value::as_i64)
                .ok_or(StaticCheckReject::InvalidExec)?;
            // 核查公示期同 §5.1 下限 24h（公示期吸收时钟偏差与副本滞后，§7.2）
            if delay_ms < MIN_PUB_PERIOD_MS {
                return Err(StaticCheckReject::InvalidExec);
            }
            // vetoThreshold 缺省 count=1；present 但形状非法 → 拒绝（同 §5.6 第 4 条口径）
            let veto_count = match verify_raw.get("vetoThreshold") {
                None | Some(Value::Null) => 1,
                Some(v) => {
                    let veto = v.as_object().ok_or(StaticCheckReject::InvalidExec)?;
                    match veto.get("count") {
                        None | Some(Value::Null) => 1,
                        Some(c) => c.as_u64().ok_or(StaticCheckReject::InvalidExec)?,
                    }
                }
            };
            if veto_count < 1 {
                return Err(StaticCheckReject::InvalidExec);
            }
            ExecVerify::DelayedVeto {
                delay_ms,
                veto_count,
            }
        }
        Some("verifier-sign") => {
            let verifiers_value = verify_raw
                .get("verifiers")
                .and_then(Value::as_array)
                .ok_or(StaticCheckReject::InvalidExec)?;
            if verifiers_value.is_empty() {
                return Err(StaticCheckReject::InvalidExec);
            }
            let mut verifiers = Vec::with_capacity(verifiers_value.len());
            for v in verifiers_value {
                let id = v
                    .as_str()
                    .filter(|s| is_valid_identity_id(s))
                    .ok_or(StaticCheckReject::InvalidExec)?;
                if verifiers.contains(&id.to_string()) {
                    return Err(StaticCheckReject::InvalidExec);
                }
                verifiers.push(id.to_string());
            }
            ExecVerify::VerifierSign { verifiers }
        }
        // vote 形态与 §5.3 vote 机制同线形，复用 parse_mechanism 的分数校验
        Some("vote") => match parse_mechanism(&Value::Object(verify_raw.clone())) {
            Ok(Mechanism::Vote {
                voter_set,
                threshold,
                quorum,
                snapshot,
            }) => ExecVerify::Vote {
                voter_set,
                threshold,
                quorum,
                snapshot,
            },
            _ => return Err(StaticCheckReject::InvalidExec),
        },
        _ => return Err(StaticCheckReject::InvalidExec),
    };
    Ok(ExecDecl {
        executor: ExecExecutor {
            kind,
            identity,
            public_key,
        },
        verify,
    })
}

/// 执行方绑定校验：回报签名者身份 == 声明执行方身份；声明钉扎公钥时逐字节
/// 相等；声明 kind=org 时回报 actor 必须为 org 且携带 orgSig（结构存在性，
/// 组织签名包五步验证链属 org 层，同 actor.rs 边界）。
pub fn executor_matches(executor: &ExecExecutor, actor: &Actor) -> bool {
    if actor.identity != executor.identity {
        return false;
    }
    if let Some(pk) = &executor.public_key
        && actor.public_key != *pk
    {
        return false;
    }
    match executor.kind {
        ActorKind::Person => actor.kind == ActorKind::Person,
        ActorKind::Org => actor.kind == ActorKind::Org && actor.org_sig.is_some(),
    }
}

/// 执行回报状态（§6.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecReportStatus {
    /// 执行方已认领。
    Accepted,
    /// 执行中。
    InProgress,
    /// 完成（必带佐证材料引用）。
    Done,
}

impl ExecReportStatus {
    /// 线形字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::InProgress => "in-progress",
            Self::Done => "done",
        }
    }
}

/// exec-report payload（§6.3 线形）。
#[derive(Clone, Debug, PartialEq)]
pub struct ExecReportPayload {
    /// 引用的决议 opHash（§6.1：决议 id = resolution 操作 opHash）。
    pub resolution: String,
    /// 回报状态。
    pub status: ExecReportStatus,
    /// 佐证材料引用（done 必填非空）。
    pub evidence: Vec<String>,
}

/// 解析 exec-report payload（结构校验；执行方绑定与生效时序门控属推导层
/// [`derive_exec_states`]）。
pub fn parse_exec_report_payload(payload: &Value) -> Result<ExecReportPayload, &'static str> {
    let obj = payload.as_object().ok_or("bad-exec-report-payload")?;
    let resolution = obj
        .get("resolution")
        .and_then(Value::as_str)
        .filter(|s| is_valid_identity_id(s))
        .ok_or("bad-exec-report-payload")?
        .to_string();
    let status = match obj.get("status").and_then(Value::as_str) {
        Some("accepted") => ExecReportStatus::Accepted,
        Some("in-progress") => ExecReportStatus::InProgress,
        Some("done") => ExecReportStatus::Done,
        _ => return Err("bad-exec-report-payload"),
    };
    let evidence = match obj.get("evidence") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or("bad-exec-report-payload")?
            .iter()
            .map(|e| {
                e.as_str()
                    .filter(|s| !s.is_empty() && s.encode_utf16().count() <= MAX_EVIDENCE_LEN_UTF16)
                    .map(str::to_string)
                    .ok_or("bad-exec-report-payload")
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    // done 必带佐证材料引用（§6.3）
    if status == ExecReportStatus::Done && evidence.is_empty() {
        return Err("bad-exec-report-payload");
    }
    Ok(ExecReportPayload {
        resolution,
        status,
        evidence,
    })
}

/// 执行状态（§6.2-3 状态机）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecState {
    /// 决议未锚定（无链上时间可推导，如实呈现，§7.2）。
    Unanchored,
    /// 待确认决议（公示期内）。
    ResolutionPending,
    /// 决议公示期内异议达阈值，打回复核。
    ResolutionVetoed,
    /// 生效决议，无有效回报，待执行。
    AwaitingExecution,
    /// 最新有效回报为 accepted / in-progress。
    InProgress,
    /// 最新有效回报为 done，核查中。
    Verifying,
    /// 核查不通过（异议达否决阈值），打回执行。
    Returned,
    /// 核查通过（终态）。
    Closed,
}

impl ExecState {
    /// 线形字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unanchored => "unanchored",
            Self::ResolutionPending => "resolution-pending",
            Self::ResolutionVetoed => "resolution-vetoed",
            Self::AwaitingExecution => "awaiting-execution",
            Self::InProgress => "in-progress",
            Self::Verifying => "verifying",
            Self::Returned => "returned",
            Self::Closed => "closed",
        }
    }
}

/// 执行推导的操作视图（求值输入 = 已验签有效操作集合 + 本副本锚定时刻，
/// 与 ladder.rs `LadderOpView` 同口径：未锚定操作不参与时间推导）。
#[derive(Clone, Debug, PartialEq)]
pub struct ExecOpView {
    /// 操作 opHash。
    pub op_hash: String,
    /// 操作者（已验签）。
    pub actor: Actor,
    /// 操作类型。
    pub op_type: OpType,
    /// 载荷原文。
    pub payload: Value,
    /// 本副本存证链锚定时刻（§7.2 时间源）。
    pub anchored_ms: Option<i64>,
}

/// 执行推导上下文（规则参数与时间由调用方注入）。
pub struct ExecCtx<'a> {
    /// 执行型事务声明（rules.exec，已过静态检查）。
    pub exec: &'a ExecDecl,
    /// 决议公示期毫秒（取规则文档版本，§6.2）。
    pub pub_period_ms: i64,
    /// 决议公示期否决阈值（§6.2）。
    pub pub_period_veto_count: u64,
    /// vote 核查名册（投票者资格过滤 + 基数；名册缺席 → vote 核查恒
    /// Verifying，fail-closed——空名册会使分数阈值平凡通过，必须显式拒绝）。
    pub roster: Option<&'a [String]>,
    /// 求值时刻。
    pub now_ms: i64,
}

/// 单条决议的执行状态推导结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ResolutionExecState {
    /// 决议 opHash。
    pub resolution_op_hash: String,
    /// 执行状态。
    pub state: ExecState,
    /// 当前状态依据的最新有效回报 opHash（无 = None）。
    pub report_op_hash: Option<String>,
    /// 决议锚定时刻。
    pub anchored_ms: Option<i64>,
    /// 决议生效时刻（= 锚定 + 规则版本公示期；未生效 = None）。
    pub effective_ms: Option<i64>,
}

/// 有效异议计数：opType=objection 且 payload.target == 目标 opHash。
fn count_objections(ops: &[ExecOpView], target: &str) -> u64 {
    ops.iter()
        .filter(|op| op.op_type == OpType::Objection)
        .filter(|op| op.payload.get("target").and_then(Value::as_str) == Some(target))
        .count() as u64
}

/// done 回报核查求值（§6.3 三形态）。返回 Decision 语义：Effective = 核查通过，
/// Pending = 核查中，Rejected = 核查不通过（打回执行）。
fn evaluate_verify(
    verify: &ExecVerify,
    report_hash: &str,
    report_anchored_ms: i64,
    ops: &[ExecOpView],
    ctx: &ExecCtx,
) -> Decision {
    match verify {
        ExecVerify::DelayedVeto {
            delay_ms,
            veto_count,
        } => evaluate_delayed_veto(
            *delay_ms,
            *veto_count,
            count_objections(ops, report_hash),
            report_anchored_ms,
            ctx.now_ms,
        ),
        ExecVerify::VerifierSign { verifiers } => {
            // 核查方确认 = vote 操作 {proposal: 回报 opHash, choice: "yes"}，
            // 按身份去重；全体核查方确认方通过（fail-closed）
            let mut confirmed: Vec<&str> = Vec::new();
            for op in ops.iter().filter(|op| op.op_type == OpType::Vote) {
                if op.payload.get("proposal").and_then(Value::as_str) != Some(report_hash) {
                    continue;
                }
                if op.payload.get("choice").and_then(Value::as_str) != Some("yes") {
                    continue;
                }
                if verifiers.iter().any(|v| v == &op.actor.identity)
                    && !confirmed.contains(&op.actor.identity.as_str())
                {
                    confirmed.push(&op.actor.identity);
                }
            }
            if confirmed.len() == verifiers.len() {
                Decision::Effective
            } else {
                Decision::Pending
            }
        }
        ExecVerify::Vote {
            threshold, quorum, ..
        } => {
            let Some(roster) = ctx.roster.filter(|r| !r.is_empty()) else {
                // 名册缺席/为空：恒核查中（fail-closed，防空名册平凡通过）
                return Decision::Pending;
            };
            let ballots: Vec<VoteBallot> = ops
                .iter()
                .filter(|op| op.op_type == OpType::Vote)
                .filter(|op| {
                    op.payload.get("proposal").and_then(Value::as_str) == Some(report_hash)
                })
                .filter(|op| roster.iter().any(|r| r == &op.actor.identity))
                .map(|op| VoteBallot {
                    op_hash: op.op_hash.clone(),
                    voter: op.actor.identity.clone(),
                    yes: op.payload.get("choice").and_then(Value::as_str) == Some("yes"),
                })
                .collect();
            evaluate_vote(threshold, quorum, &ballots, roster.len() as u64)
        }
    }
}

/// 执行状态推导（§6.2-3）：对操作集合逐决议确定性求值，与到达顺序、链拓扑
/// 无关（决议按 opHash 字典序输出；回报按 (anchored_ms, opHash) 取最新）。
/// `rules.exec == null` 的事务由调用方分流，本函数不适用（决议即终态）。
pub fn derive_exec_states(ops: &[ExecOpView], ctx: &ExecCtx) -> Vec<ResolutionExecState> {
    let mut resolutions: Vec<&ExecOpView> = ops
        .iter()
        .filter(|op| op.op_type == OpType::Resolution)
        .collect();
    resolutions.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
    let mut out = Vec::with_capacity(resolutions.len());
    for resolution in resolutions {
        let anchored_ms = resolution.anchored_ms;
        let Some(anchored) = anchored_ms else {
            out.push(ResolutionExecState {
                resolution_op_hash: resolution.op_hash.clone(),
                state: ExecState::Unanchored,
                report_op_hash: None,
                anchored_ms: None,
                effective_ms: None,
            });
            continue;
        };
        let objections = count_objections(ops, &resolution.op_hash);
        match super::resolution::resolution_state(
            anchored,
            ctx.pub_period_ms,
            objections,
            ctx.pub_period_veto_count,
            ctx.now_ms,
        ) {
            ResolutionState::Pending => out.push(ResolutionExecState {
                resolution_op_hash: resolution.op_hash.clone(),
                state: ExecState::ResolutionPending,
                report_op_hash: None,
                anchored_ms,
                effective_ms: None,
            }),
            ResolutionState::Vetoed => out.push(ResolutionExecState {
                resolution_op_hash: resolution.op_hash.clone(),
                state: ExecState::ResolutionVetoed,
                report_op_hash: None,
                anchored_ms,
                effective_ms: None,
            }),
            ResolutionState::Effective => {
                let effective_ms = anchored + ctx.pub_period_ms;
                // 有效回报：结构可解析 + 引用本决议 + 声明执行方签署 +
                // 锚定不早于决议生效时刻（纯链上时间门控；生效前回报永久惰性）
                let mut reports: Vec<&ExecOpView> = ops
                    .iter()
                    .filter(|op| op.op_type == OpType::ExecReport)
                    .filter(|op| {
                        parse_exec_report_payload(&op.payload)
                            .is_ok_and(|p| p.resolution == resolution.op_hash)
                    })
                    .filter(|op| executor_matches(&ctx.exec.executor, &op.actor))
                    .filter(|op| op.anchored_ms.is_some_and(|t| t >= effective_ms))
                    .collect();
                reports
                    .sort_by(|a, b| (a.anchored_ms, &a.op_hash).cmp(&(b.anchored_ms, &b.op_hash)));
                let Some(latest) = reports.last() else {
                    out.push(ResolutionExecState {
                        resolution_op_hash: resolution.op_hash.clone(),
                        state: ExecState::AwaitingExecution,
                        report_op_hash: None,
                        anchored_ms,
                        effective_ms: Some(effective_ms),
                    });
                    continue;
                };
                let payload =
                    parse_exec_report_payload(&latest.payload).expect("report payload re-parse");
                let report_anchored = latest.anchored_ms.expect("report anchored");
                let state = match payload.status {
                    ExecReportStatus::Accepted | ExecReportStatus::InProgress => {
                        ExecState::InProgress
                    }
                    ExecReportStatus::Done => match evaluate_verify(
                        &ctx.exec.verify,
                        &latest.op_hash,
                        report_anchored,
                        ops,
                        ctx,
                    ) {
                        Decision::Effective => ExecState::Closed,
                        Decision::Pending => ExecState::Verifying,
                        Decision::Rejected => ExecState::Returned,
                    },
                };
                out.push(ResolutionExecState {
                    resolution_op_hash: resolution.op_hash.clone(),
                    state,
                    report_op_hash: Some(latest.op_hash.clone()),
                    anchored_ms,
                    effective_ms: Some(effective_ms),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const T0: i64 = 1_720_000_000_000;

    fn actor(identity: &str) -> Actor {
        Actor {
            kind: ActorKind::Person,
            identity: identity.to_string(),
            public_key: String::new(),
            org_sig: None,
        }
    }

    fn view(
        hash: &str,
        identity: &str,
        op_type: OpType,
        payload: Value,
        anchored_ms: Option<i64>,
    ) -> ExecOpView {
        ExecOpView {
            op_hash: hash.repeat(64 / hash.len()),
            actor: actor(identity),
            op_type,
            payload,
            anchored_ms,
        }
    }

    fn exec_decl() -> ExecDecl {
        parse_exec_decl(&json!({
            "executor": { "kind": "person", "identity": "ee".repeat(32) },
            "verify": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } }
        }))
        .unwrap()
    }

    fn ctx<'a>(exec: &'a ExecDecl, now_ms: i64) -> ExecCtx<'a> {
        ExecCtx {
            exec,
            pub_period_ms: DAY,
            pub_period_veto_count: 1,
            roster: None,
            now_ms,
        }
    }

    fn resolution(hash: &str, anchored_ms: Option<i64>) -> ExecOpView {
        view(hash, "aa", OpType::Resolution, json!({}), anchored_ms)
    }

    fn done_report(hash: &str, identity: &str, resolution: &str, anchored_ms: i64) -> ExecOpView {
        view(
            hash,
            identity,
            OpType::ExecReport,
            json!({ "resolution": resolution, "status": "done", "evidence": ["ev:1"] }),
            Some(anchored_ms),
        )
    }

    #[test]
    fn parse_decl_static_check() {
        // executor 必填
        assert_eq!(
            parse_exec_decl(&json!({ "verify": { "kind": "delayed-veto", "delayMs": DAY } })),
            Err(StaticCheckReject::InvalidExec)
        );
        // 核查公示期下限 24h
        assert_eq!(
            parse_exec_decl(&json!({
                "executor": { "kind": "person", "identity": "ee".repeat(32) },
                "verify": { "kind": "delayed-veto", "delayMs": DAY - 1 }
            })),
            Err(StaticCheckReject::InvalidExec)
        );
        // verifier-sign：空列表/重复/非法身份均拒
        for verifiers in [
            json!([]),
            json!(["vv".repeat(32), "vv".repeat(32)]),
            json!(["not-hex"]),
        ] {
            assert_eq!(
                parse_exec_decl(&json!({
                    "executor": { "kind": "person", "identity": "ee".repeat(32) },
                    "verify": { "kind": "verifier-sign", "verifiers": verifiers }
                })),
                Err(StaticCheckReject::InvalidExec)
            );
        }
        // vote：复用机制分数校验（num > den 拒）
        assert_eq!(
            parse_exec_decl(&json!({
                "executor": { "kind": "person", "identity": "ee".repeat(32) },
                "verify": { "kind": "vote", "voterSet": "ladder:voters",
                    "threshold": { "num": 3, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                    "snapshot": "required" }
            })),
            Err(StaticCheckReject::InvalidExec)
        );
        // vetoThreshold 缺省 = 1
        let decl = parse_exec_decl(&json!({
            "executor": { "kind": "person", "identity": "ee".repeat(32) },
            "verify": { "kind": "delayed-veto", "delayMs": DAY }
        }))
        .unwrap();
        assert_eq!(
            decl.verify,
            ExecVerify::DelayedVeto {
                delay_ms: DAY,
                veto_count: 1
            }
        );
    }

    #[test]
    fn report_payload_shape() {
        // done 必带 evidence
        assert_eq!(
            parse_exec_report_payload(&json!({ "resolution": "aa".repeat(32), "status": "done" })),
            Err("bad-exec-report-payload")
        );
        // 未知状态拒
        assert_eq!(
            parse_exec_report_payload(&json!({ "resolution": "aa".repeat(32), "status": "bogus" })),
            Err("bad-exec-report-payload")
        );
        // accepted 可无 evidence
        let p = parse_exec_report_payload(
            &json!({ "resolution": "aa".repeat(32), "status": "accepted" }),
        )
        .unwrap();
        assert_eq!(p.status, ExecReportStatus::Accepted);
    }

    #[test]
    fn delayed_veto_lifecycle() {
        let exec = exec_decl();
        let res = resolution("22", Some(T0));
        let report = done_report("dd", &"ee".repeat(32), &"22".repeat(32), T0 + 2 * DAY);
        let ops = vec![res, report];
        // 公示期内 → resolution-pending
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + DAY / 2));
        assert_eq!(out[0].state, ExecState::ResolutionPending);
        // 生效 + done 回报 → 核查中（核查期 = 回报锚定 + 3 天）
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 2 * DAY));
        assert_eq!(out[0].state, ExecState::Verifying);
        assert_eq!(out[0].effective_ms, Some(T0 + DAY));
        // 核查期满无异议 → closed
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 5 * DAY));
        assert_eq!(out[0].state, ExecState::Closed);
        // 乱序等价
        let mut shuffled = ops.clone();
        shuffled.reverse();
        assert_eq!(
            derive_exec_states(&shuffled, &ctx(&exec, T0 + 5 * DAY)),
            out
        );
    }

    #[test]
    fn veto_returns_then_re_report() {
        let exec = exec_decl();
        let res = resolution("22", Some(T0));
        let done1 = done_report("d1", &"ee".repeat(32), &"22".repeat(32), T0 + 2 * DAY);
        let objection = view(
            "ob",
            "aa",
            OpType::Objection,
            json!({ "target": "d1".repeat(32) }),
            Some(T0 + 2 * DAY + 1),
        );
        let done2 = done_report("d2", &"ee".repeat(32), &"22".repeat(32), T0 + 4 * DAY);
        let ops = vec![res, done1, objection, done2.clone()];
        // 最新回报 = done2（无异议）→ 核查中/关闭
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 5 * DAY));
        assert_eq!(out[0].state, ExecState::Verifying);
        assert_eq!(out[0].report_op_hash, Some("d2".repeat(32)));
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 7 * DAY));
        assert_eq!(out[0].state, ExecState::Closed);
        // 无 done2 时：done1 异议达阈值 → returned
        let ops = vec![ops[0].clone(), ops[1].clone(), ops[2].clone()];
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 3 * DAY));
        assert_eq!(out[0].state, ExecState::Returned);
        // 再回报后重新回到核查 → 闭环
        let mut ops = ops;
        ops.push(done2);
        let out = derive_exec_states(&ops, &ctx(&exec, T0 + 7 * DAY));
        assert_eq!(out[0].state, ExecState::Closed);
    }

    #[test]
    fn inert_reports_excluded() {
        let exec = exec_decl();
        let res = resolution("22", Some(T0));
        // 生效前发出的回报（锚定 < 生效时刻）→ 永久惰性
        let premature = done_report("dd", &"ee".repeat(32), &"22".repeat(32), T0 + DAY / 2);
        let out = derive_exec_states(&[res.clone(), premature], &ctx(&exec, T0 + 10 * DAY));
        assert_eq!(out[0].state, ExecState::AwaitingExecution);
        // 非执行方签署的回报 → 惰性
        let rogue = done_report("dd", &"aa".repeat(32), &"22".repeat(32), T0 + 2 * DAY);
        let out = derive_exec_states(&[res, rogue], &ctx(&exec, T0 + 10 * DAY));
        assert_eq!(out[0].state, ExecState::AwaitingExecution);
    }

    #[test]
    fn verifier_sign_all_must_confirm() {
        let exec = parse_exec_decl(&json!({
            "executor": { "kind": "person", "identity": "ee".repeat(32) },
            "verify": { "kind": "verifier-sign", "verifiers": ["1a".repeat(32), "2a".repeat(32)] }
        }))
        .unwrap();
        let res = resolution("22", Some(T0));
        let report = done_report("dd", &"ee".repeat(32), &"22".repeat(32), T0 + 2 * DAY);
        let yes1 = view(
            "c1",
            &"1a".repeat(32),
            OpType::Vote,
            json!({ "proposal": "dd".repeat(32), "choice": "yes" }),
            Some(T0 + 3 * DAY),
        );
        // 仅一人确认 → 核查中
        let out = derive_exec_states(
            &[res.clone(), report.clone(), yes1.clone()],
            &ctx(&exec, T0 + 3 * DAY),
        );
        assert_eq!(out[0].state, ExecState::Verifying);
        // 非核查方确认不计
        let outsider = view(
            "c9",
            &"aa".repeat(32),
            OpType::Vote,
            json!({ "proposal": "dd".repeat(32), "choice": "yes" }),
            Some(T0 + 3 * DAY),
        );
        let out = derive_exec_states(
            &[res.clone(), report.clone(), yes1.clone(), outsider],
            &ctx(&exec, T0 + 3 * DAY),
        );
        assert_eq!(out[0].state, ExecState::Verifying);
        // 全体确认 → closed
        let yes2 = view(
            "c2",
            &"2a".repeat(32),
            OpType::Vote,
            json!({ "proposal": "dd".repeat(32), "choice": "yes" }),
            Some(T0 + 3 * DAY),
        );
        let out = derive_exec_states(&[res, report, yes1, yes2], &ctx(&exec, T0 + 3 * DAY));
        assert_eq!(out[0].state, ExecState::Closed);
    }

    #[test]
    fn vote_verify_threshold_and_empty_roster_fail_closed() {
        let exec = parse_exec_decl(&json!({
            "executor": { "kind": "person", "identity": "ee".repeat(32) },
            "verify": { "kind": "vote", "voterSet": "ladder:voters",
                "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                "snapshot": "required" }
        }))
        .unwrap();
        let res = resolution("22", Some(T0));
        let report = done_report("dd", &"ee".repeat(32), &"22".repeat(32), T0 + 2 * DAY);
        let yes = |hash: &str, voter: &str| {
            view(
                hash,
                voter,
                OpType::Vote,
                json!({ "proposal": "dd".repeat(32), "choice": "yes" }),
                Some(T0 + 3 * DAY),
            )
        };
        let roster: Vec<String> = vec!["aa".repeat(32), "bb".repeat(32), "cc".repeat(32)];
        let vote_ctx = |now_ms: i64| ExecCtx {
            exec: &exec,
            pub_period_ms: DAY,
            pub_period_veto_count: 1,
            roster: Some(&roster),
            now_ms,
        };
        // 1/3 yes < 1/2 → 核查中
        let ops = vec![res.clone(), report.clone(), yes("b1", &"aa".repeat(32))];
        assert_eq!(
            derive_exec_states(&ops, &vote_ctx(T0 + 3 * DAY))[0].state,
            ExecState::Verifying
        );
        // 2/3 yes ≥ 1/2 且参与 2/3 ≥ 1/2 → closed
        let ops = vec![
            res.clone(),
            report.clone(),
            yes("b1", &"aa".repeat(32)),
            yes("b2", &"bb".repeat(32)),
        ];
        assert_eq!(
            derive_exec_states(&ops, &vote_ctx(T0 + 3 * DAY))[0].state,
            ExecState::Closed
        );
        // 名册缺席 → 恒核查中（fail-closed）
        assert_eq!(
            derive_exec_states(&ops, &ctx(&exec, T0 + 3 * DAY))[0].state,
            ExecState::Verifying
        );
    }
}
