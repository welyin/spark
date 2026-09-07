//! 主持人展示层操作与元数据修订链（wiki/protocol/community/affair.md §11；
//! 公告面口径 wiki/protocol/community/affair-metadata.md §3/§5）。
//!
//! 发起人自动成为事务主持人，权限仅限展示层。meta-revise 生效机制固定为
//! delayed-veto（§11.2 防操纵发现面）；生效后元数据代际 metaSeq+1，
//! `basisOpHash` 指向生效的 meta-revise 操作——收录方可对事务日志复算修订
//! 链合法性（affair-metadata §4 验证分层：verified / unverified / 矛盾丢弃）。

use serde_json::Value;

use super::decide::{Decision, evaluate_delayed_veto};
use super::rules::{Mechanism, parse_mechanism};

/// 三态字段（缺省 = 不变，null = 清除，值 = 设置；同 identity §6 口径）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriState<T> {
    /// 字段未出现：不变。
    Absent,
    /// 显式 null：清除。
    Clear,
    /// 设置为值。
    Set(T),
}

/// meta-revise payload（§11.2）。
#[derive(Clone, Debug, PartialEq)]
pub struct MetaRevisePayload {
    /// 标题修订（三态）。
    pub title: TriState<String>,
    /// 简介修订（三态）。
    pub summary: TriState<String>,
    /// 标签修订（三态）。
    pub tags: TriState<Vec<String>>,
    /// 生效机制（固定 delayed-veto，含其参数）。
    pub mechanism: Mechanism,
    /// 机制原文（canonical 比对用）。
    pub mechanism_raw: Value,
}

/// 解析 meta-revise payload：mechanism 必填且必须为 delayed-veto（§11.2）；
/// 出现的字段按创世同口径校验上界（affair §2.1）；title 是必备元数据，
/// 显式 null（清除语义）在解析期即拒（fail-closed），不得应用期静默吞掉。
pub fn parse_meta_revise_payload(payload: &Value) -> Result<MetaRevisePayload, &'static str> {
    let obj = payload.as_object().ok_or("bad-meta-revise-payload")?;
    let mechanism_raw = obj
        .get("mechanism")
        .cloned()
        .ok_or("bad-meta-revise-payload")?;
    let mechanism = parse_mechanism(&mechanism_raw).map_err(|_| "bad-meta-revise-payload")?;
    if !matches!(mechanism, Mechanism::DelayedVeto { .. }) {
        return Err("meta-revise-not-delayed-veto");
    }
    let title = match obj.get("title") {
        None => TriState::Absent,
        // title 不允许清除：三态下「清除标题」语义不成立，解析期拒绝（评审提示 3）
        Some(Value::Null) => return Err("bad-meta-revise-title"),
        Some(v) => {
            let t = v.as_str().ok_or("bad-meta-revise-payload")?;
            let len = t.trim().encode_utf16().count();
            if !(1..=120).contains(&len) {
                return Err("bad-meta-revise-payload");
            }
            TriState::Set(t.to_string())
        }
    };
    let summary = match obj.get("summary") {
        None => TriState::Absent,
        Some(Value::Null) => TriState::Clear,
        Some(v) => {
            let s = v.as_str().ok_or("bad-meta-revise-payload")?;
            if s.encode_utf16().count() > 1024 {
                return Err("bad-meta-revise-payload");
            }
            TriState::Set(s.to_string())
        }
    };
    let tags = match obj.get("tags") {
        None => TriState::Absent,
        Some(Value::Null) => TriState::Clear,
        Some(v) => {
            let arr = v.as_array().ok_or("bad-meta-revise-payload")?;
            let mut tags = Vec::with_capacity(arr.len());
            for tag in arr {
                let tag = tag.as_str().ok_or("bad-meta-revise-payload")?;
                if tag.encode_utf16().count() > 32 {
                    return Err("bad-meta-revise-payload");
                }
                tags.push(tag.to_string());
            }
            if tags.len() > 16 {
                return Err("bad-meta-revise-payload");
            }
            TriState::Set(tags)
        }
    };
    Ok(MetaRevisePayload {
        title,
        summary,
        tags,
        mechanism,
        mechanism_raw,
    })
}

/// 议题元数据三元组（title/summary/tags——全网索引只收录这三样）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffairMeta {
    /// 标题。
    pub title: String,
    /// 简介。
    pub summary: String,
    /// 标签。
    pub tags: Vec<String>,
}

/// 应用一次生效的 meta-revise（三态；title 不允许清除——解析期已拒绝 null，
/// Clear 分支仅为防御保留）。
pub fn apply_meta_revise(current: &AffairMeta, payload: &MetaRevisePayload) -> AffairMeta {
    let mut next = current.clone();
    match &payload.title {
        TriState::Set(t) => next.title = t.clone(),
        // title 是必备元数据，清除语义不适用（解析期已拒绝 null title）
        TriState::Clear | TriState::Absent => {}
    }
    match &payload.summary {
        TriState::Set(s) => next.summary = s.clone(),
        TriState::Clear => next.summary = String::new(),
        TriState::Absent => {}
    }
    match &payload.tags {
        TriState::Set(t) => next.tags = t.clone(),
        TriState::Clear => next.tags = Vec::new(),
        TriState::Absent => {}
    }
    next
}

/// 元数据代际（§11.2 + affair-metadata §3）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaGeneration {
    /// 代际（创世 = 0，每次生效修订 +1）。
    pub meta_seq: u64,
    /// 生效依据：metaSeq=0 → affairId；metaSeq>0 → 生效的 meta-revise opHash。
    pub basis_op_hash: String,
    /// 该代际生效元数据（全量快照）。
    pub meta: AffairMeta,
}

/// meta-revise 求值输入（已验签入有效集的条目 + 锚定/异议上下文）。
pub struct MetaReviseEntry {
    /// 条目 opHash。
    pub op_hash: String,
    /// 操作者身份。
    pub actor_identity: String,
    /// 解析后的 payload。
    pub payload: MetaRevisePayload,
    /// 锚定时刻（§7.2 时间源，调用方注入）。
    pub anchored_ms: i64,
    /// 有效异议数（指向该条目的 objection）。
    pub objection_count: u64,
}

/// 从日志复算元数据修订链（basisOpHash 链）：创世为第 0 代；仅主持人发起
/// 且 delayed-veto 生效的 meta-revise 递进代际。多条生效修订按
/// （生效时刻 = 锚定 + delayMs，opHash 字典序）确定序应用——与到达顺序无关。
pub fn derive_meta_generations(
    affair_id: &str,
    genesis_meta: &AffairMeta,
    moderator: &str,
    revisions: &[MetaReviseEntry],
    now_ms: i64,
) -> Vec<MetaGeneration> {
    let mut effective: Vec<&MetaReviseEntry> = revisions
        .iter()
        .filter(|entry| {
            entry.actor_identity == moderator
                && match &entry.payload.mechanism {
                    Mechanism::DelayedVeto {
                        delay_ms,
                        veto_count,
                    } => {
                        evaluate_delayed_veto(
                            *delay_ms,
                            *veto_count,
                            entry.objection_count,
                            entry.anchored_ms,
                            now_ms,
                        ) == Decision::Effective
                    }
                    _ => false,
                }
        })
        .collect();
    effective.sort_by(|a, b| {
        let effective_at = |e: &&MetaReviseEntry| match &e.payload.mechanism {
            Mechanism::DelayedVeto { delay_ms, .. } => e.anchored_ms + delay_ms,
            _ => i64::MAX,
        };
        effective_at(a)
            .cmp(&effective_at(b))
            .then_with(|| a.op_hash.cmp(&b.op_hash))
    });
    let mut generations = vec![MetaGeneration {
        meta_seq: 0,
        basis_op_hash: affair_id.to_string(),
        meta: genesis_meta.clone(),
    }];
    for entry in effective {
        let prev = generations
            .last()
            .expect("generations non-empty: gen0 pushed above");
        generations.push(MetaGeneration {
            meta_seq: prev.meta_seq + 1,
            basis_op_hash: entry.op_hash.clone(),
            meta: apply_meta_revise(&prev.meta, &entry.payload),
        });
    }
    generations
}

/// 公告验证三态（affair-metadata §4 验证分层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaVerify {
    /// 公告与日志复算的该代际完全一致。
    Verified,
    /// 无日志副本可复算（纯元数据面）——暂存标注，不得覆盖已验证条目。
    Unverified,
    /// 公告与日志复算矛盾——丢弃并告警。
    Conflict,
}

/// 对公告做修订链复算（affair-metadata §4）。`generations` 为
/// [`derive_meta_generations`] 的产物；无日志副本时传 None → Unverified。
pub fn verify_meta_announce(
    announce_meta_seq: u64,
    announce_basis_op_hash: &str,
    announce_meta: &AffairMeta,
    generations: Option<&[MetaGeneration]>,
) -> MetaVerify {
    let Some(generations) = generations else {
        return MetaVerify::Unverified;
    };
    let Some(generation) = generations.iter().find(|g| g.meta_seq == announce_meta_seq) else {
        return MetaVerify::Conflict;
    };
    if generation.basis_op_hash == announce_basis_op_hash && &generation.meta == announce_meta {
        MetaVerify::Verified
    } else {
        MetaVerify::Conflict
    }
}

/// 暂存区裁决输入（affair-metadata §5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaSeen {
    /// 元数据代际。
    pub meta_seq: u64,
    /// 发布方毫秒（打破平手用）。
    pub updated_at: i64,
    /// 是否已过修订链复算。
    pub verified: bool,
}

/// 暂存区裁决结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaArbitration {
    /// 保留既有条目。
    Keep,
    /// 以来新公告替换。
    Replace,
}

/// 同 affairId 多公告裁决（affair-metadata §5）：verified 条目不被
/// unverified 覆盖；否则 `(metaSeq, updatedAt)` 字典序大者胜（metaSeq 优先）。
pub fn arbitrate_meta(existing: &MetaSeen, incoming: &MetaSeen) -> MetaArbitration {
    if existing.verified && !incoming.verified {
        return MetaArbitration::Keep;
    }
    if (incoming.meta_seq, incoming.updated_at) > (existing.meta_seq, existing.updated_at) {
        MetaArbitration::Replace
    } else {
        MetaArbitration::Keep
    }
}
