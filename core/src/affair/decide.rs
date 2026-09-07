//! 集体决策求值（wiki/protocol/community/affair.md §5.3/§5.4）。
//!
//! 三形态有效性判定（内核纯逻辑）：vote / multisig / delayed-veto。
//! 两条内核硬编码不可变校验：
//! - 单点禁令：规则表达层的静态检查在 rules.rs（multisig m<2 拒绝）；
//! - 失联经延迟否决解锁：现行机制为 multisig/vote 且提议修改 `ruleChange`
//!   键本身时，允许以 delayed-veto 形态求值（多签失联死锁的恢复路径）。

use serde_json::Value;

use super::actor::{is_valid_identity_id, verify_actor_signature};
use super::rules::{
    Fraction, Mechanism, StaticCheckReject, apply_rule_patch, parse_mechanism, static_check_rules,
};
use crate::evidence::normalize_object;

/// 决策求值结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// 生效条件已满足。
    Effective,
    /// 被否决（达到否决阈值 / 多签分量不足——提议可重提）。
    Rejected,
    /// 待定（窗口未满 / 票数未达阈值，仍可继续累积）。
    Pending,
}

/// 表决票（已验签、已按快照名册过滤后的输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteBallot {
    /// 票所在操作的 opHash（同一投票者多票时按 §8 排序键取首票）。
    pub op_hash: String,
    /// 投票者身份。
    pub voter: String,
    /// true = yes。
    pub yes: bool,
}

/// 去重计票：一人一票（§「一人一票不加权」），同投票者多票取 opHash 字典序首票
/// （确定性，与到达顺序无关）。返回（同意票数，参与数）。
pub fn tally_votes(ballots: &[VoteBallot]) -> (u64, u64) {
    let mut sorted = ballots.to_vec();
    sorted.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
    let mut seen: Vec<&str> = Vec::new();
    let mut yes = 0u64;
    for ballot in &sorted {
        if seen.contains(&ballot.voter.as_str()) {
            continue;
        }
        seen.push(&ballot.voter);
        yes += u64::from(ballot.yes);
    }
    (yes, seen.len() as u64)
}

/// vote 形态求值：同意票/快照基数 ≥ threshold 且参与数/基数 ≥ quorum。
/// `roster_size` = §9 快照名册基数。
pub fn evaluate_vote(
    threshold: &Fraction,
    quorum: &Fraction,
    ballots: &[VoteBallot],
    roster_size: u64,
) -> Decision {
    let (yes, participated) = tally_votes(ballots);
    if threshold.reached(yes, roster_size) && quorum.reached(participated, roster_size) {
        Decision::Effective
    } else {
        Decision::Pending
    }
}

/// delayed-veto 形态求值：提议锚定时刻 + delayMs 内有效异议 < vetoThreshold
/// → 生效；达到阈值 → 否决。`anchored_ms` 为锚定时刻（§7.2 时间源），`now_ms`
/// 为求值时刻，均由调用方注入。
pub fn evaluate_delayed_veto(
    delay_ms: i64,
    veto_count: u64,
    objection_count: u64,
    anchored_ms: i64,
    now_ms: i64,
) -> Decision {
    if objection_count >= veto_count {
        return Decision::Rejected;
    }
    if now_ms >= anchored_ms + delay_ms {
        Decision::Effective
    } else {
        Decision::Pending
    }
}

/// rule-change 提议本体（§5.4 payload 解析结果）。
#[derive(Clone, Debug, PartialEq)]
pub struct RuleChangeProposal {
    /// 提议声明的求值机制。
    pub mechanism: Mechanism,
    /// 机制原文（approvals 载荷与机制匹配比对用）。
    pub mechanism_raw: Value,
    /// 新规则文档片段（patch 语义：顶层键覆盖，null = 删除）。
    pub change: Value,
    /// 多签分量（仅 multisig 形态携带）。
    pub approvals: Vec<Value>,
    /// 提议声明时刻。
    pub proposed_at: i64,
}

/// 解析 rule-change payload（§5.4）。approvals 仅 multisig 形态可携带。
pub fn parse_rule_change_payload(payload: &Value) -> Result<RuleChangeProposal, &'static str> {
    let obj = payload.as_object().ok_or("bad-rule-change-payload")?;
    let mechanism_raw = obj
        .get("mechanism")
        .cloned()
        .ok_or("bad-rule-change-payload")?;
    let mechanism = parse_mechanism(&mechanism_raw).map_err(|_| "bad-rule-change-mechanism")?;
    let change = obj
        .get("change")
        .cloned()
        .ok_or("bad-rule-change-payload")?;
    if !change.is_object() {
        return Err("bad-rule-change-payload");
    }
    let approvals = match obj.get("approvals") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => v.as_array().cloned().ok_or("bad-rule-change-payload")?,
    };
    if !approvals.is_empty() && !matches!(mechanism, Mechanism::Multisig { .. }) {
        // approvals 仅 multisig 形态携带（§5.4）
        return Err("bad-rule-change-payload");
    }
    for approval in &approvals {
        let entry = approval.as_object().ok_or("bad-rule-change-payload")?;
        let identity_ok = entry
            .get("identity")
            .and_then(Value::as_str)
            .is_some_and(is_valid_identity_id);
        let keys_ok = entry.get("publicKey").and_then(Value::as_str).is_some()
            && entry.get("sig").and_then(Value::as_str).is_some();
        if !identity_ok || !keys_ok {
            return Err("bad-rule-change-payload");
        }
    }
    let proposed_at = obj
        .get("proposedAt")
        .and_then(Value::as_i64)
        .ok_or("bad-rule-change-payload")?;
    Ok(RuleChangeProposal {
        mechanism,
        mechanism_raw,
        change,
        approvals,
        proposed_at,
    })
}

/// 多签分量的签名载荷（§5.4）：`canonical({affairId, change, mechanism,
/// proposalPrevOpHash, proposedAt})`——即条目剔除 actor/sig/approvals 的提议本体。
pub fn rule_change_approval_payload(
    affair_id: &str,
    proposal: &RuleChangeProposal,
    proposal_prev_op_hash: &str,
) -> String {
    normalize_object(&serde_json::json!({
        "affairId": affair_id,
        "change": proposal.change,
        "mechanism": proposal.mechanism_raw,
        "proposalPrevOpHash": proposal_prev_op_hash,
        "proposedAt": proposal.proposed_at,
    }))
}

/// multisig 形态求值（§5.3）：≥m 个 signers 内**不同身份**的分量，逐一验签。
pub fn evaluate_multisig(
    m: u32,
    signers: &[String],
    approvals: &[Value],
    approval_payload: &str,
) -> Decision {
    let mut approved: Vec<String> = Vec::new();
    for approval in approvals {
        let Some(entry) = approval.as_object() else {
            continue;
        };
        let (Some(identity), Some(public_key), Some(sig)) = (
            entry.get("identity").and_then(Value::as_str),
            entry.get("publicKey").and_then(Value::as_str),
            entry.get("sig").and_then(Value::as_str),
        ) else {
            continue;
        };
        if approved.contains(&identity.to_string()) || !signers.contains(&identity.to_string()) {
            continue;
        }
        // parse_actor 内含公钥-身份绑定校验（identity == sha256hex(publicKey)）
        let Ok(actor) = super::actor::parse_actor(&serde_json::json!({
            "kind": "person", "identity": identity, "publicKey": public_key
        })) else {
            continue;
        };
        if verify_actor_signature(&actor, approval_payload, sig) {
            approved.push(identity.to_string());
        }
    }
    if approved.len() >= m as usize {
        Decision::Effective
    } else {
        Decision::Rejected
    }
}

/// rule-change 求值上下文（已验签、已过滤的输入集合）。
pub struct RuleChangeCtx<'a> {
    /// 所属事务。
    pub affair_id: &'a str,
    /// 提议条目的 prevOpHash（approvals 载荷分量）。
    pub proposal_prev_op_hash: &'a str,
    /// 现行规则文档原文。
    pub current_rules: &'a Value,
    /// 现行机制（static_check_rules 已解析）。
    pub current_mechanism: &'a Mechanism,
    /// vote 形态：有效票（按快照名册过滤后）与名册基数。
    pub ballots: &'a [VoteBallot],
    /// vote 形态：名册基数。
    pub roster_size: u64,
    /// delayed-veto 形态：有效异议数。
    pub objection_count: u64,
    /// delayed-veto 形态：提议锚定时刻（§7.2）。
    pub anchored_ms: i64,
    /// 求值时刻。
    pub now_ms: i64,
}

/// rule-change 求值结果。
#[derive(Clone, Debug, PartialEq)]
pub enum RuleChangeOutcome {
    /// 生效：附 patch 应用后的新规则文档（已过静态检查）。
    Effective {
        /// 新规则文档。
        new_rules: Value,
    },
    /// 无效（入无效集；reason 稳定）。
    Rejected(String),
    /// 待定（窗口未满 / 票数未达）。
    Pending,
}

/// rule-change 有效性判定（§5.3/§5.4 + 失联解锁硬编码）。
///
/// 求值机制选择：默认要求提议声明的机制与现行 `ruleChange` 逐项一致；
/// 例外（硬编码失联解锁）：现行机制为 multisig/vote 且 `change` 触及
/// `ruleChange` 键时，允许以 delayed-veto 形态求值。
pub fn evaluate_rule_change(
    proposal: &RuleChangeProposal,
    ctx: &RuleChangeCtx,
) -> RuleChangeOutcome {
    let touches_rule_change = proposal
        .change
        .as_object()
        .is_some_and(|c| c.contains_key("ruleChange"));
    let current_is_recoverable = matches!(
        ctx.current_mechanism,
        Mechanism::Multisig { .. } | Mechanism::Vote { .. }
    );
    let unlock_path = touches_rule_change
        && current_is_recoverable
        && matches!(proposal.mechanism, Mechanism::DelayedVeto { .. });
    if !unlock_path && proposal.mechanism != *ctx.current_mechanism {
        return RuleChangeOutcome::Rejected("mechanism-mismatch".to_string());
    }
    // patch 应用后的新规则文档必须先过静态检查（§5.6：不得借修改关闭
    // 单点禁令/延迟否决解锁等硬校验）
    let new_rules = apply_rule_patch(ctx.current_rules, &proposal.change);
    if let Err(reject) = static_check_rules(&new_rules) {
        return RuleChangeOutcome::Rejected(format!("rules-rejected:{}", reject.reason()));
    }
    let decision = match &proposal.mechanism {
        Mechanism::Vote {
            threshold, quorum, ..
        } => evaluate_vote(threshold, quorum, ctx.ballots, ctx.roster_size),
        Mechanism::Multisig { m, signers, .. } => {
            let payload =
                rule_change_approval_payload(ctx.affair_id, proposal, ctx.proposal_prev_op_hash);
            evaluate_multisig(*m, signers, &proposal.approvals, &payload)
        }
        Mechanism::DelayedVeto {
            delay_ms,
            veto_count,
        } => evaluate_delayed_veto(
            *delay_ms,
            *veto_count,
            ctx.objection_count,
            ctx.anchored_ms,
            ctx.now_ms,
        ),
    };
    match decision {
        Decision::Effective => RuleChangeOutcome::Effective { new_rules },
        Decision::Rejected => RuleChangeOutcome::Rejected("decision-rejected".to_string()),
        Decision::Pending => RuleChangeOutcome::Pending,
    }
}

/// 供规则表达式比对的机制解析（提议机制一致性检查用）。
pub fn mechanism_from_rules(rules: &Value) -> Result<Mechanism, StaticCheckReject> {
    let obj = rules.as_object().ok_or(StaticCheckReject::Malformed)?;
    parse_mechanism(obj.get("ruleChange").ok_or(StaticCheckReject::Malformed)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;

    #[test]
    fn tally_dedupes_by_first_sort_key() {
        let ballots = vec![
            VoteBallot {
                op_hash: "ff".repeat(32),
                voter: "aa".repeat(32),
                yes: false,
            },
            VoteBallot {
                op_hash: "11".repeat(32),
                voter: "aa".repeat(32),
                yes: true,
            },
            VoteBallot {
                op_hash: "22".repeat(32),
                voter: "bb".repeat(32),
                yes: true,
            },
        ];
        // 同一 voter 取 opHash 字典序首票（"11.." < "ff.."）→ yes
        assert_eq!(tally_votes(&ballots), (2, 2));
    }

    #[test]
    fn vote_threshold_and_quorum() {
        let half = Fraction { num: 1, den: 2 };
        let ballots = vec![
            VoteBallot {
                op_hash: "11".repeat(32),
                voter: "aa".repeat(32),
                yes: true,
            },
            VoteBallot {
                op_hash: "22".repeat(32),
                voter: "bb".repeat(32),
                yes: true,
            },
        ];
        assert_eq!(
            evaluate_vote(&half, &half, &ballots, 4),
            Decision::Effective
        );
        assert_eq!(
            evaluate_vote(&half, &half, &ballots[..1], 4),
            Decision::Pending
        );
    }

    #[test]
    fn delayed_veto_window() {
        assert_eq!(
            evaluate_delayed_veto(1000, 3, 2, 10_000, 10_999),
            Decision::Pending
        );
        assert_eq!(
            evaluate_delayed_veto(1000, 3, 2, 10_000, 11_000),
            Decision::Effective
        );
        assert_eq!(
            evaluate_delayed_veto(1000, 3, 3, 10_000, 11_000),
            Decision::Rejected
        );
    }

    #[test]
    fn rule_change_mechanism_match_and_unlock() {
        let rules = json!({
            "engine": "b1",
            "closeConditions": [],
            "pubPeriod": { "delayMs": 86400000 },
            "ruleChange": { "kind": "multisig", "m": 2, "n": 3, "signers": ["aa".repeat(32), "bb".repeat(32), "cc".repeat(32)] },
            "exec": null
        });
        let current = mechanism_from_rules(&rules).unwrap();
        let affair_id = "dd".repeat(32);
        let prev = "ee".repeat(32);
        let make_ctx = |now_ms: i64| RuleChangeCtx {
            affair_id: &affair_id,
            proposal_prev_op_hash: &prev,
            current_rules: &rules,
            current_mechanism: &current,
            ballots: &[],
            roster_size: 0,
            objection_count: 0,
            anchored_ms: 0,
            now_ms,
        };
        // 机制不一致（非解锁路径）→ 拒绝
        let proposal = parse_rule_change_payload(&json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
            "change": { "exec": null },
            "proposedAt": 1
        }))
        .unwrap();
        assert!(matches!(
            evaluate_rule_change(&proposal, &make_ctx(100)),
            RuleChangeOutcome::Rejected(r) if r == "mechanism-mismatch"
        ));
        // 失联解锁：multisig 现行 + 修改 ruleChange 键 + delayed-veto 形态
        let proposal = parse_rule_change_payload(&json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 50, "vetoThreshold": { "count": 1 } },
            "change": { "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } } },
            "proposedAt": 1
        }))
        .unwrap();
        assert!(matches!(
            evaluate_rule_change(&proposal, &make_ctx(50)),
            RuleChangeOutcome::Effective { .. }
        ));
    }

    struct MsKey {
        signing_key: ed25519_dalek::SigningKey,
        identity: String,
        public_key: String,
    }

    fn ms_key(seed: u8) -> MsKey {
        use sha2::Digest as _;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let public_key_bytes = signing_key.verifying_key().to_bytes();
        MsKey {
            signing_key,
            public_key: base64::engine::general_purpose::STANDARD.encode(public_key_bytes),
            identity: hex::encode(sha2::Sha256::digest(public_key_bytes)),
        }
    }

    fn ms_approval(key: &MsKey, payload: &str) -> Value {
        use ed25519_dalek::Signer as _;
        let sig = key.signing_key.sign(payload.as_bytes());
        json!({
            "identity": key.identity,
            "publicKey": key.public_key,
            "sig": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        })
    }

    #[test]
    fn multisig_counts_distinct_valid_signers() {
        let a = ms_key(0x21);
        let b = ms_key(0x22);
        let c = ms_key(0x23);
        let signers = vec![a.identity.clone(), b.identity.clone(), c.identity.clone()];
        // 两个不同 signer 的合法签名 → 生效
        let approvals = vec![ms_approval(&a, "p"), ms_approval(&b, "p")];
        assert_eq!(
            evaluate_multisig(2, &signers, &approvals, "p"),
            Decision::Effective
        );
        // 同一 signer 重复签名只计一次 → 不足 m → 拒绝
        let approvals = vec![ms_approval(&a, "p"), ms_approval(&a, "p")];
        assert_eq!(
            evaluate_multisig(2, &signers, &approvals, "p"),
            Decision::Rejected
        );
    }

    #[test]
    fn multisig_rejects_forged_approval() {
        let a = ms_key(0x21);
        let signers = vec![a.identity.clone()];
        // 伪造 approval：identity/publicKey 冒用 signer，sig 为 64 零字节
        let mut forged = ms_approval(&a, "p");
        forged["sig"] = json!(base64::engine::general_purpose::STANDARD.encode([0u8; 64]));
        assert_eq!(
            evaluate_multisig(1, &signers, &[forged], "p"),
            Decision::Rejected
        );
    }

    #[test]
    fn multisig_rejects_non_signer_identity() {
        let a = ms_key(0x21);
        let outsider = ms_key(0x42);
        let signers = vec![a.identity.clone()];
        // outsider 的签名本身合法，但身份不在 signers 内 → 不计入
        let approvals = vec![ms_approval(&outsider, "p"), ms_approval(&a, "p")];
        assert_eq!(
            evaluate_multisig(2, &signers, &approvals, "p"),
            Decision::Rejected
        );
        assert_eq!(
            evaluate_multisig(1, &signers, &[ms_approval(&outsider, "p")], "p"),
            Decision::Rejected
        );
    }

    #[test]
    fn multisig_rejects_wrong_payload_signature() {
        let a = ms_key(0x21);
        let signers = vec![a.identity.clone()];
        // signer 对「错载荷」的合法签名：验签按求值载荷比对 → 不计入
        let approvals = vec![ms_approval(&a, "other-payload")];
        assert_eq!(
            evaluate_multisig(1, &signers, &approvals, "p"),
            Decision::Rejected
        );
    }
}
