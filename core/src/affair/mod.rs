//! affair（事务容器）纯逻辑模块：创世记录与 affairId 自认证、操作日志 DAG、
//! 规则文档静态检查、集体决策三形态求值、关闭条件确定性求值、决议两态、
//! 事务间引用、名册快照承诺、元数据修订链复算、公开履历跨事务聚合视图
//! （profile.rs）。
//!
//! 字节级权威规格：`wiki/protocol/community/affair.md`（编码总约见同目录
//! README.md；元数据面见 affair-metadata.md）。golden vectors：
//! `code/spec/vectors/community.json`，生成器
//! `code/core/examples/gen_community_affair_vectors.rs`（C1 组）与
//! `gen_community_ladder_vectors.rs`（C6 ladderDerive 组），消费测试
//! `code/core/tests/community_affair_vectors.rs` 与
//! `community_ladder_vectors.rs`。
//!
//! 本模块为纯逻辑层：不碰网络与存储，时间一律以 `now_ms: i64` / `anchored_ms`
//! 参数注入（存证锚定时刻属调用方，§7.2 时间源）。命名红线：实现层一律
//! `affair`，与组织本地审计日志 `org:tx:` 严格分域。
//!
//! ## 规格未逐字钉死处的实现判定（已在对应文件头注说明）
//!
//! - wall-clock 求值：锚定时间越过 `notBefore + 10min` 容忍带才算满足，带内
//!   一律视为未满足（close.rs）；
//! - threshold 关闭条件计入语义：有效 content 操作按操作者身份去重（一人一次），
//!   每人计 opHash 字典序最小的一条（close.rs）；
//! - op-count 的 `filter` 匹配 `payload.kind`（插件命名串，内核不解释）；
//! - 决议公示期否决阈值读 `rules.pubPeriod.vetoThreshold.count`（缺省 1，
//!   §6.2「阈值可由 rules 配置」的落点）；
//! - 决议公示期取规则文档版本（§6.2）：payload 自声明 pubPeriod 入站解析期
//!   即须 ≥24h，复算须与判定所用 rules 版本一致，生效态求值不取自声明值
//!   （resolution.rs / kernel/affair_ops.rs，评审阻塞 1 修复）；
//! - 失联解锁：现行机制 multisig/vote 且 change 触及 `ruleChange` 键时允许
//!   delayed-veto 形态求值（decide.rs，§5.3 硬编码）；
//! - 采纳口径：delayed-veto 生效的 meta-revise / rule-change 操作计入采纳，
//!   vote/multisig 形态不计（ladder.rs，§13 登记）。
//! - 规则版本链 replay：生效时刻判定与 vote 名册注入口径见 rulechain.rs 头注
//!   （evaluate_rule_change 的生产驱动；§5.4「每一版本确定性可溯」落地）；
//! - 「推导至 X 操作（含）」（§9 快照 asOf / §6.1 决议复算操作集合）= X 的
//!   prevOpHash 因果闭包（op.rs `ancestor_op_hashes`，乱序补齐稳定）；
//! - 效力回执（PendingEffect 消费留痕）线形为最小可用自设计，见 effect.rs
//!   `EffectReceipt` 头注。

pub mod actor;
pub mod close;
pub mod decide;
pub mod effect;
pub mod exec;
pub mod genesis;
pub mod keys;
pub mod ladder;
pub mod meta;
pub mod op;
pub mod profile;
pub mod refs;
pub mod resolution;
pub mod rulechain;
pub mod rules;
pub mod snapshot;

pub use actor::{
    Actor, ActorKind, ActorReject, is_valid_identity_id, parse_actor, verify_actor_signature,
};
pub use close::{
    CloseCtx, CountedOp, WALL_CLOCK_TOLERANCE_MS, WallClockEval, evaluate_close_condition,
    evaluate_close_conditions, evaluate_wall_clock,
};
pub use decide::{
    Decision, RuleChangeCtx, RuleChangeOutcome, RuleChangeProposal, VoteBallot,
    evaluate_delayed_veto, evaluate_multisig, evaluate_rule_change, evaluate_vote,
    mechanism_from_rules, parse_rule_change_payload, rule_change_approval_payload, tally_votes,
};
pub use effect::{
    EFFECT_GRANT_PREFIX, EFFECT_RECEIPT_PREFIX, EffectGrant, EffectHookOutcome, EffectReceipt,
    EffectReject, PendingEffect, build_effect_receipt, effect_grant_key, effect_receipt_key,
    encode_effect_scope, evaluate_effect_hook, is_valid_org_id, parse_effect_grant,
    parse_effect_receipt,
};
pub use genesis::{
    AffairGenesis, GenesisReject, compute_affair_id, genesis_sign_payload, parse_genesis,
    verify_genesis,
};
pub use keys::{
    AFFAIR_FOLLOW_PREFIX, AFFAIR_HEAD_PREFIX, AFFAIR_OP_PREFIX, AFFAIR_RECORD_PREFIX,
    affair_follow_key, affair_head_key, affair_op_key, affair_record_key,
};
pub use ladder::{
    AdoptionEvent, DEFAULT_ACTIVE_WINDOW_MS, DEFAULT_CONTRIBUTOR_ACCEPTS, DEFAULT_DECAY_MS,
    DEFAULT_VOTER_ACCEPTS, DEFAULT_VOTER_DAYS_MS, LadderEntry, LadderInput, LadderOpView,
    LadderParams, LadderRoster, LadderTier, account_age_ms, derive_adoptions, derive_ladder,
    earliest_activity_ms,
};
pub use meta::{
    AffairMeta, MetaArbitration, MetaGeneration, MetaReviseEntry, MetaRevisePayload, MetaSeen,
    MetaVerify, TriState, apply_meta_revise, arbitrate_meta, derive_meta_generations,
    parse_meta_revise_payload, verify_meta_announce,
};
pub use op::{
    AffairOp, Inbound, OP_FRESHNESS_WINDOW_MS, OpLog, OpReject, OpType, ancestor_op_hashes,
    compute_op_hash, op_sign_payload, parse_op, parse_op_type, sort_op_hashes, verify_op,
    verify_op_replicated,
};
pub use profile::{
    AffairProfileStats, ProfileAffairView, ProfileVote, PublicProfile, derive_public_profile,
};
pub use refs::{AffairRef, RefReject, RefRel, parse_ref, parse_ref_array, parse_ref_rel};
pub use resolution::{
    ReplayOutcome, ResolutionPayload, ResolutionState, parse_resolution_payload, replay_resolution,
    resolution_state,
};
pub use rulechain::{
    AnchoredBallot, RuleChain, RuleChangeEntry, RuleChangeFate, RulesVersion, replay_rule_chain,
};
pub use rules::{
    CloseCondition, DEFAULT_PUB_PERIOD_MS, Fraction, MIN_PUB_PERIOD_MS, Mechanism, RulesDoc,
    StaticCheckReject, ThresholdBase, apply_rule_patch, parse_close_condition, parse_mechanism,
    rules_hash, static_check_rules,
};
pub use snapshot::{
    SnapshotPayload, member_set_hash, parse_snapshot_payload, roster_hash, verify_ladder_roster,
};
