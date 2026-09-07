//! 规则文档版本链 replay（wiki/protocol/community/affair.md §5.3/§5.4：
//! 「规则的每一版本由规则修改链确定性可溯」）。
//!
//! 创世规则为第 0 版；逐条 rule-change 操作经 [`super::decide::evaluate_rule_change`]
//! 求值，生效者按（生效时刻, opHash 字典序）确定序应用 patch 递进版本——与到达
//! 顺序、链拓扑无关（§3.3 纪律）。replay 只依赖操作集合内容与注入的锚定时刻/
//! 求值时刻，不读本地时钟。
//!
//! 规格未逐字钉死处的实现判定（最小可用，如实登记）：
//!
//! - **生效时刻判定**（§5.3 只钉有效性，未钉生效时刻的链上表达）：
//!   delayed-veto = 锚定 + delayMs（§13 采纳同口径）；multisig = 提议锚定时刻
//!   （approvals 随条目携带，无时间窗）；vote = 使计票首次同时越过 threshold
//!   与 quorum 的那张票的锚定时刻（去重口径同 `tally_votes`，有锚票按
//!   （锚定时刻, opHash）走查；无锚票参与计数但不参与生效时刻推导——生效时刻
//!   无从确定时按待定处理，fail-closed，§7.2 时间源纪律）；
//! - **vote 形态名册**：调用方注入（生产侧 = §9 投票前快照解析）；名册缺席或
//!   为空 → 恒待定（fail-closed，防空名册下 `Fraction::reached(_, 0)` 平凡通过，
//!   同 exec.rs vote 核查口径）；
//! - **未锚定提议**：不参与一切时间推导，恒待定（等更晚锚定，不随 now 翻转出
//!   生效态之外的结论）；
//! - **版本间依赖**：每轮以求值时的现行版本为基准（机制一致性比对随版本演进），
//!   迭代至无新生效为止（不动点）；同轮多个生效取（生效时刻, opHash）最小者先
//!   应用，平局可判；
//! - **vote 形态修改 `participation.ladderParams`** 时，后续 vote 提议的名册推导
//!   参数不含该修改（名册解析所需阶梯参数取自「非 vote 形态规则修改链」的现行
//!   版本，以断开「链 → 名册 → 参数 → 链」循环；调用方职责，本模块只见注入值）。

use std::collections::HashSet;

use serde_json::Value;

use super::decide::{
    RuleChangeCtx, RuleChangeOutcome, RuleChangeProposal, VoteBallot, evaluate_rule_change,
};
use super::rules::{Mechanism, RulesDoc, StaticCheckReject, rules_hash, static_check_rules};

/// 带锚定时刻的表决票（vote 形态生效时刻推导用；已按快照名册过滤）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchoredBallot {
    /// 票所在操作的 opHash。
    pub op_hash: String,
    /// 投票者身份。
    pub voter: String,
    /// true = yes。
    pub yes: bool,
    /// 本副本存证链锚定时刻；None = 未锚定（参与计数，不参与生效时刻推导）。
    pub anchored_ms: Option<i64>,
}

/// rule-change 求值条目（已验签入有效集的输入 + 调用方注入的上下文）。
#[derive(Clone, Debug)]
pub struct RuleChangeEntry {
    /// 条目 opHash。
    pub op_hash: String,
    /// 条目 prevOpHash（multisig approvals 载荷分量）。
    pub prev_op_hash: String,
    /// 解析后的提议本体（§5.4）。
    pub proposal: RuleChangeProposal,
    /// 条目锚定时刻（§7.2）；None = 未锚定，恒待定。
    pub anchored_ms: Option<i64>,
    /// 指向该条目的有效异议数（delayed-veto 求值输入）。
    pub objection_count: u64,
    /// vote 形态：表决票（已按快照名册过滤资格）。
    pub ballots: Vec<AnchoredBallot>,
    /// vote 形态：快照名册基数；None/0 → 恒待定（fail-closed）。
    pub roster_size: Option<u64>,
}

/// 规则文档版本（链上一环）。
#[derive(Clone, Debug, PartialEq)]
pub struct RulesVersion {
    /// 版本序号（创世 = 0，每次生效修改 +1）。
    pub seq: u64,
    /// 版本依据：seq=0 → affairId；seq>0 → 生效的 rule-change opHash。
    pub basis_op_hash: String,
    /// 该版本规则文档原文。
    pub rules: Value,
    /// `rulesHash`（§6.1 口径）。
    pub rules_hash: String,
    /// 生效时刻（创世 = None，自创世起有效）。
    pub effective_ms: Option<i64>,
}

/// 未生效条目的归宿（reason 字符串稳定）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleChangeFate {
    /// 待定：窗口未满 / 票数未达 / 生效时刻无从确定。
    Pending(&'static str),
    /// 无效：机制不符 / 静态检查不过 / 决策否决（入无效集，可告警）。
    Rejected(String),
}

/// 规则链 replay 产物。
#[derive(Clone, Debug)]
pub struct RuleChain {
    /// 版本序列（按应用序，首元素恒为创世版本）。
    pub versions: Vec<RulesVersion>,
    /// 未生效条目归宿（opHash 字典序，确定性）。
    pub fates: Vec<(String, RuleChangeFate)>,
    /// 现行版本静态检查视图（公示期/否决阈值/现行机制等参数的读口）。
    pub doc: RulesDoc,
}

impl RuleChain {
    /// 现行规则文档版本（versions 末元素）。
    pub fn current(&self) -> &RulesVersion {
        self.versions.last().expect("rule chain always has genesis version")
    }

    /// 按 rulesHash 定位版本（决议复算取「判定所用规则文档版本」，§6.1）。
    pub fn version_by_hash(&self, hash: &str) -> Option<&RulesVersion> {
        self.versions.iter().find(|v| v.rules_hash == hash)
    }
}

/// vote 形态生效时刻：去重（同 `tally_votes` 口径：按 opHash 序取每人首票）后，
/// 有锚票按（锚定时刻, opHash）走查，返回首个同时越过 threshold 与 quorum 的
/// 前缀末票的锚定时刻；有锚票永远无法越过（或根本无票）→ None。
fn vote_effective_ms(
    threshold: &super::rules::Fraction,
    quorum: &super::rules::Fraction,
    ballots: &[AnchoredBallot],
    roster_size: u64,
) -> Option<i64> {
    // 与 tally_votes 同口径去重：opHash 字典序，同一 voter 取首票
    let mut dedup = ballots.to_vec();
    dedup.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
    let mut seen: HashSet<&str> = HashSet::new();
    let mut counted: Vec<&AnchoredBallot> = Vec::new();
    for ballot in &dedup {
        if seen.insert(ballot.voter.as_str()) {
            counted.push(ballot);
        }
    }
    counted.sort_by(|a, b| (a.anchored_ms, &a.op_hash).cmp(&(b.anchored_ms, &b.op_hash)));
    let mut yes = 0u64;
    let mut participated = 0u64;
    for ballot in counted {
        let Some(anchored) = ballot.anchored_ms else {
            continue; // 无锚票不参与生效时刻推导（§7.2）
        };
        yes += u64::from(ballot.yes);
        participated += 1;
        if threshold.reached(yes, roster_size) && quorum.reached(participated, roster_size) {
            return Some(anchored);
        }
    }
    None
}

/// 单条目对现行版本求值：生效 → Some((生效时刻, 新规则文档))；其余 → None。
/// 未锚定 / vote 名册缺席或为空 → None（fail-closed 恒待定）。
fn evaluate_entry(
    affair_id: &str,
    entry: &RuleChangeEntry,
    doc: &RulesDoc,
    now_ms: i64,
) -> Option<(i64, Value)> {
    let anchored_ms = entry.anchored_ms?;
    if matches!(entry.proposal.mechanism, Mechanism::Vote { .. })
        && entry.roster_size.is_none_or(|size| size == 0)
    {
        return None; // 名册缺席/空：恒待定（防空名册平凡通过）
    }
    let ballots: Vec<VoteBallot> = entry
        .ballots
        .iter()
        .map(|b| VoteBallot {
            op_hash: b.op_hash.clone(),
            voter: b.voter.clone(),
            yes: b.yes,
        })
        .collect();
    let ctx = RuleChangeCtx {
        affair_id,
        proposal_prev_op_hash: &entry.prev_op_hash,
        current_rules: &doc.raw,
        current_mechanism: &doc.rule_change,
        ballots: &ballots,
        roster_size: entry.roster_size.unwrap_or(0),
        objection_count: entry.objection_count,
        anchored_ms,
        now_ms,
    };
    let RuleChangeOutcome::Effective { new_rules } = evaluate_rule_change(&entry.proposal, &ctx)
    else {
        return None;
    };
    let effective_ms = match &entry.proposal.mechanism {
        Mechanism::DelayedVeto { delay_ms, .. } => anchored_ms + delay_ms,
        // approvals 随条目携带：多签分量齐即生效，生效时刻 = 提议锚定时刻
        Mechanism::Multisig { .. } => anchored_ms,
        Mechanism::Vote {
            threshold, quorum, ..
        } => vote_effective_ms(
            threshold,
            quorum,
            &entry.ballots,
            entry.roster_size.unwrap_or(0),
        )?,
    };
    Some((effective_ms, new_rules))
}

/// 残余条目归宿分类（不动点到达后对最终版本再求值一次，reason 稳定）。
fn classify_fate(affair_id: &str, entry: &RuleChangeEntry, doc: &RulesDoc, now_ms: i64) -> RuleChangeFate {
    let Some(anchored_ms) = entry.anchored_ms else {
        return RuleChangeFate::Pending("unanchored");
    };
    if matches!(entry.proposal.mechanism, Mechanism::Vote { .. })
        && entry.roster_size.is_none_or(|size| size == 0)
    {
        return RuleChangeFate::Pending("no-roster");
    }
    let ballots: Vec<VoteBallot> = entry
        .ballots
        .iter()
        .map(|b| VoteBallot {
            op_hash: b.op_hash.clone(),
            voter: b.voter.clone(),
            yes: b.yes,
        })
        .collect();
    let ctx = RuleChangeCtx {
        affair_id,
        proposal_prev_op_hash: &entry.prev_op_hash,
        current_rules: &doc.raw,
        current_mechanism: &doc.rule_change,
        ballots: &ballots,
        roster_size: entry.roster_size.unwrap_or(0),
        objection_count: entry.objection_count,
        anchored_ms,
        now_ms,
    };
    match evaluate_rule_change(&entry.proposal, &ctx) {
        RuleChangeOutcome::Rejected(reason) => RuleChangeFate::Rejected(reason),
        // Effective 但未被动点循环应用只会因生效时刻无从确定（vote 无锚票越过）
        RuleChangeOutcome::Effective { .. } => RuleChangeFate::Pending("effective-time-undetermined"),
        RuleChangeOutcome::Pending => RuleChangeFate::Pending("pending"),
    }
}

/// 规则链 replay：创世规则 + 已生效 rule-change 链（§5.4「每一版本确定性可溯」）。
/// 迭代应用：每轮在未应用条目中求值出现行版本下已生效者，取（生效时刻, opHash）
/// 最小者应用 patch 递进，直至不动点。创世规则静态检查不过 → Err（创世入站已
/// 把关，此处防御复跑，fail-closed）。
pub fn replay_rule_chain(
    affair_id: &str,
    genesis_rules: &Value,
    entries: &[RuleChangeEntry],
    now_ms: i64,
) -> Result<RuleChain, StaticCheckReject> {
    let mut doc = static_check_rules(genesis_rules)?;
    let mut versions = vec![RulesVersion {
        seq: 0,
        basis_op_hash: affair_id.to_string(),
        rules: genesis_rules.clone(),
        rules_hash: rules_hash(genesis_rules),
        effective_ms: None,
    }];
    let mut applied: HashSet<&str> = HashSet::new();
    loop {
        let mut best: Option<(i64, &str, Value)> = None;
        for entry in entries {
            if applied.contains(entry.op_hash.as_str()) {
                continue;
            }
            let Some((effective_ms, new_rules)) = evaluate_entry(affair_id, entry, &doc, now_ms)
            else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((best_ms, best_hash, _)) => {
                    (effective_ms, entry.op_hash.as_str()) < (*best_ms, *best_hash)
                }
            };
            if better {
                best = Some((effective_ms, entry.op_hash.as_str(), new_rules));
            }
        }
        let Some((effective_ms, op_hash, new_rules)) = best else {
            break;
        };
        // evaluate_rule_change 已过静态检查；此处重建 RulesDoc 视图（防御复跑）
        doc = static_check_rules(&new_rules)?;
        applied.insert(op_hash);
        let seq = versions.len() as u64;
        versions.push(RulesVersion {
            seq,
            basis_op_hash: op_hash.to_string(),
            rules_hash: rules_hash(&new_rules),
            rules: new_rules,
            effective_ms: Some(effective_ms),
        });
    }
    let mut fates: Vec<(String, RuleChangeFate)> = entries
        .iter()
        .filter(|entry| !applied.contains(entry.op_hash.as_str()))
        .map(|entry| {
            (
                entry.op_hash.clone(),
                classify_fate(affair_id, entry, &doc, now_ms),
            )
        })
        .collect();
    fates.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(RuleChain {
        versions,
        fates,
        doc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;
    use sha2::Digest as _;

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const T0: i64 = 1_720_000_000_000;

    fn genesis_rules() -> Value {
        json!({
            "engine": "b1",
            "closeConditions": [],
            "pubPeriod": { "delayMs": DAY },
            "ruleChange": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "exec": null
        })
    }

    fn entry(
        hash: &str,
        change: Value,
        anchored_ms: Option<i64>,
        objections: u64,
    ) -> RuleChangeEntry {
        RuleChangeEntry {
            op_hash: hash.repeat(64 / hash.len()),
            prev_op_hash: "ee".repeat(32),
            proposal: super::super::decide::parse_rule_change_payload(&json!({
                "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
                "change": change,
                "proposedAt": T0,
            }))
            .unwrap(),
            anchored_ms,
            objection_count: objections,
            ballots: Vec::new(),
            roster_size: None,
        }
    }

    #[test]
    fn empty_chain_is_genesis_only() {
        let chain = replay_rule_chain("dd".repeat(32).as_str(), &genesis_rules(), &[], T0).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(chain.current().seq, 0);
        assert_eq!(chain.doc.pub_period_ms, DAY);
        assert!(chain.fates.is_empty());
    }

    #[test]
    fn delayed_veto_change_applies_after_window() {
        let affair = "dd".repeat(32);
        let e = entry("aa", json!({ "pubPeriod": { "delayMs": 2 * DAY } }), Some(T0), 0);
        // 窗口未满 → 待定，现行版本仍创世
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[e.clone()], T0 + DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(
            chain.fates,
            vec![("aa".repeat(32), RuleChangeFate::Pending("pending"))]
        );
        // 窗口满 → 生效递进；生效时刻 = 锚定 + delayMs
        let chain =
            replay_rule_chain(&affair, &genesis_rules(), std::slice::from_ref(&e), T0 + 3 * DAY)
                .unwrap();
        assert_eq!(chain.versions.len(), 2);
        assert_eq!(chain.current().effective_ms, Some(T0 + 3 * DAY));
        assert_eq!(chain.current().rules["pubPeriod"]["delayMs"], json!(2 * DAY));
        assert_eq!(chain.doc.pub_period_ms, 2 * DAY);
        // 异议达阈值 → 否决入无效集
        let vetoed = entry("aa", json!({ "pubPeriod": { "delayMs": 2 * DAY } }), Some(T0), 1);
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[vetoed], T0 + 3 * DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(
            chain.fates,
            vec![(
                "aa".repeat(32),
                RuleChangeFate::Rejected("decision-rejected".to_string())
            )]
        );
    }

    #[test]
    fn unanchored_entry_stays_pending() {
        let affair = "dd".repeat(32);
        let e = entry("aa", json!({ "exec": null }), None, 0);
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[e], T0 + 100 * DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(
            chain.fates,
            vec![("aa".repeat(32), RuleChangeFate::Pending("unanchored"))]
        );
    }

    #[test]
    fn chain_applies_in_effective_time_order_and_reevaluates() {
        let affair = "dd".repeat(32);
        // 第二条修改把 ruleChange 机制换成更长公示期：须对「第一条生效后的版本」
        // 求值（机制一致性比对随版本演进）
        let e1 = entry(
            "aa",
            json!({ "pubPeriod": { "delayMs": 2 * DAY } }),
            Some(T0),
            0,
        );
        let e2 = entry(
            "bb",
            json!({ "ruleChange": { "kind": "delayed-veto", "delayMs": 5 * DAY, "vetoThreshold": { "count": 2 } } }),
            Some(T0 + DAY),
            0,
        );
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[e2.clone(), e1], T0 + 4 * DAY).unwrap();
        assert_eq!(chain.versions.len(), 3);
        // 应用序按生效时刻：e1（T0+3d）先于 e2（T0+4d）
        assert_eq!(chain.versions[1].basis_op_hash, "aa".repeat(32));
        assert_eq!(chain.versions[2].basis_op_hash, "bb".repeat(32));
        assert_eq!(chain.doc.pub_period_ms, 2 * DAY);
        // 机制不符（对创世版本之外的机制声明）→ 拒绝
        let bad = RuleChangeEntry {
            proposal: super::super::decide::parse_rule_change_payload(&json!({
                "mechanism": { "kind": "multisig", "m": 2, "n": 2, "signers": ["11".repeat(32), "22".repeat(32)] },
                "change": { "exec": null },
                "proposedAt": T0,
            }))
            .unwrap(),
            ..e2
        };
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[bad], T0 + 100 * DAY).unwrap();
        assert!(matches!(
            chain.fates[0].1,
            RuleChangeFate::Rejected(ref r) if r == "mechanism-mismatch"
        ));
    }

    #[test]
    fn multisig_effective_at_proposal_anchor() {
        use ed25519_dalek::Signer as _;
        let affair = "dd".repeat(32);
        let mut rules = json!({
            "engine": "b1", "closeConditions": [], "pubPeriod": { "delayMs": DAY },
            "ruleChange": { "kind": "multisig", "m": 2, "n": 2, "signers": [] },
            "exec": null
        });
        // 两名 signer 的固定密钥
        let key = |seed: u8| {
            let sk = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
            let pk = sk.verifying_key().to_bytes();
            (
                sk,
                base64::engine::general_purpose::STANDARD.encode(pk),
                hex::encode(sha2::Sha256::digest(pk)),
            )
        };
        let (sk_a, pk_a, id_a) = key(0x31);
        let (sk_b, pk_b, id_b) = key(0x32);
        rules["ruleChange"]["signers"] = json!([id_a, id_b]);
        let change = json!({ "pubPeriod": { "delayMs": 2 * DAY } });
        let mechanism_raw = rules["ruleChange"].clone();
        let mut proposal = super::super::decide::parse_rule_change_payload(&json!({
            "mechanism": mechanism_raw,
            "change": change,
            "approvals": [],
            "proposedAt": T0,
        }))
        .unwrap();
        let approval_payload = super::super::decide::rule_change_approval_payload(
            &affair,
            &proposal,
            &"ee".repeat(32),
        );
        let sign = |sk: &ed25519_dalek::SigningKey| {
            base64::engine::general_purpose::STANDARD.encode(sk.sign(approval_payload.as_bytes()).to_bytes())
        };
        proposal.approvals = vec![
            json!({ "identity": id_a, "publicKey": pk_a, "sig": sign(&sk_a) }),
            json!({ "identity": id_b, "publicKey": pk_b, "sig": sign(&sk_b) }),
        ];
        let e = RuleChangeEntry {
            op_hash: "aa".repeat(32),
            prev_op_hash: "ee".repeat(32),
            proposal,
            anchored_ms: Some(T0),
            objection_count: 0,
            ballots: Vec::new(),
            roster_size: None,
        };
        let chain = replay_rule_chain(&affair, &rules, &[e], T0).unwrap();
        assert_eq!(chain.versions.len(), 2);
        assert_eq!(chain.current().effective_ms, Some(T0));
    }

    #[test]
    fn vote_requires_roster_and_derives_effective_time() {
        let affair = "dd".repeat(32);
        let rules = json!({
            "engine": "b1", "closeConditions": [], "pubPeriod": { "delayMs": DAY },
            "ruleChange": { "kind": "vote", "voterSet": "ladder:voters",
                "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                "snapshot": "required" },
            "exec": null
        });
        let mechanism_raw = rules["ruleChange"].clone();
        let proposal = super::super::decide::parse_rule_change_payload(&json!({
            "mechanism": mechanism_raw, "change": { "exec": null }, "proposedAt": T0,
        }))
        .unwrap();
        let ballot = |hash: &str, voter: &str, anchored: Option<i64>| AnchoredBallot {
            op_hash: hash.repeat(64 / hash.len()),
            voter: voter.repeat(64 / voter.len()),
            yes: true,
            anchored_ms: anchored,
        };
        let make = |ballots: Vec<AnchoredBallot>, roster_size: Option<u64>| RuleChangeEntry {
            op_hash: "aa".repeat(32),
            prev_op_hash: "ee".repeat(32),
            proposal: proposal.clone(),
            anchored_ms: Some(T0),
            objection_count: 0,
            ballots,
            roster_size,
        };
        // 名册缺席 → 恒待定（fail-closed）
        let e = make(vec![ballot("b1", "aa", Some(T0)), ballot("b2", "bb", Some(T0))], None);
        let chain = replay_rule_chain(&affair, &rules, &[e], T0 + DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(
            chain.fates,
            vec![("aa".repeat(32), RuleChangeFate::Pending("no-roster"))]
        );
        // 名册 3 人，1 票 < 1/2 → 待定
        let e = make(vec![ballot("b1", "aa", Some(T0))], Some(3));
        let chain = replay_rule_chain(&affair, &rules, &[e], T0 + DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        // 2/3 yes ≥ 1/2 且参与 2/3 ≥ 1/2 → 生效，生效时刻 = 越过阈值那票的锚定
        let e = make(
            vec![
                ballot("b1", "aa", Some(T0 + 10)),
                ballot("b2", "bb", Some(T0 + 20)),
            ],
            Some(3),
        );
        let chain = replay_rule_chain(&affair, &rules, &[e], T0 + DAY).unwrap();
        assert_eq!(chain.versions.len(), 2);
        assert_eq!(chain.current().effective_ms, Some(T0 + 20));
        // 越过阈值的票无锚定 → 生效时刻无从确定 → 待定
        let e = make(
            vec![ballot("b1", "aa", Some(T0 + 10)), ballot("b2", "bb", None)],
            Some(3),
        );
        let chain = replay_rule_chain(&affair, &rules, &[e], T0 + DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert_eq!(
            chain.fates,
            vec![(
                "aa".repeat(32),
                RuleChangeFate::Pending("effective-time-undetermined")
            )]
        );
    }

    #[test]
    fn static_check_failure_rejected() {
        let affair = "dd".repeat(32);
        // patch 应用后违反公示期下限（§5.6）→ 拒绝，不得生效
        let e = entry("aa", json!({ "pubPeriod": { "delayMs": 1 } }), Some(T0), 0);
        let chain = replay_rule_chain(&affair, &genesis_rules(), &[e], T0 + 100 * DAY).unwrap();
        assert_eq!(chain.versions.len(), 1);
        assert!(matches!(
            &chain.fates[0].1,
            RuleChangeFate::Rejected(r) if r.starts_with("rules-rejected:")
        ));
    }

    #[test]
    fn replay_is_order_independent() {
        let affair = "dd".repeat(32);
        let e1 = entry("aa", json!({ "pubPeriod": { "delayMs": 2 * DAY } }), Some(T0), 0);
        let e2 = entry("bb", json!({ "pubPeriod": { "delayMs": 3 * DAY } }), Some(T0 + DAY), 0);
        let a = replay_rule_chain(&affair, &genesis_rules(), &[e1.clone(), e2.clone()], T0 + 10 * DAY).unwrap();
        let b = replay_rule_chain(&affair, &genesis_rules(), &[e2, e1], T0 + 10 * DAY).unwrap();
        assert_eq!(a.versions, b.versions);
        assert_eq!(a.fates, b.fates);
    }
}
