//! 操作日志条目与 DAG 簿记（wiki/protocol/community/affair.md §3/§4/§8）。
//!
//! - `opHash = sha256hex(normalizeObject(操作条目全文含 sig))`：链承诺覆盖签名本身；
//! - 日志是 DAG 不是单链：并发操作引用同一 prevOpHash 即合法分支，prevOpHash
//!   语义是因果见证；状态推导不依赖链拓扑，只依赖操作集合内容；
//! - 未知 opType 整条拒收（fail-closed，§4）；入站校验顺序（§3.2）：
//!   结构 → affairId 匹配 → declaredAt 新鲜度 → actor 绑定 → 验签 →
//!   prevOpHash/vote/objection 指向已知条目（未知则暂存待补，不因乱序拒收）。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::actor::{Actor, ActorReject, is_valid_identity_id, parse_actor, verify_actor_signature};
use super::refs::parse_ref;
use crate::evidence::{normalize_object, sha256_hex};

/// declaredAt 入站新鲜度窗口：±10 min（nodeInfoClaim 先例，§3.1）。
pub const OP_FRESHNESS_WINDOW_MS: i64 = 10 * 60 * 1000;

/// opType 内核识别面（§4 枚举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpType {
    /// 插件语义载荷，内核只验签名与门槛资格。
    Content,
    /// 规则修改提议（§5.4）。
    RuleChange,
    /// 内核级表决票（仅服务 rule-change / meta-revise 的 vote 变体）。
    Vote,
    /// 对延迟生效类提议的否决异议。
    Objection,
    /// 主持人元数据修订提议（§11.2）。
    MetaRevise,
    /// 主持人展示层操作（§11.1）。
    Moderate,
    /// 追加事务间引用（§10）。
    Ref,
    /// 法定人数/阶梯名册快照载入（§9）。
    Snapshot,
    /// 决议产物（§6）。
    Resolution,
    /// 执行回报（§6.3，执行型事务）。
    ExecReport,
}

impl OpType {
    /// 线形字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::RuleChange => "rule-change",
            Self::Vote => "vote",
            Self::Objection => "objection",
            Self::MetaRevise => "meta-revise",
            Self::Moderate => "moderate",
            Self::Ref => "ref",
            Self::Snapshot => "snapshot",
            Self::Resolution => "resolution",
            Self::ExecReport => "exec-report",
        }
    }
}

/// 解析 opType（未知 = None，fail-closed）。
pub fn parse_op_type(s: &str) -> Option<OpType> {
    Some(match s {
        "content" => OpType::Content,
        "rule-change" => OpType::RuleChange,
        "vote" => OpType::Vote,
        "objection" => OpType::Objection,
        "meta-revise" => OpType::MetaRevise,
        "moderate" => OpType::Moderate,
        "ref" => OpType::Ref,
        "snapshot" => OpType::Snapshot,
        "resolution" => OpType::Resolution,
        "exec-report" => OpType::ExecReport,
        _ => return None,
    })
}

/// 操作条目解析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct AffairOp {
    /// 所属事务。
    pub affair_id: String,
    /// 因果见证：操作者观察到的日志头（首条 = affairId）。
    pub prev_op_hash: String,
    /// 操作类型。
    pub op_type: OpType,
    /// 载荷原文（按 opType 已做结构校验）。
    pub payload: Value,
    /// 操作者。
    pub actor: Actor,
    /// 签名者声明时间（毫秒）。
    pub declared_at: i64,
    /// 签名 base64。
    pub sig: String,
}

/// 操作校验失败原因（reason 字符串稳定，golden vectors 可依赖）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// opV 非 1。
    BadVersion,
    /// 未知 opType（fail-closed）。
    UnknownOpType,
    /// affairId 与所属事务不符（防跨事务搬迁）。
    AffairMismatch,
    /// declaredAt 超出 ±10 min 新鲜度窗口。
    StaleDeclaredAt,
    /// actor 校验失败。
    BadActor(ActorReject),
    /// 签名验证失败。
    InvalidSignature,
    /// prevOpHash 形状非法。
    BadPrevOpHash,
    /// payload 结构不符（内层为稳定细分 reason）。
    BadPayload(&'static str),
    /// moderate / meta-revise 非主持人发出（§11：拒入有效集）。
    NotModerator,
    /// 自指引用（§10）。
    SelfReference,
}

impl OpReject {
    /// 稳定 reason 字符串。
    pub fn reason(&self) -> String {
        match self {
            Self::Malformed => "malformed-op".to_string(),
            Self::BadVersion => "bad-version".to_string(),
            Self::UnknownOpType => "unknown-op-type".to_string(),
            Self::AffairMismatch => "affair-mismatch".to_string(),
            Self::StaleDeclaredAt => "stale-declared-at".to_string(),
            Self::BadActor(r) => r.reason().to_string(),
            Self::InvalidSignature => "invalid-signature".to_string(),
            Self::BadPrevOpHash => "bad-prev-op-hash".to_string(),
            Self::BadPayload(detail) => detail.to_string(),
            Self::NotModerator => "not-moderator".to_string(),
            Self::SelfReference => "self-reference".to_string(),
        }
    }
}

/// 操作签名载荷 = `canonical(剔除 sig 的全部字段)`。
pub fn op_sign_payload(entry: &Value) -> Result<String, OpReject> {
    let obj = entry.as_object().ok_or(OpReject::Malformed)?;
    let mut sans = obj.clone();
    sans.shift_remove("sig");
    Ok(normalize_object(&Value::Object(sans)))
}

/// opHash（§3.2：全文含 sig）。
pub fn compute_op_hash(entry: &Value) -> Result<String, OpReject> {
    entry.as_object().ok_or(OpReject::Malformed)?;
    Ok(sha256_hex(&normalize_object(entry)))
}

/// §8 确定性排序键：opHash hex 字符串字典序（UTF-8 字节序）。
pub fn sort_op_hashes(hashes: &[String]) -> Vec<String> {
    let mut out = hashes.to_vec();
    out.sort();
    out
}

/// 因果闭包（DAG 祖先集）：从 `root` 沿 prevOpHash 回溯可达的操作集合
/// （含 root 本身；prev 指向创世 affairId 时止步——创世不在操作集内）。
/// `edges` = (opHash, prevOpHash) 对。root 不在操作集 → None（fail-closed）。
///
/// 「推导至 X 操作（含）为止」（§9 快照 asOf / §6.1 决议复算的操作集合）的
/// 实现口径：取 X 的因果闭包。与 §8 字典序切分相比，因果闭包在副本乱序补齐
/// 无关分支时保持稳定（后到的旁支操作不改变祖先集），且天然表达「操作者观察
/// 到的历史」（prevOpHash = 因果见证，§3.2）。visited 集天然防环。
pub fn ancestor_op_hashes(root: &str, edges: &[(String, String)]) -> Option<HashSet<String>> {
    let prev_of: HashMap<&str, &str> = edges
        .iter()
        .map(|(hash, prev)| (hash.as_str(), prev.as_str()))
        .collect();
    if !prev_of.contains_key(root) {
        return None;
    }
    let mut visited = HashSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(hash) = stack.pop() {
        if !visited.insert(hash.clone()) {
            continue;
        }
        if let Some(prev) = prev_of.get(hash.as_str())
            && prev_of.contains_key(*prev)
        {
            stack.push((*prev).to_string());
        }
    }
    Some(visited)
}

fn validate_hex64(value: Option<&Value>) -> Result<String, OpReject> {
    let s = value.and_then(Value::as_str).ok_or(OpReject::Malformed)?;
    if !is_valid_identity_id(s) {
        return Err(OpReject::Malformed);
    }
    Ok(s.to_string())
}

/// payload 结构校验（按 opType，§4 payload 表）。返回需要指向已知条目的
/// 引用目标（vote.proposal / objection.target / moderate.target）。
fn validate_payload(
    op_type: OpType,
    payload: &Value,
    affair_id: &str,
) -> Result<Option<String>, OpReject> {
    match op_type {
        // 插件定义，内核不解释（§4）
        OpType::Content => Ok(None),
        OpType::RuleChange => {
            super::decide::parse_rule_change_payload(payload).map_err(OpReject::BadPayload)?;
            Ok(None)
        }
        OpType::Vote => {
            let obj = payload
                .as_object()
                .ok_or(OpReject::BadPayload("bad-vote-payload"))?;
            let proposal = validate_hex64(obj.get("proposal"))
                .map_err(|_| OpReject::BadPayload("bad-vote-payload"))?;
            match obj.get("choice").and_then(Value::as_str) {
                Some("yes") | Some("no") => Ok(Some(proposal)),
                _ => Err(OpReject::BadPayload("bad-vote-payload")),
            }
        }
        OpType::Objection => {
            let obj = payload
                .as_object()
                .ok_or(OpReject::BadPayload("bad-objection-payload"))?;
            let target = validate_hex64(obj.get("target"))
                .map_err(|_| OpReject::BadPayload("bad-objection-payload"))?;
            check_optional_reason(obj.get("reason"), "bad-objection-payload")?;
            Ok(Some(target))
        }
        OpType::MetaRevise => {
            super::meta::parse_meta_revise_payload(payload).map_err(OpReject::BadPayload)?;
            Ok(None)
        }
        OpType::Moderate => {
            let obj = payload
                .as_object()
                .ok_or(OpReject::BadPayload("bad-moderate-payload"))?;
            if obj.get("action").and_then(Value::as_str) != Some("fold") {
                return Err(OpReject::BadPayload("bad-moderate-payload"));
            }
            let target = validate_hex64(obj.get("target"))
                .map_err(|_| OpReject::BadPayload("bad-moderate-payload"))?;
            check_optional_reason(obj.get("reason"), "bad-moderate-payload")?;
            Ok(Some(target))
        }
        OpType::Ref => {
            parse_ref(payload, Some(affair_id)).map_err(|e| {
                if e == super::refs::RefReject::SelfReference {
                    OpReject::SelfReference
                } else {
                    OpReject::BadPayload("bad-ref-payload")
                }
            })?;
            Ok(None)
        }
        OpType::Snapshot => {
            super::snapshot::parse_snapshot_payload(payload).map_err(OpReject::BadPayload)?;
            Ok(None)
        }
        OpType::Resolution => {
            super::resolution::parse_resolution_payload(payload).map_err(OpReject::BadPayload)?;
            Ok(None)
        }
        OpType::ExecReport => {
            let report =
                super::exec::parse_exec_report_payload(payload).map_err(OpReject::BadPayload)?;
            // 决议引用纳入指向校验：未知决议按 §4 乱序规则暂存待补
            Ok(Some(report.resolution))
        }
    }
}

fn check_optional_reason(value: Option<&Value>, reject: &'static str) -> Result<(), OpReject> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(v) => match v.as_str() {
            Some(s) if s.encode_utf16().count() <= 256 => Ok(()),
            _ => Err(OpReject::BadPayload(reject)),
        },
    }
}

/// 结构解析（不验签）。
pub fn parse_op(entry: &Value) -> Result<AffairOp, OpReject> {
    let obj = entry.as_object().ok_or(OpReject::Malformed)?;
    if obj.get("opV").and_then(Value::as_u64) != Some(1) {
        return Err(OpReject::BadVersion);
    }
    let affair_id = validate_hex64(obj.get("affairId"))?;
    let prev_op_hash = obj
        .get("prevOpHash")
        .and_then(Value::as_str)
        .filter(|s| is_valid_identity_id(s))
        .ok_or(OpReject::BadPrevOpHash)?
        .to_string();
    let op_type = obj
        .get("opType")
        .and_then(Value::as_str)
        .and_then(parse_op_type)
        .ok_or(OpReject::UnknownOpType)?;
    let payload = obj.get("payload").cloned().ok_or(OpReject::Malformed)?;
    let actor =
        parse_actor(obj.get("actor").ok_or(OpReject::Malformed)?).map_err(OpReject::BadActor)?;
    let declared_at = obj
        .get("declaredAt")
        .and_then(Value::as_i64)
        .ok_or(OpReject::Malformed)?;
    let sig = obj
        .get("sig")
        .and_then(Value::as_str)
        .ok_or(OpReject::Malformed)?
        .to_string();
    Ok(AffairOp {
        affair_id,
        prev_op_hash,
        op_type,
        payload,
        actor,
        declared_at,
        sig,
    })
}

/// 入站校验（§3.2 链式）：结构 → affairId 匹配 → 新鲜度 → 验签 → payload 结构。
/// 返回（解析结果，指向已知条目的引用目标）。新鲜度门槛只适用于实时提交
/// （§3.1）；复制入站用 [`verify_op_replicated`]。
pub fn verify_op(
    entry: &Value,
    expected_affair_id: &str,
    now_ms: i64,
) -> Result<(AffairOp, Option<String>), OpReject> {
    verify_op_inner(entry, expected_affair_id, Some(now_ms))
}

/// 复制入站校验：与 [`verify_op`] 同链但豁免 declaredAt 新鲜度（§3.1 口径：
/// 复制保真由签名 + opHash 链承担，时间语义各副本吃本地锚定时刻，declaredAt
/// 为自报文本、永不进入判定——历史复制补齐不应被 ±10min 窗口锁死）。
pub fn verify_op_replicated(
    entry: &Value,
    expected_affair_id: &str,
) -> Result<(AffairOp, Option<String>), OpReject> {
    verify_op_inner(entry, expected_affair_id, None)
}

fn verify_op_inner(
    entry: &Value,
    expected_affair_id: &str,
    now_ms: Option<i64>,
) -> Result<(AffairOp, Option<String>), OpReject> {
    let op = parse_op(entry)?;
    if op.affair_id != expected_affair_id {
        return Err(OpReject::AffairMismatch);
    }
    if let Some(now_ms) = now_ms {
        if (now_ms - op.declared_at).abs() > OP_FRESHNESS_WINDOW_MS {
            return Err(OpReject::StaleDeclaredAt);
        }
    }
    let payload = op_sign_payload(entry)?;
    if !verify_actor_signature(&op.actor, &payload, &op.sig) {
        return Err(OpReject::InvalidSignature);
    }
    let link_target = validate_payload(op.op_type, &op.payload, expected_affair_id)?;
    Ok((op, link_target))
}

/// 入站结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// 已接纳入有效集。
    Accepted {
        /// 该操作的 opHash。
        op_hash: String,
    },
    /// 已知条目重复入站。
    Duplicate {
        /// 该操作的 opHash。
        op_hash: String,
    },
    /// 暂存待补（prevOpHash / vote / objection 指向未知条目；乱序不拒收）。
    Pending {
        /// 该操作的 opHash。
        op_hash: String,
    },
    /// 拒收（fail-closed）。
    Rejected(OpReject),
}

/// 事务操作日志簿记（纯内存；持久化键见 §3.3，归调用方）。
pub struct OpLog {
    affair_id: String,
    /// 创世 initiator.identity：moderate / meta-revise 的主持人门槛（§11）。
    moderator: String,
    known: HashSet<String>,
    heads: HashSet<String>,
    pending: Vec<Value>,
}

impl OpLog {
    /// 以创世校验产物建簿（affairId + 主持人身份）。
    pub fn new(affair_id: String, moderator: String) -> Self {
        Self {
            affair_id,
            moderator,
            known: HashSet::new(),
            heads: HashSet::new(),
            pending: Vec::new(),
        }
    }

    /// 本地观察到的 DAG 头集合（无后继的条目，§3.3 `affair:head:`）。
    pub fn heads(&self) -> Vec<String> {
        sort_op_hashes(&self.heads.iter().cloned().collect::<Vec<_>>())
    }

    /// 暂存待补条数。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 条目是否已在有效集。
    pub fn contains(&self, op_hash: &str) -> bool {
        self.known.contains(op_hash)
    }

    fn target_known(&self, hash: &str) -> bool {
        hash == self.affair_id || self.known.contains(hash)
    }

    /// 入站一条操作。返回 Accepted/Pending/Rejected（见 §3.2 入站校验）。
    pub fn ingest(&mut self, entry: &Value, now_ms: i64) -> Inbound {
        let outcome = self.try_accept(entry, now_ms);
        if matches!(outcome, Inbound::Accepted { .. }) {
            self.drain_pending(now_ms);
        }
        outcome
    }

    fn try_accept(&mut self, entry: &Value, now_ms: i64) -> Inbound {
        let (op, link_target) = match verify_op(entry, &self.affair_id, now_ms) {
            Ok(v) => v,
            Err(e) => return Inbound::Rejected(e),
        };
        let Ok(op_hash) = compute_op_hash(entry) else {
            return Inbound::Rejected(OpReject::Malformed);
        };
        if self.known.contains(&op_hash) {
            return Inbound::Duplicate { op_hash };
        }
        // §11：moderate / meta-revise 仅主持人（创世 initiator）合法
        if matches!(op.op_type, OpType::Moderate | OpType::MetaRevise)
            && op.actor.identity != self.moderator
        {
            return Inbound::Rejected(OpReject::NotModerator);
        }
        // 因果见证与指向校验：未知则暂存待补（§3.2/§4）
        if !self.target_known(&op.prev_op_hash)
            || link_target.is_some_and(|t| !self.target_known(&t))
        {
            self.pending.push(entry.clone());
            return Inbound::Pending { op_hash };
        }
        self.known.insert(op_hash.clone());
        self.heads.remove(&op.prev_op_hash);
        self.heads.insert(op_hash.clone());
        Inbound::Accepted { op_hash }
    }

    fn drain_pending(&mut self, now_ms: i64) {
        loop {
            let before = self.pending.len();
            let pending = std::mem::take(&mut self.pending);
            for entry in pending {
                // Accepted/Duplicate：出队；Pending：try_accept 已自行重新入队。
                // Rejected 在 drain 阶段不会出现（失效判定只在到达时做），防御性保留。
                if let Inbound::Rejected(_) = self.try_accept(&entry, now_ms) {
                    self.pending.push(entry);
                }
            }
            if self.pending.len() == before {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ancestor_closure_is_backfill_stable() {
        // 树：root ← a ← b（b 为求值根）；后到的旁支 c（root ← c）不进 b 的闭包
        let id = |s: &str| s.repeat(64 / s.len());
        let edges = vec![
            (id("a"), id("rr")), // prev = 创世（不在操作集）
            (id("b"), id("a")),
            (id("c"), id("rr")),
        ];
        let set = ancestor_op_hashes(&id("b"), &edges).unwrap();
        assert!(set.contains(&id("a")) && set.contains(&id("b")));
        assert!(!set.contains(&id("c")));
        // 未知根 → None（fail-closed）
        assert!(ancestor_op_hashes(&id("zz"), &edges).is_none());
    }

    #[test]
    fn exec_report_resolution_is_link_target() {
        // 决议引用纳入指向校验（§4 乱序规则）：引用未知决议 → 暂存而非直接入有效集
        let payload = json!({ "resolution": "ab".repeat(32), "status": "accepted" });
        let link = validate_payload(OpType::ExecReport, &payload, &"cd".repeat(32)).unwrap();
        assert_eq!(link, Some("ab".repeat(32)));
    }
}
