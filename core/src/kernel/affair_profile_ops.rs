//! 公开履历聚合查询门面（community-affairs §7.3/§10 决策 4：跨事务账号年龄 /
//! 采纳 / 投票历史 = 内核确定性聚合视图，`sdk.affairs.publicProfile` 的内核侧）。
//!
//! 装配口径与 affair_ops 各读路径一致：本机关注事务集合（关注簿记）× 本地已
//! 接受操作 + 本副本存证链锚定时刻（§7.2 时间源，只认链上锚定时间），聚合
//! 本身委托 affair 纯逻辑 [`derive_public_profile`]（乱序安全 / 重放幂等，
//! 同一查询任何节点对同一副本集合复算一致）。
//!
//! 履历是「本地副本所见」的视图：未关注/未复制到的事务不参与聚合（诚实边界，
//! 与 affair-metadata §7 元数据面查询的本地性一致）。本路径不需要创世与规则
//! 链——聚合口径只依赖操作集合 + 锚定时刻 + 异议计数。

use std::collections::HashMap;

use serde_json::{Value, json};

use super::affair_ops::{affair_anchor_map, load_ops};
use super::{Kernel, KernelError, Result};
use crate::affair::{
    LadderOpView, ProfileAffairView, derive_public_profile, is_valid_identity_id, parse_op,
};
use crate::affair::{OpType, ProfileVote};
use crate::p2p::node::system_now_ms;
use crate::sync::affairsync::list_followed_affairs;

fn sync_err(e: crate::sync::SyncError) -> KernelError {
    KernelError::Internal(format!("affairsync: {e}"))
}

impl Kernel {
    /// 公开履历聚合（sdk.affairs.publicProfile）：给定公共身份，跨本机关注的
    /// 全部事务聚合账号年龄 / 提议采纳 / 投票历史（口径见 affair/profile.rs
    /// 头注）。identity 线形校验 fail-closed；无参与事务时返回全零空视图
    /// （诚实空集，非错误）。
    pub fn affair_public_profile(&self, identity: &str) -> Result<Value> {
        if !is_valid_identity_id(identity) {
            return Err(KernelError::Internal("invalid identity".to_string()));
        }
        let storage = self.require_storage()?;
        let followed = list_followed_affairs(storage).map_err(sync_err)?;
        let mut views = Vec::with_capacity(followed.len());
        for affair_id in &followed {
            let raw_ops = load_ops(storage, affair_id)?;
            if raw_ops.is_empty() {
                continue; // 已关注但本地尚无操作副本：不参与聚合
            }
            let anchors = affair_anchor_map(storage, affair_id)?;
            let mut objections: HashMap<String, u64> = HashMap::new();
            let mut ops = Vec::with_capacity(raw_ops.len());
            for (op_hash, value) in raw_ops {
                let Ok(parsed) = parse_op(&value) else {
                    continue; // 入站已全链校验；损坏记录跳过（防御，同 affair_eval）
                };
                if parsed.op_type == OpType::Objection
                    && let Some(target) = parsed
                        .payload
                        .get("target")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                {
                    *objections.entry(target).or_default() += 1;
                }
                ops.push(LadderOpView {
                    anchored_ms: anchors.get(&("ops".to_string(), op_hash.clone())).copied(),
                    objection_count: 0, // 异议索引遍历时同步建立，结束后统一回填
                    op_hash,
                    actor_kind: parsed.actor.kind,
                    actor_identity: parsed.actor.identity.clone(),
                    op_type: parsed.op_type,
                    payload: parsed.payload.clone(),
                });
            }
            for op in &mut ops {
                op.objection_count = objections.get(&op.op_hash).copied().unwrap_or(0);
            }
            views.push(ProfileAffairView {
                affair_id: affair_id.clone(),
                ops,
            });
        }
        let now_ms = system_now_ms();
        let profile = derive_public_profile(identity, &views, now_ms);
        Ok(profile_json(&profile))
    }
}

/// 聚合产物的线形 JSON（camelCase，与 affair 门面各读路径一致；比率不产
/// 浮点——采纳率 = adoptions / proposals，精确计数对交由客户端呈现）。
fn profile_json(profile: &crate::affair::PublicProfile) -> Value {
    let per_affair: Vec<Value> = profile
        .per_affair
        .iter()
        .map(|s| {
            json!({
                "affairId": s.affair_id,
                "opCount": s.op_count,
                "proposals": s.proposals,
                "adoptions": s.adoptions,
                "votes": s.votes,
                "votesYes": s.votes_yes,
                "votesNo": s.votes_no,
                "firstActivityMs": s.first_activity_ms,
                "lastActivityMs": s.last_activity_ms,
            })
        })
        .collect();
    let vote_history: Vec<Value> = profile
        .vote_history
        .iter()
        .map(|v: &ProfileVote| {
            json!({
                "affairId": v.affair_id,
                "opHash": v.op_hash,
                "proposal": v.proposal,
                "choice": if v.yes { "yes" } else { "no" },
                "anchoredMs": v.anchored_ms,
            })
        })
        .collect();
    json!({
        "identity": profile.identity,
        "nowMs": profile.now_ms,
        "affairsParticipated": profile.affairs_participated,
        "firstActivityMs": profile.first_activity_ms,
        "accountAgeMs": profile.account_age_ms,
        "proposals": profile.proposals,
        "adoptions": profile.adoptions,
        "votes": profile.votes,
        "votesYes": profile.votes_yes,
        "votesNo": profile.votes_no,
        "perAffair": per_affair,
        "voteHistory": vote_history,
    })
}

#[cfg(test)]
#[path = "affair_profile_ops_tests.rs"]
mod tests;
