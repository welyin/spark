//! 公开履历聚合：公共身份（opt-in 稳定公开账号，affair §2.2）跨事务的
//! 账号年龄 / 采纳 / 投票历史聚合视图（wiki/architecture/community-affairs.md
//! §7.3「履历 = indexer 对公共事务日志的聚合视图，算法在索引层，非新数据类型」、
//! §10 决策 4「聚合函数内核确定性化：同一查询任何节点返回同一结果、可全网复算」）。
//!
//! 与 affair 模块同口径：纯函数、数据源注入（各事务操作视图由调用方装配，
//! 时间一律 `now_ms` / `anchored_ms` 参数注入，只认链上锚定时刻），不碰网络
//! 与存储。聚合只取交换/结合律运算（min / 计数 / 求和），输入按 affairId 归并、
//! 操作按 opHash 去重（内容寻址，重复条目幂等），输出集合一律确定性排序——
//! 与到达顺序、重放次数无关（乱序安全 / 重放等价）。
//!
//! 口径（逐项沿用既有单事务推导，不新造语义）：
//!
//! - 账号年龄 = 该身份跨事务最早链上活跃时刻（person 署名操作的存证锚定时刻
//!   最早者，同 ladder.rs `earliest_activity_ms` 口径）至求值时刻；未锚定
//!   操作不参与一切时间推导；
//! - 提议数 = 该身份作为 person 操作者的 meta-revise / rule-change 操作计数
//!   （全机制）；采纳数 = 其中经 delayed-veto 生效者（复用 ladder.rs
//!   `derive_adoptions`，§13 最小口径：vote/multisig 生效不计采纳），
//!   采纳率 = 采纳数 / 提议数（只输出精确计数对，比率呈现归客户端）；
//! - 投票历史 = 该身份的内核级表决票（vote 操作，§4：仅服务 rule-change /
//!   meta-revise 的 vote 变体；插件业务投票走 content、不进本视图），
//!   逐票记录 + yes/no 合计；
//! - 组织表态（actor kind = org）不构成个人履历，全程跳过（产品第六节）。
//!
//! 防老号交易边界（产品第六节）：本视图只是公开身份的公开计数聚合，不产出
//! 任何全局分值/权重；阶梯票权仍严格单事务推导（ladder.rs），本模块不消费
//! 也不反馈。

use std::collections::BTreeMap;

use super::actor::ActorKind;
use super::ladder::{LadderOpView, derive_adoptions};
use super::op::OpType;

/// 聚合输入：一个事务的已接受操作视图（已验签入有效集；锚定时刻与异议计数
/// 已由调用方注入 `LadderOpView`）。契约：同一事务至多一条视图；重复视图按
/// affairId 归并、操作按 opHash 去重（重放幂等）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileAffairView {
    /// 事务 id（64 hex）。
    pub affair_id: String,
    /// 该事务的有效操作集合（可含任意操作者的操作；本模块自行过滤）。
    pub ops: Vec<LadderOpView>,
}

/// 投票历史条目：一张内核级表决票（§4 vote 操作）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileVote {
    /// 投票所在事务。
    pub affair_id: String,
    /// 投票操作 opHash。
    pub op_hash: String,
    /// 表决目标（rule-change / meta-revise 提议 opHash）。
    pub proposal: String,
    /// 选择：true = yes，false = no。
    pub yes: bool,
    /// 本副本存证链锚定时刻；未锚定为 None（不计账龄但仍计票——计票口径
    /// 不依赖时间，与 §5.3 逐票验签计数一致）。
    pub anchored_ms: Option<i64>,
}

/// 单事务履历分量（聚合的中间层，透明呈现用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffairProfileStats {
    /// 事务 id。
    pub affair_id: String,
    /// 该身份在本事务的 person 操作数（全类型）。
    pub op_count: u64,
    /// 提议数（meta-revise + rule-change，全机制）。
    pub proposals: u64,
    /// 采纳数（§13 口径：delayed-veto 生效者）。
    pub adoptions: u64,
    /// 内核级表决票总数 / 其中 yes / 其中 no。
    pub votes: u64,
    /// 赞成票数。
    pub votes_yes: u64,
    /// 反对票数。
    pub votes_no: u64,
    /// 本事务内最早 / 最近链上活跃时刻（锚定时刻；无 = None）。
    pub first_activity_ms: Option<i64>,
    /// 本事务内最近链上活跃时刻。
    pub last_activity_ms: Option<i64>,
}

/// 公开履历聚合产物（确定性：同一输入集合任何节点复算一致）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicProfile {
    /// 公共身份 id。
    pub identity: String,
    /// 求值时刻（注入）。
    pub now_ms: i64,
    /// 参与的事务数（≥1 条 person 操作的事务）。
    pub affairs_participated: u64,
    /// 跨事务最早链上活跃时刻；无任何已锚定活动 = None。
    pub first_activity_ms: Option<i64>,
    /// 账号年龄（毫秒）= now_ms − first_activity_ms；无链上活动 = None。
    pub account_age_ms: Option<i64>,
    /// 跨事务提议总数（meta-revise + rule-change，全机制）。
    pub proposals: u64,
    /// 跨事务采纳总数（§13 口径）。采纳率 = adoptions / proposals
    /// （精确计数对，比率呈现归客户端，本层不产浮点）。
    pub adoptions: u64,
    /// 跨事务内核级表决票总数 / yes / no。
    pub votes: u64,
    /// 赞成票总数。
    pub votes_yes: u64,
    /// 反对票总数。
    pub votes_no: u64,
    /// 单事务分量，按 affairId 字典序（确定性）。
    pub per_affair: Vec<AffairProfileStats>,
    /// 逐票历史，按（affairId, 锚定时刻（未锚定排后）, opHash）排序。
    pub vote_history: Vec<ProfileVote>,
}

/// 乱序安全的操作归并：同一 opHash 重复出现时按交换律规则合并
/// （锚定时刻取已锚定者、两者皆锚定取小者；异议计数取大者）——重复视图 /
/// 乱序补齐下输出与归并顺序无关。
fn merge_op_view(slot: &mut LadderOpView, incoming: &LadderOpView) {
    slot.anchored_ms = match (slot.anchored_ms, incoming.anchored_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    slot.objection_count = slot.objection_count.max(incoming.objection_count);
}

/// 公开履历聚合主函数：跨事务确定性聚合（语义见模块头注）。
pub fn derive_public_profile(
    identity: &str,
    affairs: &[ProfileAffairView],
    now_ms: i64,
) -> PublicProfile {
    // 按 affairId 归并（BTreeMap 迭代即字典序）；操作按 opHash 去重归并
    let mut by_affair: BTreeMap<&str, BTreeMap<&str, LadderOpView>> = BTreeMap::new();
    for view in affairs {
        let ops = by_affair.entry(view.affair_id.as_str()).or_default();
        for op in &view.ops {
            match ops.get_mut(op.op_hash.as_str()) {
                Some(slot) => merge_op_view(slot, op),
                None => {
                    ops.insert(op.op_hash.as_str(), op.clone());
                }
            }
        }
    }

    let mut per_affair = Vec::new();
    let mut vote_history = Vec::new();
    for (affair_id, ops_map) in &by_affair {
        let all_ops: Vec<LadderOpView> = ops_map.values().cloned().collect();
        let mine: Vec<&LadderOpView> = all_ops
            .iter()
            .filter(|op| op.actor_kind == ActorKind::Person && op.actor_identity == identity)
            .collect();
        if mine.is_empty() {
            continue; // 未参与本事务
        }
        let proposals = mine
            .iter()
            .filter(|op| matches!(op.op_type, OpType::MetaRevise | OpType::RuleChange))
            .count() as u64;
        // 采纳复用 §13 口径（含 delayed-veto 窗口/异议求值），只计本人提议
        let adoptions = derive_adoptions(&all_ops, now_ms)
            .iter()
            .filter(|a| a.actor_identity == identity)
            .count() as u64;
        let anchored: Vec<i64> = mine.iter().filter_map(|op| op.anchored_ms).collect();
        let mut stats = AffairProfileStats {
            affair_id: affair_id.to_string(),
            op_count: mine.len() as u64,
            proposals,
            adoptions,
            votes: 0,
            votes_yes: 0,
            votes_no: 0,
            first_activity_ms: anchored.iter().copied().min(),
            last_activity_ms: anchored.iter().copied().max(),
        };
        for op in &mine {
            if op.op_type != OpType::Vote {
                continue;
            }
            // 入站已按 §4 校验 vote payload；此处防御性解析，畸形跳过
            let Some(proposal) = op
                .payload
                .get("proposal")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let yes = match op.payload.get("choice").and_then(serde_json::Value::as_str) {
                Some("yes") => true,
                Some("no") => false,
                _ => continue,
            };
            stats.votes += 1;
            if yes {
                stats.votes_yes += 1;
            } else {
                stats.votes_no += 1;
            }
            vote_history.push(ProfileVote {
                affair_id: affair_id.to_string(),
                op_hash: op.op_hash.clone(),
                proposal: proposal.to_string(),
                yes,
                anchored_ms: op.anchored_ms,
            });
        }
        per_affair.push(stats);
    }
    vote_history.sort_by(|a, b| {
        (&a.affair_id, a.anchored_ms.unwrap_or(i64::MAX), &a.op_hash)
            .cmp(&(&b.affair_id, b.anchored_ms.unwrap_or(i64::MAX), &b.op_hash))
    });

    let first_activity_ms = per_affair
        .iter()
        .filter_map(|s| s.first_activity_ms)
        .min();
    PublicProfile {
        identity: identity.to_string(),
        now_ms,
        affairs_participated: per_affair.len() as u64,
        first_activity_ms,
        account_age_ms: first_activity_ms.map(|first| now_ms - first),
        proposals: per_affair.iter().map(|s| s.proposals).sum(),
        adoptions: per_affair.iter().map(|s| s.adoptions).sum(),
        votes: per_affair.iter().map(|s| s.votes).sum(),
        votes_yes: per_affair.iter().map(|s| s.votes_yes).sum(),
        votes_no: per_affair.iter().map(|s| s.votes_no).sum(),
        per_affair,
        vote_history,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const NOW: i64 = 1_720_000_000_000;

    fn id(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn op(
        hash_seed: &str,
        identity: &str,
        op_type: OpType,
        payload: Value,
        anchored_ms: Option<i64>,
        objections: u64,
    ) -> LadderOpView {
        LadderOpView {
            op_hash: format!("{:0>64}", hash_seed),
            actor_kind: ActorKind::Person,
            actor_identity: identity.to_string(),
            op_type,
            payload,
            anchored_ms,
            objection_count: objections,
        }
    }

    fn delayed_veto_payload(delay_ms: i64) -> Value {
        json!({
            "title": "修订",
            "mechanism": { "kind": "delayed-veto", "delayMs": delay_ms, "vetoThreshold": { "count": 1 } }
        })
    }

    fn vote_payload(proposal: &str, choice: &str) -> Value {
        json!({ "proposal": proposal, "choice": choice })
    }

    /// 两个事务的夹具：alice 在 A 有老 content + 两提议（一采纳一被否决）+
    /// 一票；在 B 有一提议（采纳）+ 两票；bob 在 B 有 content（不混入）。
    fn fixture() -> (String, Vec<ProfileAffairView>) {
        let alice = id('a');
        let bob = id('b');
        let affair_a = id('1');
        let affair_b = id('2');
        let views = vec![
            ProfileAffairView {
                affair_id: affair_a,
                ops: vec![
                    op("a1", &alice, OpType::Content, json!({}), Some(NOW - 50 * DAY), 0),
                    op("a2", &alice, OpType::MetaRevise, delayed_veto_payload(DAY), Some(NOW - 40 * DAY), 0),
                    op("a3", &alice, OpType::RuleChange, delayed_veto_payload(DAY), Some(NOW - 30 * DAY), 1),
                    op("a4", &alice, OpType::Vote, vote_payload(&id('9'), "yes"), Some(NOW - 20 * DAY), 0),
                ],
            },
            ProfileAffairView {
                affair_id: affair_b,
                ops: vec![
                    op("b1", &alice, OpType::MetaRevise, delayed_veto_payload(DAY), Some(NOW - 10 * DAY), 0),
                    op("b2", &alice, OpType::Vote, vote_payload(&id('8'), "no"), Some(NOW - 5 * DAY), 0),
                    op("b3", &alice, OpType::Vote, vote_payload(&id('8'), "yes"), None, 0),
                    op("b4", &bob, OpType::Content, json!({}), Some(NOW - 60 * DAY), 0),
                ],
            },
        ];
        (alice, views)
    }

    #[test]
    fn aggregate_counts_and_age() {
        let (alice, views) = fixture();
        let profile = derive_public_profile(&alice, &views, NOW);
        assert_eq!(profile.affairs_participated, 2);
        // 账号年龄跨事务取最早：A 的 content（50 天前）；bob 更早但不混入
        assert_eq!(profile.first_activity_ms, Some(NOW - 50 * DAY));
        assert_eq!(profile.account_age_ms, Some(50 * DAY));
        // 提议 3（a2 a3 b1）；采纳 2（a2 b1；a3 异议达阈值否决）
        assert_eq!(profile.proposals, 3);
        assert_eq!(profile.adoptions, 2);
        // 投票 3 票：yes 2 / no 1（含未锚定票）
        assert_eq!(profile.votes, 3);
        assert_eq!(profile.votes_yes, 2);
        assert_eq!(profile.votes_no, 1);
        assert_eq!(profile.vote_history.len(), 3);
        // per_affair 按 affairId 字典序
        assert_eq!(profile.per_affair[0].affair_id, id('1'));
        assert_eq!(profile.per_affair[1].affair_id, id('2'));
        assert_eq!(profile.per_affair[0].proposals, 2);
        assert_eq!(profile.per_affair[0].adoptions, 1);
        assert_eq!(profile.per_affair[1].votes, 2);
        // 未锚定票排在事务内已锚定票之后
        let b_votes: Vec<&ProfileVote> = profile
            .vote_history
            .iter()
            .filter(|v| v.affair_id == id('2'))
            .collect();
        assert_eq!(b_votes[0].anchored_ms, Some(NOW - 5 * DAY));
        assert_eq!(b_votes[1].anchored_ms, None);
    }

    #[test]
    fn empty_and_unknown_identity() {
        let (_alice, views) = fixture();
        let nobody = derive_public_profile(&id('c'), &views, NOW);
        assert_eq!(nobody.affairs_participated, 0);
        assert_eq!(nobody.account_age_ms, None);
        assert_eq!(nobody.proposals, 0);
        assert_eq!(nobody.votes, 0);
        assert!(derive_public_profile(&id('a'), &[], NOW)
            .per_affair
            .is_empty());
    }

    #[test]
    fn org_ops_never_counted() {
        let alice = id('a');
        let mut org_op = op("o1", &alice, OpType::Content, json!({}), Some(NOW - DAY), 0);
        org_op.actor_kind = ActorKind::Org;
        let views = vec![ProfileAffairView {
            affair_id: id('1'),
            ops: vec![org_op],
        }];
        let profile = derive_public_profile(&alice, &views, NOW);
        assert_eq!(profile.affairs_participated, 0);
    }

    #[test]
    fn vote_mechanism_proposals_not_adopted() {
        let alice = id('a');
        // vote/multisig 形态提议计入提议数但不计采纳（§13 最小口径）
        let vote_proposal = op(
            "v1",
            &alice,
            OpType::RuleChange,
            json!({ "mechanism": { "kind": "vote", "voterSet": "ladder:voters",
                "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                "snapshot": "required" }, "change": {}, "proposedAt": 1 }),
            Some(NOW - 10 * DAY),
            0,
        );
        let views = vec![ProfileAffairView {
            affair_id: id('1'),
            ops: vec![vote_proposal],
        }];
        let profile = derive_public_profile(&alice, &views, NOW);
        assert_eq!(profile.proposals, 1);
        assert_eq!(profile.adoptions, 0);
    }

    #[test]
    fn unordered_and_replay_equivalent() {
        let (alice, views) = fixture();
        let baseline = derive_public_profile(&alice, &views, NOW);

        // 事务顺序颠倒 + 事务内操作顺序颠倒 → 逐字段等价
        let mut shuffled: Vec<ProfileAffairView> = views.iter().rev().cloned().collect();
        for view in &mut shuffled {
            view.ops.reverse();
        }
        assert_eq!(derive_public_profile(&alice, &shuffled, NOW), baseline);

        // 重放：整个输入再来一遍（视图重复 + 操作重复）→ 幂等
        let mut replayed = shuffled.clone();
        replayed.extend(views.iter().cloned());
        assert_eq!(derive_public_profile(&alice, &replayed, NOW), baseline);

        // 同视图内同一操作出现两次 → 去重幂等
        let mut dup = views.clone();
        let extra = dup[0].ops[0].clone();
        dup[0].ops.push(extra);
        assert_eq!(derive_public_profile(&alice, &dup, NOW), baseline);
    }

    #[test]
    fn conflicting_duplicate_merge_is_order_safe() {
        let alice = id('a');
        // 同一 opHash 的冲突副本（一锚定一未锚定）：交换律归并 → 两种顺序等价
        let anchored = op("m1", &alice, OpType::Content, json!({}), Some(NOW - DAY), 0);
        let mut unanchored = anchored.clone();
        unanchored.anchored_ms = None;
        let mk = |first: LadderOpView, second: LadderOpView| {
            vec![ProfileAffairView {
                affair_id: id('1'),
                ops: vec![first, second],
            }]
        };
        let p1 = derive_public_profile(&alice, &mk(anchored.clone(), unanchored.clone()), NOW);
        let p2 = derive_public_profile(&alice, &mk(unanchored, anchored), NOW);
        assert_eq!(p1, p2);
        assert_eq!(p1.account_age_ms, Some(DAY));
    }
}
