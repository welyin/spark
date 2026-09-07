//! 创世记录与 affairId（wiki/protocol/community/affair.md §2）。
//!
//! `affairId = sha256hex(normalizeObject(创世记录剔除 sig))`——自认证：持创世
//! 记录即可复算，无需外部锚；创世记录内任何字段（含 rules/refs）都被 affairId
//! 承诺，事后不可改。canonical 口径沿用 sync-evidence §1（`evidence::canonical`）。
//!
//! 字符数上界（title/summary/tags）按 JS `String.length` 语义计 UTF-16 code
//! unit，与参考实现逐字节对齐。

use serde_json::Value;

use super::actor::{Actor, ActorReject, parse_actor, verify_actor_signature};
use super::refs::{AffairRef, RefReject, parse_ref_array};
use super::rules::{RulesDoc, StaticCheckReject, static_check_rules};
use crate::evidence::{normalize_object, sha256_hex};

/// 事务类型标识形状：`^[A-Za-z0-9_-]+(:[A-Za-z0-9_-]+)*$`，≤64 字符（§2.1）。
fn is_valid_affair_type(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.split(':').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// JS `String.length` 语义的长度（UTF-16 code unit 数）。
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 创世记录解析结果（`rules`/`sig` 之外的字段视图；原文哈希在 Value 上计算）。
#[derive(Clone, Debug, PartialEq)]
pub struct AffairGenesis {
    /// 事务类型标识（插件命名空间，内核不解释）。
    pub affair_type: String,
    /// 标题（trim 后 1–120）。
    pub title: String,
    /// 简介（≤1024；缺省 ""）。
    pub summary: String,
    /// 标签（≤16 个、每个 ≤32；缺省 []）。
    pub tags: Vec<String>,
    /// 发起人（创世签名主体）。
    pub initiator: Actor,
    /// 初始投票者集合（去重后；缺省 = 仅发起人）。
    pub initial_voters: Vec<String>,
    /// 创世即声明的谱系引用。
    pub refs: Vec<AffairRef>,
    /// 发起人本地毫秒（仅展示用；权威时间见规格 §7）。
    pub created_at: i64,
}

/// 创世校验失败原因（reason 字符串稳定，golden vectors 可依赖）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenesisReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// affairV 非 1。
    BadVersion,
    /// type 形状非法。
    BadType,
    /// title trim 后不在 1–120。
    BadTitle,
    /// summary 超 1024。
    BadSummary,
    /// tags 超 16 个或单项超 32。
    BadTags,
    /// initiator 校验失败（内层 reason 见 actor.rs）。
    BadActor(ActorReject),
    /// initialVoters 元素非 64 hex。
    BadInitialVoters,
    /// refs 校验失败（含自指）。
    BadRefs(RefReject),
    /// createdAt 非整数毫秒。
    BadCreatedAt,
    /// 缺 sig 或形状非法。
    MissingSig,
    /// 签名验证失败。
    InvalidSignature,
    /// 规则文档静态检查拒绝（§5.6）。
    RulesRejected(StaticCheckReject),
}

impl GenesisReject {
    /// 稳定 reason 字符串。
    pub fn reason(&self) -> String {
        match self {
            Self::Malformed => "malformed-genesis".to_string(),
            Self::BadVersion => "bad-version".to_string(),
            Self::BadType => "bad-type".to_string(),
            Self::BadTitle => "bad-title".to_string(),
            Self::BadSummary => "bad-summary".to_string(),
            Self::BadTags => "bad-tags".to_string(),
            Self::BadActor(r) => r.reason().to_string(),
            Self::BadInitialVoters => "bad-initial-voters".to_string(),
            Self::BadRefs(r) => r.reason().to_string(),
            Self::BadCreatedAt => "bad-created-at".to_string(),
            Self::MissingSig => "missing-sig".to_string(),
            Self::InvalidSignature => "invalid-signature".to_string(),
            Self::RulesRejected(r) => format!("rules-rejected:{}", r.reason()),
        }
    }
}

/// 创世签名载荷 = `canonical(剔除 sig 的全部字段)`（community README 总约）。
pub fn genesis_sign_payload(record: &Value) -> Result<String, GenesisReject> {
    let obj = record.as_object().ok_or(GenesisReject::Malformed)?;
    let mut sans = obj.clone();
    sans.shift_remove("sig");
    Ok(normalize_object(&Value::Object(sans)))
}

/// affairId 自认证复算（§2.3）。
pub fn compute_affair_id(record: &Value) -> Result<String, GenesisReject> {
    Ok(sha256_hex(&genesis_sign_payload(record)?))
}

/// 结构解析（§2.1 字段约束；不验签、不查自指——自指需先复算 affairId）。
pub fn parse_genesis(record: &Value) -> Result<AffairGenesis, GenesisReject> {
    let obj = record.as_object().ok_or(GenesisReject::Malformed)?;
    if obj.get("affairV").and_then(Value::as_u64) != Some(1) {
        return Err(GenesisReject::BadVersion);
    }
    let affair_type = obj
        .get("type")
        .and_then(Value::as_str)
        .filter(|s| is_valid_affair_type(s))
        .ok_or(GenesisReject::BadType)?
        .to_string();
    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .ok_or(GenesisReject::BadTitle)?;
    let trimmed_len = utf16_len(title.trim());
    if trimmed_len < 1 || trimmed_len > 120 {
        return Err(GenesisReject::BadTitle);
    }
    let summary = match obj.get("summary") {
        None | Some(Value::Null) => String::new(),
        Some(v) => {
            let s = v.as_str().ok_or(GenesisReject::BadSummary)?;
            if utf16_len(s) > 1024 {
                return Err(GenesisReject::BadSummary);
            }
            s.to_string()
        }
    };
    let mut tags = Vec::new();
    if let Some(tags_value) = obj.get("tags") {
        for tag in tags_value.as_array().ok_or(GenesisReject::BadTags)? {
            let tag = tag.as_str().ok_or(GenesisReject::BadTags)?;
            if utf16_len(tag) > 32 {
                return Err(GenesisReject::BadTags);
            }
            tags.push(tag.to_string());
        }
        if tags.len() > 16 {
            return Err(GenesisReject::BadTags);
        }
    }
    let initiator = parse_actor(obj.get("initiator").ok_or(GenesisReject::Malformed)?)
        .map_err(GenesisReject::BadActor)?;
    let mut initial_voters = Vec::new();
    match obj.get("initialVoters") {
        None | Some(Value::Null) => initial_voters.push(initiator.identity.clone()),
        Some(v) => {
            for voter in v.as_array().ok_or(GenesisReject::BadInitialVoters)? {
                let id = voter.as_str().ok_or(GenesisReject::BadInitialVoters)?;
                if !super::actor::is_valid_identity_id(id) {
                    return Err(GenesisReject::BadInitialVoters);
                }
                if !initial_voters.contains(&id.to_string()) {
                    initial_voters.push(id.to_string());
                }
            }
        }
    }
    let refs = parse_ref_array(obj.get("refs"), None).map_err(GenesisReject::BadRefs)?;
    let created_at = obj
        .get("createdAt")
        .and_then(Value::as_i64)
        .ok_or(GenesisReject::BadCreatedAt)?;
    Ok(AffairGenesis {
        affair_type,
        title: title.to_string(),
        summary,
        tags,
        initiator,
        initial_voters,
        refs,
        created_at,
    })
}

/// 创世完整校验链：结构 → affairId 复算 → refs 自指禁令 → 验签 → 规则静态
/// 检查（§5.6：创世拒绝创建）。通过则返回解析结果、affairId 与规则视图。
pub fn verify_genesis(record: &Value) -> Result<(AffairGenesis, String, RulesDoc), GenesisReject> {
    let genesis = parse_genesis(record)?;
    let affair_id = compute_affair_id(record)?;
    // 自指禁令（§10）：创世 refs 不得指向复算出的本事务 id
    if genesis.refs.iter().any(|r| r.target == affair_id) {
        return Err(GenesisReject::BadRefs(RefReject::SelfReference));
    }
    let sig = record
        .get("sig")
        .and_then(Value::as_str)
        .ok_or(GenesisReject::MissingSig)?;
    let payload = genesis_sign_payload(record)?;
    if !verify_actor_signature(&genesis.initiator, &payload, sig) {
        return Err(GenesisReject::InvalidSignature);
    }
    let rules = static_check_rules(record.get("rules").ok_or(GenesisReject::Malformed)?)
        .map_err(GenesisReject::RulesRejected)?;
    Ok((genesis, affair_id, rules))
}
