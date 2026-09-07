//! 账龄原语与参与阶梯推导（wiki/protocol/community/affair.md §5.5/§9/§13；
//! 产品 community-model 第六节「参与阶梯详设」；总体方案 §7.1 账龄/阶梯基础件）。
//!
//! 账龄 = 身份在**本副本存证链**上的最早活跃时刻：只认操作条目的存证锚定
//! 时间（§7.2 时间源，调用方从已校验的本地链注入 `anchored_ms`），不认
//! declaredAt 声明时间——插件伪造不了链上时间。未锚定的操作不参与一切
//! 时间推导（无链上时间可推导）。
//!
//! 阶梯（observer / contributor / voter）从操作集合**确定性**推导，与到达
//! 顺序、链拓扑无关：
//!
//! - 采纳 = 经「延迟生效 + 阈值否决」生效的 meta-revise / rule-change 操作
//!   （产品：采纳是集体决策，默认形态即 delayed-veto；vote/multisig 形态的
//!   规则修改同样产生效力但**不计采纳**——采纳口径取任务/产品定义的最小集，
//!   §13）；采纳记入提议者（actor），生效时刻 = 锚定时刻 + delayMs；
//! - 贡献者：累计采纳 ≥ `contributorAccepts`（缺省 1），在级起点 = 第
//!   `contributorAccepts` 次采纳的生效时刻；
//! - 投票者：贡献者 且 在级满 `voterDays`（缺省 30 天）且 累计采纳 ≥
//!   `voterAccepts`（缺省 3）且 近 `activeWindowMs`（缺省 90 天）内有链上活动；
//! - 在级衰减：投票者近 `decayMs`（缺省 90 天）无链上活动 → 降回贡献者
//!   （采纳与在级起点保留累计，恢复活动并重新满足条件即回级）；
//! - 初始投票者（创世 `initialVoters`，冷启动）自创世锚定时刻起为投票者，
//!   衰减规则同等适用（以其链上最后活动衡量；无任何链上活动则无衰减时刻
//!   可判，保留投票者——§13 口径）；
//! - 组织表态（actor kind = org）不爬阶梯、无票权（产品第六节），推导跳过；
//! - 作用域 = 本事务日志（治理上下文）：推导只在单个 affair 的操作集合上
//!   进行，不产出任何全局/跨事务分值——防老号交易（产品第六节）。

use serde_json::Value;

use super::actor::ActorKind;
use super::decide::{Decision, evaluate_delayed_veto};
use super::op::{OpType, sort_op_hashes};

/// 缺省阶梯参数（affair §5.5：1 次采纳 / 在级 30 天 + 累计 3 次采纳 +
/// 近 90 天活跃 / 90 天衰减，均为产品默认值）。
pub const DEFAULT_CONTRIBUTOR_ACCEPTS: u64 = 1;
/// 缺省投票者在级天数（30 天，毫秒）。
pub const DEFAULT_VOTER_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// 缺省投票者累计采纳次数（3 次）。
pub const DEFAULT_VOTER_ACCEPTS: u64 = 3;
/// 缺省活跃窗口（90 天，毫秒）。
pub const DEFAULT_ACTIVE_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// 缺省衰减窗口（90 天，毫秒）。
pub const DEFAULT_DECAY_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// 阶梯参数（affair §5.5 `participation.ladderParams`；缺省即产品默认值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LadderParams {
    /// 贡献者所需采纳次数（≥1）。
    pub contributor_accepts: u64,
    /// 投票者在级时长（毫秒，>0）。
    pub voter_days_ms: i64,
    /// 投票者所需累计采纳次数（≥1）。
    pub voter_accepts: u64,
    /// 活跃窗口（毫秒，>0）：近窗口内须有链上活动。
    pub active_window_ms: i64,
    /// 衰减窗口（毫秒，>0）：投票者无活动超过窗口即降回贡献者。
    pub decay_ms: i64,
}

impl Default for LadderParams {
    fn default() -> Self {
        Self {
            contributor_accepts: DEFAULT_CONTRIBUTOR_ACCEPTS,
            voter_days_ms: DEFAULT_VOTER_DAYS_MS,
            voter_accepts: DEFAULT_VOTER_ACCEPTS,
            active_window_ms: DEFAULT_ACTIVE_WINDOW_MS,
            decay_ms: DEFAULT_DECAY_MS,
        }
    }
}

impl LadderParams {
    /// 从 `participation` 声明解析 `ladderParams`（§5.5：逐字段覆盖缺省）。
    /// 形状非法（非对象、数值越界）→ 拒绝（fail-closed，同静态检查口径）。
    pub fn from_participation(participation: &Value) -> Result<Self, &'static str> {
        let mut params = Self::default();
        let Some(raw) = participation.get("ladderParams") else {
            return Ok(params);
        };
        let obj = raw.as_object().ok_or("bad-ladder-params")?;
        let positive_i64 = |v: Option<&Value>| -> Result<i64, &'static str> {
            match v {
                None => Ok(0),
                Some(v) => {
                    let n = v.as_i64().ok_or("bad-ladder-params")?;
                    if n <= 0 {
                        return Err("bad-ladder-params");
                    }
                    Ok(n)
                }
            }
        };
        if let Some(n) = obj.get("contributorAccepts") {
            let n = n.as_u64().ok_or("bad-ladder-params")?;
            if n < 1 {
                return Err("bad-ladder-params");
            }
            params.contributor_accepts = n;
        }
        let voter_days = positive_i64(obj.get("voterDays"))?;
        if voter_days > 0 {
            // 线形单位是天（affair §5.5 示例 `voterDays: 30`），内部统一毫秒；
            // 溢出 fail-closed，同静态检查口径。
            params.voter_days_ms = voter_days
                .checked_mul(24 * 60 * 60 * 1000)
                .ok_or("bad-ladder-params")?;
        }
        if let Some(n) = obj.get("voterAccepts") {
            let n = n.as_u64().ok_or("bad-ladder-params")?;
            if n < 1 {
                return Err("bad-ladder-params");
            }
            params.voter_accepts = n;
        }
        let active_window = positive_i64(obj.get("activeWindowMs"))?;
        if active_window > 0 {
            params.active_window_ms = active_window;
        }
        let decay = positive_i64(obj.get("decayMs"))?;
        if decay > 0 {
            params.decay_ms = decay;
        }
        Ok(params)
    }
}

/// 阶梯推导输入的操作视图（已验签入有效集；时间与异议计数由调用方注入）。
#[derive(Clone, Debug, PartialEq)]
pub struct LadderOpView {
    /// 操作 opHash（§8 排序键）。
    pub op_hash: String,
    /// 操作者类别（org 表态不爬阶梯）。
    pub actor_kind: ActorKind,
    /// 操作者身份。
    pub actor_identity: String,
    /// 操作类型。
    pub op_type: OpType,
    /// 载荷原文（rule-change / meta-revise 的 mechanism 从其中解析）。
    pub payload: Value,
    /// 本副本存证链锚定时刻（§7.2）；None = 未锚定，不参与时间推导。
    pub anchored_ms: Option<i64>,
    /// 指向该操作的有效异议数（delayed-veto 求值输入，调用方注入）。
    pub objection_count: u64,
}

/// 身份的最早链上活跃时刻：其署名操作中存证锚定时刻最早者。
/// 只统计 person 操作者（org 表态不构成个人账龄）。
pub fn earliest_activity_ms(identity: &str, ops: &[LadderOpView]) -> Option<i64> {
    ops.iter()
        .filter(|op| op.actor_kind == ActorKind::Person && op.actor_identity == identity)
        .filter_map(|op| op.anchored_ms)
        .min()
}

/// 账龄（毫秒）= 求值时刻 − 最早链上活跃时刻；无链上活动 → None。
pub fn account_age_ms(identity: &str, ops: &[LadderOpView], now_ms: i64) -> Option<i64> {
    earliest_activity_ms(identity, ops).map(|first| now_ms - first)
}

/// 采纳事件：一条 delayed-veto 生效的 meta-revise / rule-change 操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptionEvent {
    /// 被采纳操作的 opHash。
    pub op_hash: String,
    /// 被采纳者（提议者 actor）。
    pub actor_identity: String,
    /// 采纳生效时刻（锚定时刻 + delayMs，链上推导时间）。
    pub effective_ms: i64,
}

/// 从操作集合推导采纳事件：meta-revise / rule-change 操作，声明机制为
/// delayed-veto 且（锚定 + delayMs 已过 且 有效异议 < 否决阈值）→ 采纳。
/// 结果按（生效时刻, opHash）字典序排序——确定性，与到达顺序无关。
pub fn derive_adoptions(ops: &[LadderOpView], now_ms: i64) -> Vec<AdoptionEvent> {
    let mut events: Vec<AdoptionEvent> = Vec::new();
    for op in ops {
        if op.actor_kind != ActorKind::Person {
            continue;
        }
        if !matches!(op.op_type, OpType::MetaRevise | OpType::RuleChange) {
            continue;
        }
        let (delay_ms, veto_count) = match delayed_veto_mechanism(&op.payload, op.op_type) {
            Some(v) => v,
            None => continue,
        };
        let Some(anchored_ms) = op.anchored_ms else {
            continue; // 无链上锚定时间：delayed-veto 窗口无从求值
        };
        if evaluate_delayed_veto(
            delay_ms,
            veto_count,
            op.objection_count,
            anchored_ms,
            now_ms,
        ) != Decision::Effective
        {
            continue;
        }
        events.push(AdoptionEvent {
            op_hash: op.op_hash.clone(),
            actor_identity: op.actor_identity.clone(),
            effective_ms: anchored_ms + delay_ms,
        });
    }
    events.sort_by(|a, b| {
        a.effective_ms
            .cmp(&b.effective_ms)
            .then_with(|| a.op_hash.cmp(&b.op_hash))
    });
    events
}

/// 从载荷解析 delayed-veto 机制参数（meta-revise / rule-change 两形态）。
/// 机制非 delayed-veto（vote/multisig 形态、或解析失败）→ None（不计采纳）。
fn delayed_veto_mechanism(payload: &Value, op_type: OpType) -> Option<(i64, u64)> {
    let mechanism = payload.get("mechanism")?;
    let obj = mechanism.as_object()?;
    if obj.get("kind").and_then(Value::as_str) != Some("delayed-veto") {
        return None;
    }
    let delay_ms = obj.get("delayMs").and_then(Value::as_i64)?;
    let veto_count = obj
        .get("vetoThreshold")
        .and_then(|v| v.get("count"))
        .and_then(Value::as_u64)?;
    if delay_ms <= 0 || veto_count < 1 {
        return None;
    }
    let _ = op_type; // 两形态结构一致，参数口径相同（§5.4 / §11.2）
    Some((delay_ms, veto_count))
}

/// 阶梯级别（§5.5 取值；observer 为缺省）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderTier {
    /// 观察者：查看/评论，零门槛。
    Observer,
    /// 贡献者：累计采纳达 contributorAccepts。
    Contributor,
    /// 投票者：贡献者满在级天数 + 累计采纳 + 近窗口活跃（未衰减）。
    Voter,
}

impl LadderTier {
    /// 线形字符串（§5.5 取值）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Contributor => "contributor",
            Self::Voter => "voter",
        }
    }
}

/// 阶梯名册条目（一人一票，不加权）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LadderEntry {
    /// 身份 id。
    pub identity: String,
    /// 当前级别。
    pub tier: LadderTier,
    /// 累计采纳次数（生效）。
    pub accepts: u64,
    /// 当前级别在级起点（链上时间）：observer = None。
    pub tier_since_ms: Option<i64>,
    /// 链上最后活跃时刻（无 = None）。
    pub last_activity_ms: Option<i64>,
}

/// 阶梯名册（推导产物；投票者集合经 [`LadderRoster::voter_identities`] 取出，
/// 喂 §9 快照 rosterHash）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LadderRoster {
    /// 名册条目，按 identity 字典序（确定性）。
    pub entries: Vec<LadderEntry>,
}

impl LadderRoster {
    /// 投票者身份集合（§9 名册口径的输入），按 identity 字典序。
    pub fn voter_identities(&self) -> Vec<String> {
        let mut voters: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.tier == LadderTier::Voter)
            .map(|e| e.identity.clone())
            .collect();
        voters.sort();
        voters
    }
}

/// 阶梯推导输入。
#[derive(Clone, Copy)]
pub struct LadderInput<'a> {
    /// 创世初始投票者（§2.1 `initialVoters`，冷启动）。
    pub initial_voters: &'a [String],
    /// 创世记录的本副本存证链锚定时刻（初始投票者在级起点；可未锚定）。
    pub genesis_anchored_ms: Option<i64>,
    /// 有效操作集合（已验签；时间与异议计数已注入）。
    pub ops: &'a [LadderOpView],
    /// 阶梯参数（§5.5，调用方从规则文档版本解析）。
    pub params: LadderParams,
    /// 求值时刻（毫秒，调用方注入）。
    pub now_ms: i64,
}

/// 从操作集合确定性推导阶梯名册（语义见模块头注；§13）。
pub fn derive_ladder(input: &LadderInput) -> LadderRoster {
    let params = input.params;
    let adoptions = derive_adoptions(input.ops, input.now_ms);
    let mut identities: Vec<String> = input.initial_voters.to_vec();
    for op in input.ops {
        if op.actor_kind == ActorKind::Person && !identities.contains(&op.actor_identity) {
            identities.push(op.actor_identity.clone());
        }
    }
    identities = sort_op_hashes(&identities);

    let mut entries = Vec::with_capacity(identities.len());
    for identity in identities {
        let accepts: Vec<i64> = adoptions
            .iter()
            .filter(|a| a.actor_identity == identity)
            .map(|a| a.effective_ms)
            .collect();
        let last_activity_ms = input
            .ops
            .iter()
            .filter(|op| op.actor_kind == ActorKind::Person && op.actor_identity == identity)
            .filter_map(|op| op.anchored_ms)
            .max();
        let entry = if accepts.len() as u64 >= params.contributor_accepts {
            let contributor_since = accepts[(params.contributor_accepts - 1) as usize];
            let is_voter = (input.now_ms - contributor_since) >= params.voter_days_ms
                && accepts.len() as u64 >= params.voter_accepts
                && last_activity_ms
                    .is_some_and(|last| input.now_ms - last < params.active_window_ms);
            let decayed =
                last_activity_ms.is_some_and(|last| input.now_ms - last >= params.decay_ms);
            if is_voter && !decayed {
                LadderEntry {
                    identity,
                    tier: LadderTier::Voter,
                    accepts: accepts.len() as u64,
                    tier_since_ms: Some(contributor_since),
                    last_activity_ms,
                }
            } else {
                LadderEntry {
                    identity,
                    tier: LadderTier::Contributor,
                    accepts: accepts.len() as u64,
                    tier_since_ms: Some(contributor_since),
                    last_activity_ms,
                }
            }
        } else {
            let initial = input.initial_voters.contains(&identity);
            let decayed = initial
                && last_activity_ms.is_some_and(|last| input.now_ms - last >= params.decay_ms);
            if initial && !decayed {
                LadderEntry {
                    identity,
                    tier: LadderTier::Voter,
                    accepts: accepts.len() as u64,
                    tier_since_ms: input.genesis_anchored_ms,
                    last_activity_ms,
                }
            } else {
                LadderEntry {
                    identity,
                    tier: LadderTier::Observer,
                    accepts: accepts.len() as u64,
                    tier_since_ms: None,
                    last_activity_ms,
                }
            }
        };
        entries.push(entry);
    }
    LadderRoster { entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const NOW: i64 = 1_720_000_000_000;

    fn view(
        identity: &str,
        op_type: OpType,
        payload: Value,
        anchored_ms: Option<i64>,
        objections: u64,
    ) -> LadderOpView {
        LadderOpView {
            op_hash: format!("{:0>64x}", anchored_ms.unwrap_or(0)),
            actor_kind: ActorKind::Person,
            actor_identity: identity.to_string(),
            op_type,
            payload,
            anchored_ms,
            objection_count: objections,
        }
    }

    fn meta_revise_payload(delay_ms: i64) -> Value {
        json!({
            "title": "新标题",
            "mechanism": { "kind": "delayed-veto", "delayMs": delay_ms, "vetoThreshold": { "count": 1 } }
        })
    }

    #[test]
    fn params_defaults_and_override() {
        let params = LadderParams::from_participation(&json!({})).unwrap();
        assert_eq!(params, LadderParams::default());
        assert_eq!(params.voter_days_ms, 30 * DAY);
        assert_eq!(params.voter_accepts, 3);
        let custom = LadderParams::from_participation(&json!({
            "ladderParams": { "voterDays": 10, "voterAccepts": 2 }
        }))
        .unwrap();
        assert_eq!(custom.voter_days_ms, 10 * DAY);
        assert_eq!(custom.voter_accepts, 2);
        assert_eq!(custom.contributor_accepts, 1);
        // 越界拒绝
        assert!(
            LadderParams::from_participation(&json!({
                "ladderParams": { "voterDays": 0 }
            }))
            .is_err()
        );
        assert!(
            LadderParams::from_participation(&json!({
                "ladderParams": "bad"
            }))
            .is_err()
        );
    }

    #[test]
    fn adoption_requires_delayed_veto_effective() {
        let id = "aa".repeat(32);
        // 窗口未满 → 不采纳
        let pending = view(
            &id,
            OpType::MetaRevise,
            meta_revise_payload(10_000),
            Some(NOW),
            0,
        );
        assert!(derive_adoptions(&[pending], NOW + 9_999).is_empty());
        // 窗口满 → 采纳，生效时刻 = 锚定 + delay
        let ok = view(
            &id,
            OpType::MetaRevise,
            meta_revise_payload(10_000),
            Some(NOW),
            0,
        );
        let events = derive_adoptions(&[ok], NOW + 10_000);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].effective_ms, NOW + 10_000);
        // 异议达阈值 → 否决不采纳
        let vetoed = view(
            &id,
            OpType::MetaRevise,
            meta_revise_payload(10_000),
            Some(NOW),
            1,
        );
        assert!(derive_adoptions(&[vetoed], NOW + 10_000).is_empty());
        // 未锚定 → 无从求值
        let unanchored = view(
            &id,
            OpType::MetaRevise,
            meta_revise_payload(10_000),
            None,
            0,
        );
        assert!(derive_adoptions(&[unanchored], NOW + 10_000).is_empty());
        // vote 机制 → 不计采纳
        let vote = view(
            &id,
            OpType::RuleChange,
            json!({ "mechanism": { "kind": "vote", "voterSet": "ladder:voters",
                "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                "snapshot": "required" }, "change": {}, "proposedAt": 1 }),
            Some(NOW),
            0,
        );
        assert!(derive_adoptions(&[vote], NOW + 10_000).is_empty());
        // org 表态不采纳
        let mut org_op = view(
            &id,
            OpType::MetaRevise,
            meta_revise_payload(10_000),
            Some(NOW),
            0,
        );
        org_op.actor_kind = ActorKind::Org;
        assert!(derive_adoptions(&[org_op], NOW + 10_000).is_empty());
    }

    #[test]
    fn account_age_uses_earliest_anchor() {
        let id = "aa".repeat(32);
        let ops = [
            view(&id, OpType::Content, json!({}), Some(NOW - 5 * DAY), 0),
            view(&id, OpType::Content, json!({}), Some(NOW - 1 * DAY), 0),
        ];
        assert_eq!(account_age_ms(&id, &ops, NOW), Some(5 * DAY));
        assert_eq!(account_age_ms(&"bb".repeat(32), &ops, NOW), None);
    }

    #[test]
    fn ladder_promotes_and_decays() {
        let id = "aa".repeat(32);
        let t0 = NOW - 40 * DAY; // 首次采纳锚定：40 天前
        let ops = vec![
            view(
                &id,
                OpType::MetaRevise,
                meta_revise_payload(DAY),
                Some(t0),
                0,
            ),
            view(
                &id,
                OpType::MetaRevise,
                meta_revise_payload(DAY),
                Some(t0 + 2 * DAY),
                0,
            ),
            view(
                &id,
                OpType::MetaRevise,
                meta_revise_payload(DAY),
                Some(t0 + 3 * DAY),
                0,
            ),
        ];
        let input = LadderInput {
            initial_voters: &[],
            genesis_anchored_ms: None,
            ops: &ops,
            params: LadderParams::default(),
            now_ms: NOW,
        };
        let roster = derive_ladder(&input);
        assert_eq!(roster.entries.len(), 1);
        assert_eq!(roster.entries[0].tier, LadderTier::Voter);
        assert_eq!(roster.entries[0].accepts, 3);
        assert_eq!(roster.voter_identities(), vec![id.clone()]);

        // 采纳不足（仅 2 次）→ 贡献者
        let two = LadderInput {
            ops: &ops[..2],
            ..input
        };
        assert_eq!(derive_ladder(&two).entries[0].tier, LadderTier::Contributor);
        // 采纳为 0 → 观察者
        let zero = LadderInput {
            ops: &ops[..0],
            ..input
        };
        assert_eq!(derive_ladder(&zero).entries.len(), 0);

        // 在级不满 30 天 → 贡献者
        let young = LadderInput {
            now_ms: t0 + 20 * DAY,
            ..input
        };
        let young_roster = derive_ladder(&young);
        assert_eq!(young_roster.entries[0].tier, LadderTier::Contributor);

        // 近 90 天无活动（全部操作锚定 100 天前）→ 衰减回贡献者
        let stale = LadderInput {
            now_ms: t0 + 100 * DAY,
            ..input
        };
        assert_eq!(
            derive_ladder(&stale).entries[0].tier,
            LadderTier::Contributor
        );

        // 衰减后恢复活动 → 回级（采纳与在级起点保留）
        let mut revived_ops = ops.clone();
        revived_ops.push(view(
            &id,
            OpType::Content,
            json!({}),
            Some(t0 + 100 * DAY),
            0,
        ));
        let revived = LadderInput {
            initial_voters: &[],
            genesis_anchored_ms: None,
            ops: &revived_ops,
            params: LadderParams::default(),
            now_ms: t0 + 100 * DAY,
        };
        assert_eq!(derive_ladder(&revived).entries[0].tier, LadderTier::Voter);

        // 初始投票者：无活动不衰减（无链上活动可判），有活动超窗衰减
        // 同样展开写出（`..input` 与 545 行 push 的可变借用冲突）。
        let initial = LadderInput {
            initial_voters: &[id.clone()],
            genesis_anchored_ms: Some(t0),
            ops: &ops[..0],
            params: LadderParams::default(),
            now_ms: NOW,
        };
        let initial_roster = derive_ladder(&initial);
        assert_eq!(initial_roster.entries[0].tier, LadderTier::Voter);
        let initial_stale = LadderInput {
            ops: &ops[..1],
            now_ms: t0 + 100 * DAY,
            ..initial
        };
        assert_eq!(
            derive_ladder(&initial_stale).entries[0].tier,
            LadderTier::Contributor
        );
    }
}
