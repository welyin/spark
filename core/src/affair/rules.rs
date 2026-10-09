//! 规则文档（rules）与静态检查（wiki/protocol/community/affair.md §5）。
//!
//! 规则文档在创世/规则修改合入时经 [`static_check_rules`] 把关：关闭条件只许
//! §5.2 三种可判定形式（到达顺序依赖、无公示期立即生效等不可判定表达在此
//! fail-closed），ruleChange 机制只许 §5.3 三形态，且含两条内核硬编码不可变
//! 校验——单点禁令（任何单一密钥直接产生效力的规则表达）与失联解锁路径
//! （求值侧见 decide.rs）。

use serde_json::Value;

use super::actor::is_valid_identity_id;
use super::effect::is_valid_org_id;
use crate::evidence::{normalize_object, sha256_hex};

/// 公示期缺省值 = 24h（§5.1）。
pub const DEFAULT_PUB_PERIOD_MS: i64 = 24 * 60 * 60 * 1000;
/// 公示期下限 = 24h（§5.1/§7：公示期吸收时钟偏差与副本滞后）。
pub const MIN_PUB_PERIOD_MS: i64 = DEFAULT_PUB_PERIOD_MS;
/// 决议公示期默认否决阈值：任一有效异议即打回复核（§6.2，可由 rules 配置）。
pub const DEFAULT_PUB_PERIOD_VETO_COUNT: u64 = 1;

/// 分数阈值 `{num, den}`（静态检查要求 0 < num ≤ den）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fraction {
    /// 分子。
    pub num: u64,
    /// 分母。
    pub den: u64,
}

impl Fraction {
    /// `part / base >= num / den`（u128 防溢出）。
    pub fn reached(&self, part: u64, base: u64) -> bool {
        u128::from(part) * u128::from(self.den) >= u128::from(base) * u128::from(self.num)
    }

    /// 满足分数条件的最小 part（`ceil(base * num / den)`）。
    pub fn min_part(&self, base: u64) -> u64 {
        (u128::from(base) * u128::from(self.num)).div_ceil(u128::from(self.den)) as u64
    }
}

/// threshold 关闭条件的基数来源（§5.2）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThresholdBase {
    /// `snapshot:<opHash>`：指定快照操作载入的名册。
    Snapshot(String),
    /// `ladder:voters`：阶梯投票者名册（推导属 C6；求值时基数由调用方注入）。
    LadderVoters,
}

/// 关闭条件（§5.2 可判定形式枚举，内核只识别这三种）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseCondition {
    /// 按 §8 排序键排序后的第 N 个匹配有效操作出现时关闭。
    OpCount {
        /// 匹配的操作类型。
        op_type: super::op::OpType,
        /// 可选插件命名串过滤（匹配 `payload.kind`，插件语义内核不解释）。
        filter: Option<String>,
        /// 第 N 个。
        count: u64,
    },
    /// 满足条件的有效操作数 ≥ 基数 × num/den 时关闭。
    Threshold {
        /// 基数来源。
        base: ThresholdBase,
        /// 阈值分数。
        ratio: Fraction,
    },
    /// 锚定时间不早于 notBefore（近似时间，语义见 §7.2；求值见 close.rs）。
    WallClock {
        /// Unix 毫秒。
        not_before: i64,
    },
}

/// 集体决策机制（§5.3 三形态）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mechanism {
    /// 正式投票：同意票/快照基数 ≥ threshold 且参与数 ≥ quorum。
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
    /// m/n 多签：≥m 个 signers 内不同身份的签名。
    Multisig {
        /// 门槛。
        m: u32,
        /// 声明总数。
        n: u32,
        /// 签名者身份列表。
        signers: Vec<String>,
    },
    /// 延迟生效 + 阈值否决。
    DelayedVeto {
        /// 公示期毫秒。
        delay_ms: i64,
        /// 否决阈值（有效异议数）。
        veto_count: u64,
    },
}

impl Mechanism {
    /// 机制类别字符串。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Vote { .. } => "vote",
            Self::Multisig { .. } => "multisig",
            Self::DelayedVeto { .. } => "delayed-veto",
        }
    }
}

/// 事务本体保留策略（affair-model §4.2，A22）：决议结论锚定组织存证链之外，
/// 事务**本体**去留的组织自选声明。内核只提供字段与复制组语义，不评估
/// 「该不该保留」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// 关注即保留（缺省，现状语义）：无人持有即消亡。
    Followers,
    /// 声明组织的数据节点将本体纳入长期副本——组织级自选动作，须配对组织侧
    /// pin 接受声明（`org:pin:{orgId}:{affairId}`，见 retention.rs）双条件生效。
    OrgPinned,
}

impl RetentionPolicy {
    /// 线形字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Followers => "followers",
            Self::OrgPinned => "org-pinned",
        }
    }
}

/// retention 声明的组织列表上限（防超大声明撑爆规则文档）。
pub const MAX_RETENTION_ORGS: usize = 64;

/// retention 字段解析结果（缺省 = followers + 空组织列表）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionDecl {
    /// 保留策略档位。
    pub policy: RetentionPolicy,
    /// org-pinned 档的意向组织列表（非空、去重、字典序）；followers 档恒空。
    pub orgs: Vec<String>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::Followers
    }
}

/// 解析 retention 字段（缺省 = followers；形状非法 fail-closed，不静默回退）。
pub fn parse_retention(value: Option<&Value>) -> Result<RetentionDecl, StaticCheckReject> {
    let obj = match value {
        None | Some(Value::Null) => return Ok(RetentionDecl::default()),
        Some(v) => v.as_object().ok_or(StaticCheckReject::InvalidRetention)?,
    };
    let policy = match obj.get("policy").and_then(Value::as_str) {
        Some("followers") => RetentionPolicy::Followers,
        Some("org-pinned") => RetentionPolicy::OrgPinned,
        _ => return Err(StaticCheckReject::InvalidRetention),
    };
    let orgs = match obj.get("orgs") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => {
            let arr = v.as_array().ok_or(StaticCheckReject::InvalidRetention)?;
            if arr.len() > MAX_RETENTION_ORGS {
                return Err(StaticCheckReject::InvalidRetention);
            }
            let mut orgs = Vec::with_capacity(arr.len());
            for org in arr {
                let id = org
                    .as_str()
                    .filter(|s| is_valid_org_id(s))
                    .ok_or(StaticCheckReject::InvalidRetention)?;
                if orgs.contains(&id.to_string()) {
                    return Err(StaticCheckReject::InvalidRetention);
                }
                orgs.push(id.to_string());
            }
            orgs.sort();
            orgs
        }
    };
    match policy {
        // followers 档携带意向组织 = 自相矛盾声明，拒绝（不猜着执行）
        RetentionPolicy::Followers if !orgs.is_empty() => {
            Err(StaticCheckReject::InvalidRetention)
        }
        // org-pinned 档必须点名至少一个意向组织（否则与 followers 无异）
        RetentionPolicy::OrgPinned if orgs.is_empty() => {
            Err(StaticCheckReject::InvalidRetention)
        }
        _ => Ok(RetentionDecl { policy, orgs }),
    }
}

/// 静态检查拒绝原因（§5.6；reason 字符串稳定，golden vectors 可依赖）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticCheckReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// engine 非 "b1"。
    UnknownEngine,
    /// closeConditions 项非 §5.2 枚举（含到达顺序依赖等不可判定形式）。
    UnknownCloseCondition,
    /// op-count 的 opType 非内核枚举。
    UnknownOpType,
    /// op-count 的 count < 1。
    InvalidCount,
    /// threshold 的 base 非 `snapshot:<opHash>` / `ladder:voters`。
    InvalidThresholdBase,
    /// 分数阈值不满足 0 < num ≤ den。
    InvalidThreshold,
    /// pubPeriod.delayMs < 24h（含 wall-clock 无公示期立即生效的表达）。
    InvalidPubPeriod,
    /// ruleChange 机制非 §5.3 三形态。
    UnknownMechanism,
    /// multisig 不满足 1 ≤ m ≤ n ≤ signers 长度 / signers 非法。
    InvalidMultisig,
    /// 单点禁令：单一密钥直接产生效力的规则表达（multisig m < 2）。
    SinglePointEffect,
    /// delayed-veto 参数非法（delayMs ≤ 0 或否决阈值 < 1）。
    InvalidDelayedVeto,
    /// participation 声明非法（ladder/combine/credentials 形状）。
    InvalidParticipation,
    /// exec 声明非法（非 null 且 verify.kind 非三枚举）。
    InvalidExec,
    /// retention 声明非法（policy 非两枚举 / orgs 形状与档位矛盾）。
    InvalidRetention,
}

impl StaticCheckReject {
    /// 稳定 reason 字符串。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Malformed => "malformed-rules",
            Self::UnknownEngine => "unknown-engine",
            Self::UnknownCloseCondition => "unknown-close-condition",
            Self::UnknownOpType => "unknown-op-type",
            Self::InvalidCount => "invalid-count",
            Self::InvalidThresholdBase => "invalid-threshold-base",
            Self::InvalidThreshold => "invalid-threshold",
            Self::InvalidPubPeriod => "invalid-pub-period",
            Self::UnknownMechanism => "unknown-mechanism",
            Self::InvalidMultisig => "invalid-multisig",
            Self::SinglePointEffect => "single-point-effect",
            Self::InvalidDelayedVeto => "invalid-delayed-veto",
            Self::InvalidParticipation => "invalid-participation",
            Self::InvalidExec => "invalid-exec",
            Self::InvalidRetention => "invalid-retention",
        }
    }
}

/// 静态检查通过的规则文档视图。
#[derive(Clone, Debug, PartialEq)]
pub struct RulesDoc {
    /// 关闭条件数组（任一满足即关闭；可空 = 不自动关闭）。
    pub close_conditions: Vec<CloseCondition>,
    /// 公示期毫秒（≥ 24h）。
    pub pub_period_ms: i64,
    /// 决议公示期否决阈值（§6.2；缺省 1）。
    pub pub_period_veto_count: u64,
    /// 规则修改的集体决策机制。
    pub rule_change: Mechanism,
    /// 本体保留策略（affair-model §4.2；缺省 followers）。
    pub retention: RetentionDecl,
    /// 规则文档原文（canonical/rulesHash 与 patch 应用都在原文上进行）。
    pub raw: Value,
}

fn parse_fraction(value: Option<&Value>) -> Result<Fraction, StaticCheckReject> {
    let obj = value
        .and_then(Value::as_object)
        .ok_or(StaticCheckReject::Malformed)?;
    let num = obj
        .get("num")
        .and_then(Value::as_u64)
        .ok_or(StaticCheckReject::InvalidThreshold)?;
    let den = obj
        .get("den")
        .and_then(Value::as_u64)
        .ok_or(StaticCheckReject::InvalidThreshold)?;
    let fraction = Fraction { num, den };
    if num == 0 || num > den {
        return Err(StaticCheckReject::InvalidThreshold);
    }
    Ok(fraction)
}

/// 解析单条关闭条件（未知 type 即拒绝：不可判定条件 fail-closed，§5.6）。
pub fn parse_close_condition(value: &Value) -> Result<CloseCondition, StaticCheckReject> {
    let obj = value.as_object().ok_or(StaticCheckReject::Malformed)?;
    match obj.get("type").and_then(Value::as_str) {
        Some("op-count") => {
            let op_type = obj
                .get("opType")
                .and_then(Value::as_str)
                .and_then(super::op::parse_op_type)
                .ok_or(StaticCheckReject::UnknownOpType)?;
            let filter = match obj.get("filter") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let s = v.as_str().ok_or(StaticCheckReject::Malformed)?;
                    if s.is_empty() || s.chars().count() > 128 {
                        return Err(StaticCheckReject::Malformed);
                    }
                    Some(s.to_string())
                }
            };
            let count = obj
                .get("count")
                .and_then(Value::as_u64)
                .ok_or(StaticCheckReject::InvalidCount)?;
            if count < 1 {
                return Err(StaticCheckReject::InvalidCount);
            }
            Ok(CloseCondition::OpCount {
                op_type,
                filter,
                count,
            })
        }
        Some("threshold") => {
            let base_str = obj
                .get("base")
                .and_then(Value::as_str)
                .ok_or(StaticCheckReject::InvalidThresholdBase)?;
            let base = match base_str.strip_prefix("snapshot:") {
                Some(op_hash) if is_valid_identity_id(op_hash) => {
                    ThresholdBase::Snapshot(op_hash.to_string())
                }
                Some(_) => return Err(StaticCheckReject::InvalidThresholdBase),
                None if base_str == "ladder:voters" => ThresholdBase::LadderVoters,
                None => return Err(StaticCheckReject::InvalidThresholdBase),
            };
            let ratio = parse_fraction(Some(value))?;
            Ok(CloseCondition::Threshold { base, ratio })
        }
        Some("wall-clock") => {
            let not_before = obj
                .get("notBefore")
                .and_then(Value::as_i64)
                .ok_or(StaticCheckReject::Malformed)?;
            if not_before <= 0 {
                return Err(StaticCheckReject::Malformed);
            }
            Ok(CloseCondition::WallClock { not_before })
        }
        Some(_) => Err(StaticCheckReject::UnknownCloseCondition),
        None => Err(StaticCheckReject::Malformed),
    }
}

/// 解析集体决策机制（§5.3；含单点禁令硬校验）。
pub fn parse_mechanism(value: &Value) -> Result<Mechanism, StaticCheckReject> {
    let obj = value.as_object().ok_or(StaticCheckReject::Malformed)?;
    match obj.get("kind").and_then(Value::as_str) {
        Some("vote") => {
            let voter_set = obj
                .get("voterSet")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(StaticCheckReject::Malformed)?
                .to_string();
            let snapshot = obj
                .get("snapshot")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(StaticCheckReject::Malformed)?
                .to_string();
            Ok(Mechanism::Vote {
                voter_set,
                threshold: parse_fraction(obj.get("threshold"))?,
                quorum: parse_fraction(obj.get("quorum"))?,
                snapshot,
            })
        }
        Some("multisig") => {
            let m = obj
                .get("m")
                .and_then(Value::as_u64)
                .ok_or(StaticCheckReject::InvalidMultisig)? as u32;
            let n = obj
                .get("n")
                .and_then(Value::as_u64)
                .ok_or(StaticCheckReject::InvalidMultisig)? as u32;
            let signers_value = obj
                .get("signers")
                .and_then(Value::as_array)
                .ok_or(StaticCheckReject::InvalidMultisig)?;
            let mut signers = Vec::with_capacity(signers_value.len());
            for signer in signers_value {
                let id = signer
                    .as_str()
                    .filter(|s| is_valid_identity_id(s))
                    .ok_or(StaticCheckReject::InvalidMultisig)?;
                if signers.contains(&id.to_string()) {
                    return Err(StaticCheckReject::InvalidMultisig);
                }
                signers.push(id.to_string());
            }
            if m < 1 || m > n || n as usize > signers.len() {
                return Err(StaticCheckReject::InvalidMultisig);
            }
            // 单点禁令（§5.3 内核硬编码）：m == 1 时任一单一密钥即可产生效力
            if m < 2 {
                return Err(StaticCheckReject::SinglePointEffect);
            }
            Ok(Mechanism::Multisig { m, n, signers })
        }
        Some("delayed-veto") => {
            let delay_ms = obj
                .get("delayMs")
                .and_then(Value::as_i64)
                .ok_or(StaticCheckReject::InvalidDelayedVeto)?;
            let veto_count = obj
                .get("vetoThreshold")
                .and_then(|v| v.get("count"))
                .and_then(Value::as_u64)
                .ok_or(StaticCheckReject::InvalidDelayedVeto)?;
            if delay_ms <= 0 || veto_count < 1 {
                return Err(StaticCheckReject::InvalidDelayedVeto);
            }
            Ok(Mechanism::DelayedVeto {
                delay_ms,
                veto_count,
            })
        }
        Some(_) => Err(StaticCheckReject::UnknownMechanism),
        None => Err(StaticCheckReject::Malformed),
    }
}

fn check_participation(value: &Value) -> Result<(), StaticCheckReject> {
    let obj = value
        .as_object()
        .ok_or(StaticCheckReject::InvalidParticipation)?;
    let check_ladder = |key: &str| -> Result<(), StaticCheckReject> {
        let Some(ladder) = obj
            .get(key)
            .and_then(|v| v.get("ladder"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        match ladder {
            "observer" | "contributor" | "voter" => Ok(()),
            _ => Err(StaticCheckReject::InvalidParticipation),
        }
    };
    check_ladder("contribute")?;
    check_ladder("vote")?;
    if let Some(combine) = obj.get("combine") {
        match combine.as_str() {
            Some("all") | Some("any") => {}
            _ => return Err(StaticCheckReject::InvalidParticipation),
        }
    }
    if let Some(credentials) = obj.get("credentials") {
        for cred in credentials
            .as_array()
            .ok_or(StaticCheckReject::InvalidParticipation)?
        {
            let entry = cred
                .as_object()
                .ok_or(StaticCheckReject::InvalidParticipation)?;
            let cred_type_ok = entry
                .get("credType")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            let domain_ok = entry
                .get("verifierDomain")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            if !cred_type_ok || !domain_ok {
                return Err(StaticCheckReject::InvalidParticipation);
            }
        }
    }
    // 阶梯参数越界在校验期即拒（§5.5/§5.6）：与 affair_ladder_status 读路径
    // 共用 LadderParams::from_participation 把关，创世与 rule-change patch
    // 应用后同口径（评审建议 1）
    super::ladder::LadderParams::from_participation(value)
        .map_err(|_| StaticCheckReject::InvalidParticipation)?;
    Ok(())
}

fn check_exec(value: &Value) -> Result<(), StaticCheckReject> {
    if value.is_null() {
        return Ok(());
    }
    let obj = value.as_object().ok_or(StaticCheckReject::InvalidExec)?;
    let verify = obj
        .get("verify")
        .and_then(Value::as_object)
        .ok_or(StaticCheckReject::InvalidExec)?;
    match verify.get("kind").and_then(Value::as_str) {
        Some("delayed-veto") | Some("verifier-sign") | Some("vote") => Ok(()),
        _ => Err(StaticCheckReject::InvalidExec),
    }
}

/// 规则文档静态检查（§5.6）：任一不满足 → 拒绝合入。
pub fn static_check_rules(value: &Value) -> Result<RulesDoc, StaticCheckReject> {
    let obj = value.as_object().ok_or(StaticCheckReject::Malformed)?;
    if obj.get("engine").and_then(Value::as_str) != Some("b1") {
        return Err(StaticCheckReject::UnknownEngine);
    }
    let close_conditions = match obj.get("closeConditions") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or(StaticCheckReject::Malformed)?
            .iter()
            .map(parse_close_condition)
            .collect::<Result<Vec<_>, _>>()?,
    };
    // pubPeriod 形状检查 fail-closed：字段整体缺席才应用缺省值；present 但
    // 形状/类型非法（非对象、delayMs/vetoThreshold.count 类型错误）→ Malformed
    // 拒绝，不得静默回退缺省值让坏形状规则按缺省生效（评审阻塞 2）
    let pub_period = match obj.get("pubPeriod") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_object().ok_or(StaticCheckReject::Malformed)?),
    };
    let pub_period_ms = match pub_period.and_then(|p| p.get("delayMs")) {
        None | Some(Value::Null) => DEFAULT_PUB_PERIOD_MS,
        Some(v) => v.as_i64().ok_or(StaticCheckReject::Malformed)?,
    };
    // 公示期下限 24h（§5.1）；「wall-clock 无公示期立即生效」的表达在此被拒
    if pub_period_ms < MIN_PUB_PERIOD_MS {
        return Err(StaticCheckReject::InvalidPubPeriod);
    }
    let pub_period_veto_count = match pub_period.and_then(|p| p.get("vetoThreshold")) {
        None | Some(Value::Null) => DEFAULT_PUB_PERIOD_VETO_COUNT,
        Some(v) => {
            let veto = v.as_object().ok_or(StaticCheckReject::Malformed)?;
            match veto.get("count") {
                None | Some(Value::Null) => DEFAULT_PUB_PERIOD_VETO_COUNT,
                Some(c) => c.as_u64().ok_or(StaticCheckReject::Malformed)?,
            }
        }
    };
    if pub_period_veto_count < 1 {
        return Err(StaticCheckReject::InvalidPubPeriod);
    }
    let rule_change = parse_mechanism(obj.get("ruleChange").ok_or(StaticCheckReject::Malformed)?)?;
    if let Some(participation) = obj.get("participation") {
        check_participation(participation)?;
    }
    if let Some(exec) = obj.get("exec") {
        check_exec(exec)?;
    }
    let retention = parse_retention(obj.get("retention"))?;
    Ok(RulesDoc {
        close_conditions,
        pub_period_ms,
        pub_period_veto_count,
        rule_change,
        retention,
        raw: value.clone(),
    })
}

/// 规则文档哈希：`sha256hex(normalizeObject(rules))`（§6.1 rulesHash 口径）。
pub fn rules_hash(rules: &Value) -> String {
    sha256_hex(&normalize_object(rules))
}

/// 规则 patch 应用（§5.4：顶层键覆盖，null = 删除该键）。
pub fn apply_rule_patch(old: &Value, patch: &Value) -> Value {
    let mut out = old.as_object().cloned().unwrap_or_default();
    if let Some(patch_obj) = patch.as_object() {
        for (key, value) in patch_obj {
            if value.is_null() {
                out.shift_remove(key);
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline_rules() -> Value {
        json!({
            "engine": "b1",
            "closeConditions": [{ "type": "op-count", "opType": "content", "count": 100 }],
            "pubPeriod": { "delayMs": 86400000 },
            "participation": { "contribute": { "ladder": "contributor" }, "vote": { "ladder": "voter" }, "combine": "all" },
            "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
            "exec": null
        })
    }

    #[test]
    fn baseline_passes() {
        let doc = static_check_rules(&baseline_rules()).unwrap();
        assert_eq!(doc.pub_period_ms, DEFAULT_PUB_PERIOD_MS);
        assert_eq!(doc.pub_period_veto_count, 1);
        assert!(matches!(doc.rule_change, Mechanism::DelayedVeto { .. }));
        // retention 缺省 = followers（旧事务无感知，A22）
        assert_eq!(doc.retention, RetentionDecl::default());
    }

    #[test]
    fn retention_two_tiers_and_default() {
        let org_a = format!("org_{}", "aa".repeat(32));
        let org_b = format!("org_{}", "bb".repeat(32));
        // 显式 followers
        let mut r = baseline_rules();
        r["retention"] = json!({ "policy": "followers" });
        let doc = static_check_rules(&r).unwrap();
        assert_eq!(doc.retention.policy, RetentionPolicy::Followers);
        assert!(doc.retention.orgs.is_empty());
        // org-pinned：意向组织列表解析 + 字典序
        let mut r = baseline_rules();
        r["retention"] = json!({ "policy": "org-pinned", "orgs": [org_b, org_a.clone()] });
        let doc = static_check_rules(&r).unwrap();
        assert_eq!(doc.retention.policy, RetentionPolicy::OrgPinned);
        assert_eq!(doc.retention.orgs, vec![org_a.clone(), format!("org_{}", "bb".repeat(32))]);
        // retention 经规则 patch 修改后重过静态检查（§5.4 顶层键覆盖）
        let patched = apply_rule_patch(&r, &json!({ "retention": { "policy": "followers" } }));
        let doc = static_check_rules(&patched).unwrap();
        assert_eq!(doc.retention.policy, RetentionPolicy::Followers);
        // patch 删除 retention 键 → 回缺省
        let patched = apply_rule_patch(&r, &json!({ "retention": null }));
        assert_eq!(
            static_check_rules(&patched).unwrap().retention,
            RetentionDecl::default()
        );
    }

    #[test]
    fn retention_rejects_bad_shapes() {
        let org_a = format!("org_{}", "aa".repeat(32));
        let cases: Vec<Value> = vec![
            json!("org-pinned"),                                          // 非对象
            json!({ "policy": "forever" }),                               // 未知档位
            json!({ "orgs": [org_a] }),                                   // 缺 policy
            json!({ "policy": "org-pinned" }),                            // org-pinned 缺 orgs
            json!({ "policy": "org-pinned", "orgs": [] }),                // org-pinned 空 orgs
            json!({ "policy": "followers", "orgs": [org_a] }),            // followers 带 orgs
            json!({ "policy": "org-pinned", "orgs": ["org_xyz"] }),       // 非法 orgId
            json!({ "policy": "org-pinned", "orgs": [org_a, org_a] }),    // 重复组织
            json!({ "policy": "org-pinned", "orgs": "org_abc" }),         // orgs 非数组
        ];
        for (i, retention) in cases.iter().enumerate() {
            let mut r = baseline_rules();
            r["retention"] = retention.clone();
            assert_eq!(
                static_check_rules(&r).unwrap_err(),
                StaticCheckReject::InvalidRetention,
                "case {i} 必败"
            );
        }
    }

    #[test]
    fn rejects_undecidable_and_single_point() {
        // 到达顺序依赖：未知关闭条件类型
        let mut r = baseline_rules();
        r["closeConditions"] = json!([{ "type": "first-received", "count": 5 }]);
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::UnknownCloseCondition
        );
        // 无公示期的 wall-clock 立即生效
        let mut r = baseline_rules();
        r["closeConditions"] = json!([{ "type": "wall-clock", "notBefore": 1_730_000_000_000i64 }]);
        r["pubPeriod"] = json!({ "delayMs": 0 });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::InvalidPubPeriod
        );
        // 单点效力：multisig m = 1
        let mut r = baseline_rules();
        r["ruleChange"] =
            json!({ "kind": "multisig", "m": 1, "n": 1, "signers": ["ab".repeat(32)] });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::SinglePointEffect
        );
        // m > n
        let mut r = baseline_rules();
        r["ruleChange"] = json!({ "kind": "multisig", "m": 3, "n": 2, "signers": ["ab".repeat(32), "cd".repeat(32), "ef".repeat(32)] });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::InvalidMultisig
        );
    }

    #[test]
    fn patch_semantics() {
        let old = json!({"engine": "b1", "exec": null, "keep": 1});
        let patched = apply_rule_patch(&old, &json!({"exec": {"x": 1}, "keep": null}));
        assert_eq!(patched, json!({"engine": "b1", "exec": {"x": 1}}));
    }

    #[test]
    fn malformed_pub_period_shape_rejected() {
        // pubPeriod 存在但非对象 → Malformed（不得回退缺省 24h 生效）
        let mut r = baseline_rules();
        r["pubPeriod"] = json!("86400000");
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::Malformed
        );
        // delayMs 类型错误 → Malformed
        let mut r = baseline_rules();
        r["pubPeriod"] = json!({ "delayMs": "86400000" });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::Malformed
        );
        // vetoThreshold 非对象 → Malformed
        let mut r = baseline_rules();
        r["pubPeriod"] = json!({ "delayMs": 86400000, "vetoThreshold": "1" });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::Malformed
        );
        // vetoThreshold.count 类型错误 → Malformed
        let mut r = baseline_rules();
        r["pubPeriod"] = json!({ "delayMs": 86400000, "vetoThreshold": { "count": "1" } });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::Malformed
        );
        // 字段整体缺席 → 缺省值生效（24h / 否决阈值 1）
        let mut r = baseline_rules();
        r.as_object_mut().unwrap().shift_remove("pubPeriod");
        let doc = static_check_rules(&r).unwrap();
        assert_eq!(doc.pub_period_ms, DEFAULT_PUB_PERIOD_MS);
        assert_eq!(doc.pub_period_veto_count, 1);
        // vetoThreshold 缺席 count → 缺省 1
        let mut r = baseline_rules();
        r["pubPeriod"] = json!({ "delayMs": 86400000, "vetoThreshold": {} });
        assert_eq!(static_check_rules(&r).unwrap().pub_period_veto_count, 1);
    }

    #[test]
    fn ladder_params_checked_at_static_check() {
        // 越界阶梯参数在校验期即拒（与 ladder 读路径同口径）
        let mut r = baseline_rules();
        r["participation"]["ladderParams"] = json!({ "voterDays": 0 });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::InvalidParticipation
        );
        let mut r = baseline_rules();
        r["participation"]["ladderParams"] = json!({ "contributorAccepts": 0 });
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::InvalidParticipation
        );
        // 形状非法（非对象）→ InvalidParticipation
        let mut r = baseline_rules();
        r["participation"]["ladderParams"] = json!("bad");
        assert_eq!(
            static_check_rules(&r).unwrap_err(),
            StaticCheckReject::InvalidParticipation
        );
        // 合法覆盖 → 通过
        let mut r = baseline_rules();
        r["participation"]["ladderParams"] =
            json!({ "contributorAccepts": 2, "voterDays": 10, "voterAccepts": 2 });
        assert!(static_check_rules(&r).is_ok());
    }
}
