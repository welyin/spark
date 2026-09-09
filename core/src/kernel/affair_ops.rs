//! affair 门面（community-affairs C9）：插件 SDK `sdk.affairs` 的内核侧最小
//! 命令层（wiki/architecture/community-affairs.md §7.2）。
//!
//! 只做本地副本簿记与确定性推导编排，复用两层既有纯逻辑：
//! - 复制/落库/存证锚定走 affairsync 公共入口（`follow_affair` /
//!   `ingest_local_entries`，C4 实现，本文件不改动其语义）；
//! - 校验/推导走 affair 纯逻辑（`verify_genesis` / `verify_op` 链 /
//!   `replay_rule_chain` / `replay_resolution` / `derive_ladder` /
//!   `derive_exec_states` / `evaluate_effect_hook`）。
//!
//! 关注/取关/读日志/提交操作覆盖关注者副本的本地半集；规则版本、决议、阶梯、
//! 执行状态与组织效力全部从本地已接受操作集合 + 本副本存证链锚定时刻确定性
//! 推导（§7.2 时间源：只认链上锚定时间，未锚定条目不参与时间推导）。
//!
//! 生产求值链（各读路径共享 [`Kernel::affair_eval`] 装配）：
//!
//! - **规则版本**（§5.4）：`affair_read_rules` / 各读路径的规则参数一律取
//!   `replay_rule_chain`（创世规则 + 已生效 rule-change 链）的现行版本，不再只读
//!   创世规则；vote 形态提议的名册 = §9 投票前快照（`resolve_snapshot_roster`
//!   生产化 `verify_ladder_roster`）；
//! - **决议**（§6）：`affair_read_resolution` 逐决议复算（`replay_resolution`，
//!   操作集合 = 决议因果闭包、规则版本按 rulesHash 命中、公示期取命中版本），
//!   复算不符入无效集（state = "invalid"）；
//! - **执行型事务**（§6.2-3）：`affair_read_exec` 给出 `derive_exec_states`
//!   状态机（名册/公示期参数注入）；`affair_submit_op` 对 exec-report 做生产
//!   校验入口（exec 声明 / 执行方绑定 / 决议生效时序）；
//! - **组织效力**（org-genesis §6）：`affair_org_effects` 产出待应用事件并标注
//!   回执状态；`affair_apply_org_effects` 消费事件写回执（`org:effectrcpt:`，
//!   最小可用线形见 effect.rs `EffectReceipt`）并逐条存证。**名册/策略内容的
//!   实际变更不在内核**（决议 tally 是插件语义，内核只承诺字节）——回执即
//!   「该决议对该组织此 scope 已生效」的机器可读凭据，内容应用归插件/组织侧。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::org_sync::OrgSyncRequest;
use super::{Kernel, KernelError, Result};
use crate::affair::exec::{ExecCtx, ExecOpView, derive_exec_states, executor_matches};
use crate::affair::{
    AffairGenesis, AffairOp, AnchoredBallot, CloseCtx, CloseCondition, CountedOp,
    EFFECT_GRANT_PREFIX, EffectHookOutcome, LadderInput, LadderOpView, LadderParams, OpType,
    ResolutionPayload, ResolutionState, RuleChain, RuleChangeEntry, RuleChangeFate, RulesVersion,
    SnapshotPayload, ThresholdBase, affair_head_key, affair_record_key, ancestor_op_hashes,
    build_effect_receipt, compute_op_hash, effect_receipt_key, evaluate_effect_hook,
    is_valid_identity_id, is_valid_org_id, parse_effect_grant, parse_effect_receipt, parse_genesis,
    parse_op, parse_resolution_payload, parse_rule_change_payload, parse_snapshot_payload,
    replay_resolution, replay_rule_chain, resolution_state, roster_hash, verify_ladder_roster,
};
use crate::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence};
use crate::p2p::node::system_now_ms;
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::affairsync::{
    follow_affair, follow_state, ingest_local_entries, list_followed_affairs, unfollow_affair,
};

/// 存证锚定索引键：`{collection}:{id}`（本地查找用，非存储键）。
pub(crate) type AnchorKey = (String, String);

fn sync_err(e: crate::sync::SyncError) -> KernelError {
    KernelError::Internal(format!("affairsync: {e}"))
}

/// 求值装配：一条已接受操作 + 本副本链上锚定时刻（§7.2 时间源）。
struct EvalOp {
    op_hash: String,
    parsed: AffairOp,
    anchored_ms: Option<i64>,
}

/// 决议复算结果（§6.1 复算 + §6.2 两态 + 规则版本命中）。
struct ResolutionEval {
    op_hash: String,
    payload: ResolutionPayload,
    anchored_ms: Option<i64>,
    /// 命中的规则版本（rulesHash 复算定位）。
    version: Option<RulesVersion>,
    /// 复算是否已执行（未锚定决议无链上时间，无法界定「决议开始前」名册/时刻）。
    evaluated: bool,
    /// 复算失败原因（稳定字符串）；None = 通过或未求值。
    replay_reason: Option<String>,
    /// §6.2 两态（未锚定 / 无效 = None）。
    state: Option<ResolutionState>,
}

impl ResolutionEval {
    /// 复算通过（未求值不算通过，也不算判无效——未锚定是诚实中间态）。
    fn valid(&self) -> Option<bool> {
        if self.evaluated {
            Some(self.replay_reason.is_none())
        } else {
            None
        }
    }
}

/// 门面求值上下文：一次装载供各读路径复用。
struct AffairEval {
    affair_id: String,
    genesis: AffairGenesis,
    ops: Vec<EvalOp>,
    anchors: HashMap<AnchorKey, i64>,
    now_ms: i64,
    /// (opHash, prevOpHash) 边集（因果闭包推导用）。
    edges: Vec<(String, String)>,
    /// 有效异议计数索引（objection.target → 计数）。
    objections: HashMap<String, u64>,
    /// 阶梯参数：取「非 vote 形态规则修改链」（backbone）现行版本——vote 形态
    /// 修改 ladderParams 不追溯进名册推导（断开 链→名册→参数 循环，
    /// rulechain.rs 头注登记）。
    ladder_params: LadderParams,
    /// 规则链 replay 终态（vote 形态名册注入后的全形态）。
    chain: RuleChain,
}

impl AffairEval {
    /// 创世锚定时刻。
    fn genesis_anchored_ms(&self) -> Option<i64> {
        self.anchors
            .get(&("genesis".to_string(), self.affair_id.clone()))
            .copied()
    }

    /// 指定操作集合切口（None = 全集）+ 推导时刻的阶梯投票者名册（§13 口径）。
    fn voter_identities(&self, cut: Option<&HashSet<String>>, now_ms: i64) -> Vec<String> {
        let views: Vec<LadderOpView> = self
            .ops
            .iter()
            .filter(|op| cut.is_none_or(|c| c.contains(&op.op_hash)))
            .map(|op| LadderOpView {
                op_hash: op.op_hash.clone(),
                actor_kind: op.parsed.actor.kind,
                actor_identity: op.parsed.actor.identity.clone(),
                op_type: op.parsed.op_type,
                payload: op.parsed.payload.clone(),
                anchored_ms: op.anchored_ms,
                objection_count: self.objections.get(&op.op_hash).copied().unwrap_or(0),
            })
            .collect();
        crate::affair::derive_ladder(&LadderInput {
            initial_voters: &self.genesis.initial_voters,
            genesis_anchored_ms: self.genesis_anchored_ms(),
            ops: &views,
            params: self.ladder_params,
            now_ms,
        })
        .voter_identities()
    }

    /// 「推导至 as_of 操作（含）」的名册与推导时刻（§9）：切口 = as_of 的
    /// prevOpHash 因果闭包（op.rs `ancestor_op_hashes`，乱序补齐稳定）；
    /// 推导时刻 = as_of 锚定时刻。as_of == affairId 表示创世切口（空操作集，
    /// 推导时刻 = 创世锚定）。as_of 未知/未锚定 → None（fail-closed）。
    fn derive_roster_as_of(&self, as_of: &str) -> Option<(Vec<String>, i64)> {
        let (cut, now_ms) = if as_of == self.affair_id {
            (HashSet::new(), self.genesis_anchored_ms()?)
        } else {
            let as_op = self.ops.iter().find(|op| op.op_hash == as_of)?;
            (
                ancestor_op_hashes(as_of, &self.edges)?,
                as_op.anchored_ms?,
            )
        };
        Some((self.voter_identities(Some(&cut), now_ms), now_ms))
    }

    /// 解析 §9 阶梯快照名册（verify_ladder_roster 生产调用）：确定性推导至
    /// asOf 的投票者集合，复算 rosterHash 比对承诺（哈希承诺防编造，不符 →
    /// None，fail-closed）。org-roster 形态的成员集内容须由组织侧提供，本门面
    /// 暂无生产来源 → None（如实标注的缺口）。
    fn resolve_snapshot_roster(&self, snapshot_op_hash: &str) -> Option<Vec<String>> {
        let op = self.ops.iter().find(|op| op.op_hash == snapshot_op_hash)?;
        if op.parsed.op_type != OpType::Snapshot {
            return None;
        }
        let payload = parse_snapshot_payload(&op.parsed.payload).ok()?;
        let SnapshotPayload::Ladder { as_of, .. } = &payload else {
            return None; // org-roster 形态：成员集内容无本地生产来源
        };
        let (identities, _) = self.derive_roster_as_of(as_of)?;
        verify_ladder_roster(&payload, &identities).then_some(identities)
    }

    /// vote 形态提议的名册（任务 2 线上化）：锚定不晚于提议锚定的最近一条
    /// ladder 快照（平局取 opHash 大者），复算其名册；无可用快照 → None
    /// （fail-closed，rulechain 侧恒待定）。
    fn resolve_vote_roster(&self, proposal_anchored_ms: Option<i64>) -> Option<Vec<String>> {
        let proposal_anchor = proposal_anchored_ms?;
        let mut best: Option<&EvalOp> = None;
        for op in &self.ops {
            if op.parsed.op_type != OpType::Snapshot {
                continue;
            }
            let Some(anchor) = op.anchored_ms else {
                continue;
            };
            if anchor > proposal_anchor {
                continue; // 投票前快照：晚于提议的快照不作基数
            }
            let better = match best {
                None => true,
                Some(current) => {
                    (anchor, op.op_hash.as_str())
                        > (current.anchored_ms.unwrap_or(i64::MIN), current.op_hash.as_str())
                }
            };
            if better {
                best = Some(op);
            }
        }
        self.resolve_snapshot_roster(&best?.op_hash)
    }

    /// 构造 rule-change 求值条目（阶段 A 无名册；阶段 B 注入投票前快照名册）。
    fn rule_change_entries(&self, with_rosters: bool) -> Vec<RuleChangeEntry> {
        self.ops
            .iter()
            .filter(|op| op.parsed.op_type == OpType::RuleChange)
            .filter_map(|op| {
                let proposal = parse_rule_change_payload(&op.parsed.payload).ok()?;
                let mut entry = RuleChangeEntry {
                    op_hash: op.op_hash.clone(),
                    prev_op_hash: op.parsed.prev_op_hash.clone(),
                    proposal,
                    anchored_ms: op.anchored_ms,
                    objection_count: self.objections.get(&op.op_hash).copied().unwrap_or(0),
                    ballots: Vec::new(),
                    roster_size: None,
                };
                if with_rosters
                    && matches!(
                        entry.proposal.mechanism,
                        crate::affair::Mechanism::Vote { .. }
                ) {
                    // 投票前快照名册：资格过滤 + 基数（§5.3 vote 形态判定）
                    let roster = self.resolve_vote_roster(entry.anchored_ms);
                    if let Some(roster) = &roster {
                        entry.ballots = self
                            .ops
                            .iter()
                            .filter(|vote| vote.parsed.op_type == OpType::Vote)
                            .filter(|vote| {
                                vote.parsed.payload.get("proposal").and_then(Value::as_str)
                                    == Some(op.op_hash.as_str())
                            })
                            .filter(|vote| roster.contains(&vote.parsed.actor.identity))
                            .map(|vote| AnchoredBallot {
                                op_hash: vote.op_hash.clone(),
                                voter: vote.parsed.actor.identity.clone(),
                                yes: vote.parsed.payload.get("choice").and_then(Value::as_str)
                                    == Some("yes"),
                                anchored_ms: vote.anchored_ms,
                            })
                            .collect();
                    }
                    entry.roster_size = roster.map(|r| r.len() as u64);
                }
                Some(entry)
            })
            .collect()
    }

    /// 逐决议复算（§6.1）：规则版本按 rulesHash 命中；复算操作集合 = 决议
    /// 因果闭包（剔除决议自身）；求值上下文（快照基数/阶梯基数/锚定时刻）
    /// 按条件形态注入。未锚定决议求值跳过（evaluated = false）。
    fn eval_resolutions(&self) -> Vec<ResolutionEval> {
        self.ops
            .iter()
            .filter(|op| op.parsed.op_type == OpType::Resolution)
            .filter_map(|op| {
                let payload = parse_resolution_payload(&op.parsed.payload).ok()?;
                Some(self.eval_resolution(op, payload))
            })
            .collect()
    }

    fn eval_resolution(&self, op: &EvalOp, payload: ResolutionPayload) -> ResolutionEval {
        let mut out = ResolutionEval {
            op_hash: op.op_hash.clone(),
            payload,
            anchored_ms: op.anchored_ms,
            version: None,
            evaluated: false,
            replay_reason: None,
            state: None,
        };
        // 规则版本命中（§6.1 rulesHash = 判定所用规则文档版本）
        let Some(version) = self.chain.version_by_hash(&out.payload.rules_hash) else {
            out.evaluated = true;
            out.replay_reason = Some("rules-hash-mismatch".to_string());
            return out;
        };
        // 版本生效时刻不得晚于决议锚定（规则须在决议作出时已生效）
        if let (Some(effective), Some(anchored)) = (version.effective_ms, op.anchored_ms)
            && effective > anchored
        {
            out.evaluated = true;
            out.replay_reason = Some("rules-version-not-in-effect".to_string());
            return out;
        }
        out.version = Some(version.clone());
        let Some(anchored) = op.anchored_ms else {
            return out; // 未锚定：诚实中间态，不求值
        };
        out.evaluated = true;
        // 条件须为命中版本内声明的关闭条件（§6.1「被满足的 §5.2 关闭条件原文」）
        let Ok(condition) = crate::affair::parse_close_condition(&out.payload.condition) else {
            out.replay_reason = Some("bad-condition".to_string());
            return out;
        };
        // 链上版本构建期已过静态检查（replay_rule_chain 把关），此处为防御分支
        let Ok(version_doc) = crate::affair::static_check_rules(&version.rules) else {
            out.replay_reason = Some("rules-version-corrupt".to_string());
            return out;
        };
        if !version_doc.close_conditions.contains(&condition) {
            out.replay_reason = Some("condition-not-in-rules".to_string());
            return out;
        }
        // 复算操作集合 = 决议因果闭包（剔除决议自身）
        let mut cut = ancestor_op_hashes(&op.op_hash, &self.edges).unwrap_or_default();
        cut.remove(&op.op_hash);
        let valid_ops: Vec<CountedOp> = self
            .ops
            .iter()
            .filter(|view| cut.contains(&view.op_hash))
            .map(|view| CountedOp {
                op_hash: view.op_hash.clone(),
                op_type: view.parsed.op_type,
                actor_identity: view.parsed.actor.identity.clone(),
                payload: view.parsed.payload.clone(),
            })
            .collect();
        let ctx = self.close_ctx_for(&condition, &op.op_hash, anchored);
        let outcome = replay_resolution(&version.rules, &out.payload, &valid_ops, &ctx);
        if let Some(reason) = outcome.reason() {
            out.replay_reason = Some(reason.to_string());
            return out;
        }
        // 生效态求值（§6.2）：公示期/否决阈值取命中规则版本
        let (pub_period_ms, veto_count) = rules_pub_params(&version.rules);
        out.state = Some(resolution_state(
            anchored,
            pub_period_ms,
            self.objections.get(&op.op_hash).copied().unwrap_or(0),
            veto_count,
            self.now_ms,
        ));
        out
    }

    /// 决议复算的求值上下文（任务 2 注入）：wall-clock 锚定 = 决议锚定时刻；
    /// threshold 基数按 base 注入——snapshot 形态复算名册大小，ladder:voters
    /// 形态以决议因果闭包 + 决议锚定时刻推导（「决议开始前」名册）。
    fn close_ctx_for(&self, condition: &CloseCondition, res_hash: &str, res_anchor: i64) -> CloseCtx {
        let mut ctx = CloseCtx {
            snapshot_roster_size: None,
            ladder_voters_size: None,
            anchored_ms: Some(res_anchor),
        };
        if let CloseCondition::Threshold { base, .. } = condition {
            match base {
                ThresholdBase::Snapshot(snapshot_hash) => {
                    ctx.snapshot_roster_size = self
                        .resolve_snapshot_roster(snapshot_hash)
                        .map(|roster| roster.len() as u64);
                }
                ThresholdBase::LadderVoters => {
                    let mut cut =
                        ancestor_op_hashes(res_hash, &self.edges).unwrap_or_default();
                    cut.remove(res_hash);
                    ctx.ladder_voters_size =
                        Some(self.voter_identities(Some(&cut), res_anchor).len() as u64);
                }
            }
        }
        ctx
    }
}

impl Kernel {
    /// affair 域即时反熵触发（affair-sync §7）：关注/本地事务写入后经
    /// org-sync worker 向该事务目录中已连接的关注者发 affairsync-hello
    /// （对端按 diff 回 need/推 data 收敛）。p2p 未启动时 org_sync_tx 为
    /// None 静默跳过——收敛由对端上线后的 tick S4 周期 hello 兜底。
    fn affair_hello_now(&self, affair_id: &str) {
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::AffairHello {
                affair_id: Some(affair_id.to_string()),
            });
        }
    }

    /// 关注事务（sdk.affairs.follow）：创世记录全链校验（含 §5.6 静态检查），
    /// 落关注簿记并把创世作为本地首条记录自举（存证锚定 + pmeta，与 C4
    /// 复制面落库同路径）。affairId 由创世记录自认证复算，不信调用方自报。
    pub fn affair_follow(&mut self, genesis: &Value) -> Result<String> {
        let (_, affair_id, _) = crate::affair::verify_genesis(genesis)
            .map_err(|e| KernelError::Internal(e.reason()))?;
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        let storage = self.require_storage_raw_mut()?;
        follow_affair(storage, &affair_id, now).map_err(sync_err)?;
        ingest_local_entries(
            storage,
            &node_id,
            &affair_id,
            std::slice::from_ref(genesis),
            now,
        )
        .map_err(sync_err)?;
        // 关注即副本（affair-sync §5）：关注后主动向已知关注者交换摘要——
        // 本机多为空副本，hello 触发对端回推 data 完成首轮收敛
        self.affair_hello_now(&affair_id);
        // sdk.affairs.onChange 事件源（变更通知非可靠队列，插件重读收敛）
        let _ = self.event_tx.send(crate::p2p::P2pEvent::AffairChanged(json!({
            "affairId": affair_id,
            "change": "followed",
        })));
        Ok(affair_id)
    }

    /// 取关事务（sdk.affairs.unfollow）：只删关注簿记（本地键），保留已复制
    /// 数据（affair-sync §5 口径：取关不删除数据）。
    pub fn affair_unfollow(&mut self, affair_id: &str) -> Result<()> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        unfollow_affair(self.require_storage_raw_mut()?, affair_id).map_err(sync_err)?;
        // sdk.affairs.onChange 事件源
        let _ = self.event_tx.send(crate::p2p::P2pEvent::AffairChanged(json!({
            "affairId": affair_id,
            "change": "unfollowed",
        })));
        Ok(())
    }

    /// 本机关注的事务 id 列表（sdk.affairs.listFollowed，字典序）。
    pub fn affair_list_followed(&self) -> Result<Vec<String>> {
        Ok(list_followed_affairs(self.require_storage()?).map_err(sync_err)?)
    }

    /// 提交一条操作（sdk.affairs.submitOp）：实时提交入站链（verify_op 结构/
    /// affairId/declaredAt 新鲜度/验签/payload + §11 主持人门槛 + 因果见证，
    /// 未知指向持久暂存待补；复制面入站同链但豁免新鲜度，affair.md §3.1）。
    /// exec-report 先过生产校验入口（§6.3，见 [`Self::gate_exec_report`]）。
    /// 须先关注该事务。返回 opHash 与判定状态（accepted / pending / duplicate）。
    pub fn affair_submit_op(&mut self, op: &Value) -> Result<Value> {
        let affair_id = op
            .get("affairId")
            .and_then(Value::as_str)
            .filter(|s| is_valid_identity_id(s))
            .ok_or_else(|| KernelError::Internal("invalid affairId".to_string()))?
            .to_string();
        let op_hash = compute_op_hash(op)
            .map_err(|e| KernelError::Internal(format!("malformed op: {}", e.reason())))?;
        // exec-report 生产校验入口（结构解析失败的操作交给入站链给出稳定 reason）
        if let Ok(parsed) = parse_op(op)
            && parsed.op_type == OpType::ExecReport
        {
            self.gate_exec_report(&affair_id, &parsed)?;
        }
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        let summary = ingest_local_entries(
            self.require_storage_raw_mut()?,
            &node_id,
            &affair_id,
            std::slice::from_ref(op),
            now,
        )
        .map_err(sync_err)?;
        let status = if summary.accepted > 0 {
            "accepted"
        } else if summary.pending > 0 {
            "pending"
        } else if summary.duplicates > 0 {
            "duplicate"
        } else {
            // 透出拒收原因（OpReject 稳定 reason，插件侧可诊断 fresh/验签/暂存）；
            // 本地入站链中 verify_op 全过仍被拒只剩 §11 主持人门槛（评审提示 2）
            let detail = crate::affair::verify_op(op, &affair_id, now)
                .err()
                .map(|e| e.reason())
                .unwrap_or_else(|| "not-moderator".to_string());
            return Err(KernelError::Internal(format!(
                "affair op rejected: {affair_id}: {detail}"
            )));
        };
        // 本地事务写入后即时反熵（affair-sync §7）：向已连接关注者发
        // affairsync-hello，对端落后即回 need 拉走本条操作（秒级传播；
        // duplicate/pending 也发——暂存条目的因果见证可能正是对端缺的）
        self.affair_hello_now(&affair_id);
        // sdk.affairs.onChange 事件源（accepted/pending/duplicate 如实透出）
        let _ = self.event_tx.send(crate::p2p::P2pEvent::AffairChanged(json!({
            "affairId": affair_id,
            "change": "submitted",
            "opHash": op_hash.clone(),
            "status": status,
        })));
        Ok(json!({ "affairId": affair_id, "opHash": op_hash, "status": status }))
    }

    /// exec-report 生产校验入口（§6.3）：执行型事务声明存在（现行规则版本
    /// exec ≠ null）→ 执行方绑定（声明身份/公钥/orgSig 形态）→ 引用决议已
    /// 生效（复算有效 + §6.2 生效态）。任一不过即拒（fail-closed；生效前发出
    /// 的回报在推导层本即永久惰性，exec.rs 头注）。复制面入站不走本入口
    /// （签名校验后由推导层惰性口径兜底，乱序/重放同输出）。
    fn gate_exec_report(&self, affair_id: &str, parsed: &AffairOp) -> Result<()> {
        let reject = |reason: &str| {
            Err(KernelError::Internal(format!(
                "affair op rejected: {affair_id}: {reason}"
            )))
        };
        let eval = self.affair_eval(affair_id)?;
        let exec_raw = eval
            .chain
            .doc
            .raw
            .get("exec")
            .cloned()
            .unwrap_or(Value::Null);
        if exec_raw.is_null() {
            return reject("exec-not-declared");
        }
        let decl = crate::affair::exec::parse_exec_decl(&exec_raw)
            .map_err(|e| KernelError::Internal(format!("corrupted exec decl: {}", e.reason())))?;
        if !executor_matches(&decl.executor, &parsed.actor) {
            return reject("not-executor");
        }
        let payload = crate::affair::exec::parse_exec_report_payload(&parsed.payload)
            .map_err(|e| KernelError::Internal(format!("affair op rejected: {affair_id}: {e}")))?;
        // 引用决议未知 → 放行给入站链按 §4 乱序规则暂存待补（本地不可判定不拒收）；
        // 已知但未生效/复算无效 → 拒（生效前回报在推导层本即永久惰性，fail-closed）
        let resolutions = eval.eval_resolutions();
        let Some(re) = resolutions.iter().find(|re| re.op_hash == payload.resolution) else {
            return Ok(());
        };
        if !(re.valid() == Some(true) && re.state == Some(ResolutionState::Effective)) {
            return reject("resolution-not-effective");
        }
        Ok(())
    }

    /// 读本事务本地副本的操作日志（sdk.affairs.readLog）：创世记录 + 已接受
    /// 操作（opHash 字典序，§8 排序键）+ DAG 头 + 关注状态。未关注的已知
    /// 事务也可读（数据已在本地）；完全未知返回空日志。
    pub fn affair_read_log(&self, affair_id: &str) -> Result<Value> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let storage = self.require_storage()?;
        let genesis = storage
            .get(&affair_record_key(affair_id))?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let ops = load_ops(storage, affair_id)?;
        let heads = storage
            .get(&affair_head_key(affair_id))?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| {
                value.get("heads").and_then(Value::as_array).map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        let followed_at = follow_state(storage, affair_id)
            .map_err(sync_err)?
            .map(|state| state.followed_at);
        Ok(json!({
            "affairId": affair_id,
            "genesis": genesis,
            "ops": ops.iter().map(|(hash, op)| json!({ "opHash": hash, "op": op })).collect::<Vec<_>>(),
            "heads": heads,
            "followedAt": followed_at,
        }))
    }

    /// 读规则文档版本链（规则 replay 的读出口）：现行版本 = 创世规则 + 已生效
    /// rule-change 链（§5.4「每一版本确定性可溯」，evaluate_rule_change 生产
    /// 驱动）。逐版本给 seq/依据/哈希/生效时刻；未生效提议给归宿（pending /
    /// rejected + 稳定 reason）。
    pub fn affair_read_rules(&self, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval(affair_id)?;
        let versions: Vec<Value> = eval
            .chain
            .versions
            .iter()
            .map(|v| {
                json!({
                    "seq": v.seq,
                    "basisOpHash": v.basis_op_hash,
                    "rulesHash": v.rules_hash,
                    "effectiveMs": v.effective_ms,
                })
            })
            .collect();
        let changes: Vec<Value> = eval
            .chain
            .fates
            .iter()
            .map(|(op_hash, fate)| {
                let (fate_str, reason) = match fate {
                    RuleChangeFate::Pending(reason) => ("pending", *reason),
                    RuleChangeFate::Rejected(reason) => ("rejected", reason.as_str()),
                };
                json!({ "opHash": op_hash, "fate": fate_str, "reason": reason })
            })
            .collect();
        let current = eval.chain.current();
        Ok(json!({
            "affairId": affair_id,
            "nowMs": eval.now_ms,
            "current": {
                "seq": current.seq,
                "rulesHash": current.rules_hash,
                "rules": current.rules,
            },
            "versions": versions,
            "changes": changes,
        }))
    }

    /// 读决议（sdk.affairs.readResolution）：日志中 opType=resolution 的操作，
    /// 逐个复算（§6.1：操作集合 = 决议因果闭包；规则版本按 rulesHash 命中，
    /// 公示期/否决阈值取命中版本）并给出公示期状态（§6.2）。复算不符 →
    /// state = "invalid"（入无效集，可告警——valid/replay 字段给出机器可读
    /// 原因）。时间权威 = 本副本存证链锚定时刻（§7.2）：未锚定的决议按
    /// "unanchored" 如实返回且不复算，不得用声明时间冒充链上时间。
    pub fn affair_read_resolution(&self, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval(affair_id)?;
        let mut resolutions = Vec::new();
        for re in eval.eval_resolutions() {
            let (pub_period_ms, _) = re
                .version
                .as_ref()
                .map(|v| rules_pub_params(&v.rules))
                .unwrap_or((crate::affair::DEFAULT_PUB_PERIOD_MS, 1));
            let replay = match (re.evaluated, &re.replay_reason) {
                (false, _) => "not-evaluated-unanchored".to_string(),
                (true, None) => "ok".to_string(),
                (true, Some(reason)) => reason.clone(),
            };
            let state = if re.valid() == Some(false) {
                "invalid"
            } else {
                match re.state {
                    None => "unanchored",
                    Some(ResolutionState::Pending) => "pending",
                    Some(ResolutionState::Effective) => "effective",
                    Some(ResolutionState::Vetoed) => "vetoed",
                }
            };
            resolutions.push(json!({
                "opHash": re.op_hash,
                "result": re.payload.result,
                "condition": re.payload.condition,
                "countedOps": re.payload.counted_ops,
                "rulesHash": re.payload.rules_hash,
                "rulesSeq": re.version.as_ref().map(|v| v.seq),
                "pubPeriodMs": re.version.as_ref().map(|_| pub_period_ms),
                "declaredPubPeriodMs": re.payload.pub_period_ms,
                "anchoredMs": re.anchored_ms,
                "objections": eval.objections.get(&re.op_hash).copied().unwrap_or(0),
                "replay": replay,
                "valid": re.valid(),
                "state": state,
            }));
        }
        Ok(json!({ "affairId": affair_id, "resolutions": resolutions }))
    }

    /// 阶梯/账龄状态（sdk.affairs.ladderStatus）：从本地已接受操作集合 +
    /// 链上锚定时刻按 §13 口径确定性推导（初始投票者来自创世，参数取规则链
    /// replay 的 backbone 现行版本——vote 形态规则修改不追溯进名册推导，
    /// 缺省产品默认值）。
    pub fn affair_ladder_status(&self, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval(affair_id)?;
        let views: Vec<LadderOpView> = eval
            .ops
            .iter()
            .map(|op| LadderOpView {
                op_hash: op.op_hash.clone(),
                actor_kind: op.parsed.actor.kind,
                actor_identity: op.parsed.actor.identity.clone(),
                op_type: op.parsed.op_type,
                payload: op.parsed.payload.clone(),
                anchored_ms: op.anchored_ms,
                objection_count: eval.objections.get(&op.op_hash).copied().unwrap_or(0),
            })
            .collect();
        let roster = crate::affair::derive_ladder(&LadderInput {
            initial_voters: &eval.genesis.initial_voters,
            genesis_anchored_ms: eval.genesis_anchored_ms(),
            ops: &views,
            params: eval.ladder_params,
            now_ms: eval.now_ms,
        });
        let entries: Vec<Value> = roster
            .entries
            .iter()
            .map(|entry| {
                let account_age_ms =
                    crate::affair::account_age_ms(&entry.identity, &views, eval.now_ms);
                json!({
                    "identity": entry.identity,
                    "tier": entry.tier.as_str(),
                    "accepts": entry.accepts,
                    "tierSinceMs": entry.tier_since_ms,
                    "lastActivityMs": entry.last_activity_ms,
                    "accountAgeMs": account_age_ms,
                })
            })
            .collect();
        Ok(json!({
            "affairId": affair_id,
            "nowMs": eval.now_ms,
            "entries": entries,
            "voters": roster.voter_identities(),
        }))
    }

    /// 阶梯快照 payload 生产助手（§9「投票前快照」的获取侧）：从本地日志
    /// 确定性推导至 asOf（缺省 = 当前最大 opHash，无操作 = 创世切口）的投票者
    /// 名册，计算 rosterHash。返回 payload 供插件签名后以 snapshot 操作提交；
    /// 附名册原文（透明呈现）。asOf 未锚定 → 报错（fail-closed：推导时刻无从
    /// 定义）。注意：副本后续乱序补齐因果闭包内的操作会使复算名册漂移、
    /// rosterHash 失配——快照即失效（fail-closed），这是 §9 哈希承诺的诚实边界。
    pub fn affair_snapshot_payload(&self, affair_id: &str, as_of: Option<&str>) -> Result<Value> {
        let eval = self.affair_eval(affair_id)?;
        let as_of = match as_of {
            Some(as_of) => {
                if !is_valid_identity_id(as_of) {
                    return Err(KernelError::Internal("invalid asOf opHash".to_string()));
                }
                as_of.to_string()
            }
            None => eval
                .ops
                .iter()
                .map(|op| op.op_hash.clone())
                .max()
                .unwrap_or_else(|| affair_id.to_string()),
        };
        let (roster, as_of_anchored_ms) = eval.derive_roster_as_of(&as_of).ok_or_else(|| {
            KernelError::Internal(format!("asOf unknown or unanchored: {as_of}"))
        })?;
        let hash = roster_hash(&roster).map_err(|e| KernelError::Internal(e.to_string()))?;
        Ok(json!({
            "affairId": affair_id,
            "payload": { "basis": "ladder", "asOf": as_of, "rosterHash": hash },
            "roster": roster,
            "asOfAnchoredMs": as_of_anchored_ms,
        }))
    }

    /// 执行状态读路径（§6.2-3 状态机生产驱动）：现行规则版本 exec == null 的
    /// 事务如实返回空状态集（决议即终态）；否则逐决议推导执行状态
    /// （derive_exec_states 注入现行公示期参数与当前阶梯投票者名册）。复算
    /// 无效的决议入无效集，不参与执行状态机。
    pub fn affair_read_exec(&self, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval(affair_id)?;
        let exec_raw = eval
            .chain
            .doc
            .raw
            .get("exec")
            .cloned()
            .unwrap_or(Value::Null);
        if exec_raw.is_null() {
            return Ok(json!({
                "affairId": affair_id,
                "nowMs": eval.now_ms,
                "exec": Value::Null,
                "states": [],
            }));
        }
        let decl = crate::affair::exec::parse_exec_decl(&exec_raw)
            .map_err(|e| KernelError::Internal(format!("corrupted exec decl: {}", e.reason())))?;
        let invalid: HashSet<String> = eval
            .eval_resolutions()
            .iter()
            .filter(|re| re.valid() == Some(false))
            .map(|re| re.op_hash.clone())
            .collect();
        let views: Vec<ExecOpView> = eval
            .ops
            .iter()
            .filter(|op| !(op.parsed.op_type == OpType::Resolution && invalid.contains(&op.op_hash)))
            .map(|op| ExecOpView {
                op_hash: op.op_hash.clone(),
                actor: op.parsed.actor.clone(),
                op_type: op.parsed.op_type,
                payload: op.parsed.payload.clone(),
                anchored_ms: op.anchored_ms,
            })
            .collect();
        // vote 核查名册 = 当前阶梯投票者集合（全量切口、求值时刻）
        let roster = eval.voter_identities(None, eval.now_ms);
        let ctx = ExecCtx {
            exec: &decl,
            pub_period_ms: eval.chain.doc.pub_period_ms,
            pub_period_veto_count: eval.chain.doc.pub_period_veto_count,
            roster: Some(&roster),
            now_ms: eval.now_ms,
        };
        let states: Vec<Value> = derive_exec_states(&views, &ctx)
            .iter()
            .map(|state| {
                json!({
                    "resolutionOpHash": state.resolution_op_hash,
                    "state": state.state.as_str(),
                    "reportOpHash": state.report_op_hash,
                    "anchoredMs": state.anchored_ms,
                    "effectiveMs": state.effective_ms,
                })
            })
            .collect();
        Ok(json!({
            "affairId": affair_id,
            "nowMs": eval.now_ms,
            "exec": exec_raw,
            "rosterSize": roster.len(),
            "states": states,
        }))
    }

    /// 决议组织效力钩子（org-genesis §6；community-affairs §7.1）：对
    /// (orgId, affairId) 逐条事先声明 × 逐条有效决议求值三线（事先声明存在 /
    /// 决议生效 / 声明存证严格早于决议锚定），产出待应用事件列表并标注回执
    /// 状态（`affair_apply_org_effects` 的消费留痕）。复算无效的决议不产生
    /// 效力（§6.1 无效集），在 invalidResolutions 如实列出。
    /// 声明的存证锚定时刻 = 存证链上 payloadHash 与现行声明记录逐字节一致
    /// 的最早 put 条目时刻（声明键 append 语义：历史覆盖在链上留痕，现行
    /// 值即最新声明）。本函数只产出事件，不做名册/策略应用。
    pub fn affair_org_effects(&self, org_id: &str, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval_for_org(org_id, affair_id)?;
        let (rows, invalid) = self.compute_effect_rows(&eval, org_id)?;
        let storage = self.require_storage()?;
        let mut effects = Vec::new();
        for row in &rows {
            let (outcome_str, pending) = match &row.outcome {
                EffectHookOutcome::Apply(event) => (
                    "apply",
                    Some(json!({
                        "orgId": event.org_id,
                        "affairId": event.affair_id,
                        "resolutionOpHash": event.resolution_op_hash,
                        "scope": event.scope,
                        "grantKey": event.grant_key,
                    })),
                ),
                EffectHookOutcome::NotDeclared => ("notDeclared", None),
                EffectHookOutcome::Revoked => ("revoked", None),
                EffectHookOutcome::ResolutionNotEffective => ("resolutionNotEffective", None),
                EffectHookOutcome::GrantNotAnchored => ("grantNotAnchored", None),
                EffectHookOutcome::ResolutionNotAnchored => ("resolutionNotAnchored", None),
                EffectHookOutcome::NotPrior => ("notPrior", None),
            };
            let mut entry = json!({
                "scope": row.scope,
                "grantKey": row.grant_key,
                "resolutionOpHash": row.resolution_op_hash,
                "outcome": outcome_str,
            });
            if let Some(pending) = pending {
                entry["pendingEffect"] = pending;
                // 回执状态标注（消费编排读口）：同键回执且决议一致 = recorded
                if let Ok(receipt_key) =
                    effect_receipt_key(org_id, affair_id, &row.scope)
                {
                    let recorded = storage
                        .get(&receipt_key)?
                        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                        .and_then(|value| parse_effect_receipt(&value).ok())
                        .is_some_and(|receipt| {
                            receipt.key == receipt_key
                                && receipt.resolution_op_hash == row.resolution_op_hash
                        });
                    entry["receipt"] = json!({
                        "state": if recorded { "recorded" } else { "unrecorded" },
                        "receiptKey": receipt_key,
                    });
                }
            }
            effects.push(entry);
        }
        Ok(json!({
            "orgId": org_id,
            "affairId": affair_id,
            "nowMs": eval.now_ms,
            "effects": effects,
            "invalidResolutions": invalid,
        }))
    }

    /// 待应用效力事件的消费编排（任务 3 最小可用实现）：对
    /// `affair_org_effects` 判定为 Apply 的 (声明 × 决议) 写回执
    /// （`org:effectrcpt:` 键，线形见 effect.rs `EffectReceipt`）并逐条存证
    /// 锚定（domain = orgId，collection = "effectrcpt"）；收尾按 affair.md
    /// §6.3 对每条 Apply 生效决议封存 evi:resolution 条目（先条目后锚，
    /// 编排见 kernel/affair_evi_ops.rs）。
    ///
    /// 语义（协议未钉死处的最小可用判定，如实登记）：
    /// - 同一 scope 多条生效决议并存时，回执只跟踪**最新**决议（按决议锚定
    ///   时刻，平局 opHash 大者——链上时间的 LWW）；较旧决议的事件记
    ///   "superseded-by-newer" 不写回执（防两份生效决议来回覆写震荡）；
    /// - 幂等：同键回执且决议一致 → "already-recorded"；现行回执指向较旧
    ///   决议（如补扫历史）→ 覆盖更新为最新（"superseded"，历史在存证链）；
    /// - **名册/策略内容的实际变更不在内核**（决议 tally 为插件语义），
    ///   回执 = 内核受理留痕 + 组织侧/插件侧内容应用的机器可读凭据。
    pub fn affair_apply_org_effects(&mut self, org_id: &str, affair_id: &str) -> Result<Value> {
        let eval = self.affair_eval_for_org(org_id, affair_id)?;
        let (rows, invalid) = self.compute_effect_rows(&eval, org_id)?;
        let node_id = self.sync_node_id();
        let now = eval.now_ms;
        // 每个 scope 的最新 Apply 事件（决议锚定时刻 + opHash 字典序大者）
        let mut newest: HashMap<&str, &EffectRow> = HashMap::new();
        for row in rows
            .iter()
            .filter(|row| matches!(row.outcome, EffectHookOutcome::Apply(_)))
        {
            let entry = newest.entry(row.scope.as_str()).or_insert(row);
            if (row.resolution_anchored_ms.unwrap_or(i64::MIN), &row.resolution_op_hash)
                > (
                    entry.resolution_anchored_ms.unwrap_or(i64::MIN),
                    &entry.resolution_op_hash,
                )
            {
                *entry = row;
            }
        }
        let mut actions = Vec::new();
        for row in &rows {
            let EffectHookOutcome::Apply(event) = &row.outcome else {
                actions.push(json!({
                    "scope": row.scope,
                    "resolutionOpHash": row.resolution_op_hash,
                    "action": "skipped",
                    "outcome": effect_outcome_str(&row.outcome),
                }));
                continue;
            };
            if !std::ptr::eq(newest[row.scope.as_str()], row) {
                actions.push(json!({
                    "scope": row.scope,
                    "resolutionOpHash": row.resolution_op_hash,
                    "action": "skipped",
                    "outcome": "superseded-by-newer",
                }));
                continue;
            }
            let receipt_key = effect_receipt_key(&event.org_id, &event.affair_id, &event.scope)
                .map_err(|e| KernelError::Internal(e.reason().to_string()))?;
            let existing = self
                .require_storage()?
                .get(&receipt_key)?
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| parse_effect_receipt(&value).ok())
                .filter(|receipt| receipt.key == receipt_key);
            if existing
                .as_ref()
                .is_some_and(|receipt| receipt.resolution_op_hash == event.resolution_op_hash)
            {
                actions.push(json!({
                    "scope": event.scope,
                    "resolutionOpHash": event.resolution_op_hash,
                    "action": "already-recorded",
                    "receiptKey": receipt_key,
                }));
                continue;
            }
            let receipt = build_effect_receipt(event, now)
                .map_err(|e| KernelError::Internal(e.reason().to_string()))?;
            let storage = self.require_storage_raw_mut()?;
            storage.put(&receipt_key, &receipt.to_string())?;
            // 回执逐条存证（append-only 留痕；声明侧锚定查找按 payloadHash
            // 全链扫描，与本锚定同链互证）
            append_evidence(
                storage,
                NewEvidenceEntry::from_parts(
                    org_id,
                    "effectrcpt",
                    &receipt_key,
                    EvidenceOp::Put,
                    Some(&receipt),
                    None,
                    now,
                    &node_id,
                ),
            )?;
            actions.push(json!({
                "scope": event.scope,
                "resolutionOpHash": event.resolution_op_hash,
                "action": if existing.is_some() { "superseded" } else { "recorded" },
                "receiptKey": receipt_key,
            }));
        }
        // evi:resolution 条目（affair.md §6.3，A21）：生效判定路径「先条目
        // 后锚」——本组织每条 Apply 生效决议（三线齐备 = 生效 × 事先声明 ×
        // 事先性）向本机链确定性自写同内容条目，再触发锚；无声明组织效力的
        // 事务（纯讨论）无 Apply 行，自然不写。
        let mut seals = Vec::new();
        let mut seen = HashSet::new();
        for row in &rows {
            if !matches!(row.outcome, EffectHookOutcome::Apply(_))
                || !seen.insert(row.resolution_op_hash.as_str())
            {
                continue;
            }
            let (Some(op), Some(effective_ts)) = (
                eval.ops.iter().find(|op| op.op_hash == row.resolution_op_hash),
                row.resolution_anchored_ms,
            ) else {
                continue; // Apply 行必有决议锚定时刻；防御分支
            };
            seals.push(crate::kernel::affair_evi_ops::ResolutionSeal {
                resolution_op_hash: row.resolution_op_hash.clone(),
                payload: op.parsed.payload.clone(),
                sig_set: op.parsed.actor.org_sig.clone(),
                effective_ts,
            });
        }
        let resolution_evidence =
            self.seal_resolution_entries(org_id, &eval.affair_id, now, &seals)?;
        Ok(json!({
            "orgId": org_id,
            "affairId": affair_id,
            "nowMs": now,
            "actions": actions,
            "resolutionEvidence": resolution_evidence,
            "invalidResolutions": invalid,
        }))
    }

    /// 效力钩子求值共享：校验 orgId 并装配求值上下文。
    fn affair_eval_for_org(&self, org_id: &str, affair_id: &str) -> Result<AffairEval> {
        if !is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        self.affair_eval(affair_id)
    }

    /// 逐声明 × 逐有效决议的三线判定（`affair_org_effects` /
    /// `affair_apply_org_effects` 共用）。返回（判定行，复算无效被剔除的决议
    /// opHash 列表）。
    fn compute_effect_rows(
        &self,
        eval: &AffairEval,
        org_id: &str,
    ) -> Result<(Vec<EffectRow>, Vec<String>)> {
        let storage = self.require_storage()?;
        // 决议集合：复算无效者入无效集（§6.1），不产生效力
        let mut resolutions = Vec::new();
        let mut invalid = Vec::new();
        for re in eval.eval_resolutions() {
            if re.valid() == Some(false) {
                invalid.push(re.op_hash.clone());
                continue;
            }
            resolutions.push(re);
        }
        invalid.sort();

        // 声明集合：扫 (orgId, affairId) 键域，结构/键-文一致不符跳过。
        let prefix = format!("{EFFECT_GRANT_PREFIX}{org_id}:{}:", eval.affair_id);
        let mut rows = Vec::new();
        for (key, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue; // 损坏记录跳过
            };
            let Ok(grant) = parse_effect_grant(&value) else {
                continue;
            };
            if grant.key != key {
                continue; // 键-文不符（记录内容与挂载键不一致），fail-closed
            }
            let grant_anchor = grant_anchor_ms(storage, &value)?;
            for re in &resolutions {
                let outcome = match re.anchored_ms {
                    None => EffectHookOutcome::ResolutionNotAnchored,
                    Some(_) => evaluate_effect_hook(
                        Some(&grant),
                        grant_anchor,
                        &re.op_hash,
                        re.anchored_ms,
                        re.state.unwrap_or(ResolutionState::Pending),
                    ),
                };
                rows.push(EffectRow {
                    scope: grant.scope.clone(),
                    grant_key: grant.key.clone(),
                    resolution_op_hash: re.op_hash.clone(),
                    resolution_anchored_ms: re.anchored_ms,
                    outcome,
                });
            }
        }
        Ok((rows, invalid))
    }

    /// 读创世 + 已接受操作（opHash 字典序）；创世缺失报内部错误（调用方应
    /// 先关注/等复制收敛）。
    fn load_affair_genesis_ops(&self, affair_id: &str) -> Result<(Value, Vec<(String, Value)>)> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let storage = self.require_storage()?;
        let genesis_raw = storage
            .get(&affair_record_key(affair_id))?
            .ok_or_else(|| KernelError::Internal(format!("unknown affair: {affair_id}")))?;
        let genesis = serde_json::from_str::<Value>(&genesis_raw)
            .map_err(|e| KernelError::Internal(format!("corrupted genesis: {e}")))?;
        Ok((genesis, load_ops(storage, affair_id)?))
    }

    /// 求值上下文装配（各读/写路径共享）：创世 + 已接受操作 + 锚定表 →
    /// 规则链 replay（两阶段：先无名册 backbone 定阶梯参数，再注入投票前
    /// 快照名册全形态 replay）。
    fn affair_eval(&self, affair_id: &str) -> Result<AffairEval> {
        let (genesis_value, raw_ops) = self.load_affair_genesis_ops(affair_id)?;
        let genesis = parse_genesis(&genesis_value)
            .map_err(|e| KernelError::Internal(format!("corrupted genesis: {}", e.reason())))?;
        let genesis_rules = genesis_value
            .get("rules")
            .cloned()
            .ok_or_else(|| KernelError::Internal("corrupted genesis: rules missing".to_string()))?;
        let storage = self.require_storage()?;
        let anchors = affair_anchor_map(storage, affair_id)?;
        let now_ms = system_now_ms();
        let mut ops = Vec::with_capacity(raw_ops.len());
        for (op_hash, value) in raw_ops {
            let Ok(parsed) = parse_op(&value) else {
                continue; // 入站已全链校验；损坏记录跳过（防御）
            };
            let anchored_ms = anchors.get(&("ops".to_string(), op_hash.clone())).copied();
            ops.push(EvalOp {
                op_hash,
                parsed,
                anchored_ms,
            });
        }
        let edges: Vec<(String, String)> = ops
            .iter()
            .map(|op| (op.op_hash.clone(), op.parsed.prev_op_hash.clone()))
            .collect();
        let mut objections: HashMap<String, u64> = HashMap::new();
        for op in &ops {
            if op.parsed.op_type != OpType::Objection {
                continue;
            }
            if let Some(target) = op
                .parsed
                .payload
                .get("target")
                .and_then(Value::as_str)
                .map(ToString::to_string)
            {
                *objections.entry(target).or_default() += 1;
            }
        }
        let mut eval = AffairEval {
            affair_id: affair_id.to_string(),
            genesis,
            ops,
            anchors,
            now_ms,
            edges,
            objections,
            ladder_params: LadderParams::default(),
            chain: replay_rule_chain(affair_id, &genesis_rules, &[], now_ms)
                .map_err(|e| KernelError::Internal(format!("corrupted rules: {}", e.reason())))?,
        };
        // 阶段 A：无名册 backbone（vote 形态名册缺席恒待定）→ 定阶梯参数
        let entries = eval.rule_change_entries(false);
        let backbone = replay_rule_chain(affair_id, &genesis_rules, &entries, now_ms)
            .map_err(|e| KernelError::Internal(format!("corrupted rules: {}", e.reason())))?;
        eval.ladder_params = LadderParams::from_participation(
            backbone
                .current()
                .rules
                .get("participation")
                .unwrap_or(&Value::Null),
        )
        .map_err(|r| KernelError::Internal(format!("bad ladder params: {r}")))?;
        // 阶段 B：注入投票前快照名册的全形态 replay
        let entries = eval.rule_change_entries(true);
        eval.chain = replay_rule_chain(affair_id, &genesis_rules, &entries, now_ms)
            .map_err(|e| KernelError::Internal(format!("corrupted rules: {}", e.reason())))?;
        Ok(eval)
    }
}

/// 效力钩子判定行（`compute_effect_rows` 产物）。
struct EffectRow {
    scope: String,
    grant_key: String,
    resolution_op_hash: String,
    /// 决议锚定时刻（同 scope 多决议并存时取最新的排序键）。
    resolution_anchored_ms: Option<i64>,
    outcome: EffectHookOutcome,
}

/// 效力判定结果的稳定字符串（与 `affair_org_effects` 输出对齐）。
fn effect_outcome_str(outcome: &EffectHookOutcome) -> &'static str {
    match outcome {
        EffectHookOutcome::Apply(_) => "apply",
        EffectHookOutcome::NotDeclared => "notDeclared",
        EffectHookOutcome::Revoked => "revoked",
        EffectHookOutcome::ResolutionNotEffective => "resolutionNotEffective",
        EffectHookOutcome::GrantNotAnchored => "grantNotAnchored",
        EffectHookOutcome::ResolutionNotAnchored => "resolutionNotAnchored",
        EffectHookOutcome::NotPrior => "notPrior",
    }
}

/// 扫描 `affair:op:{affairId}:` 键域，返回 (opHash, 原文) 按 opHash 字典序。
pub(crate) fn load_ops<S: StorageBackend>(storage: &S, affair_id: &str) -> Result<Vec<(String, Value)>> {
    let prefix = format!("{}{}:", crate::affair::AFFAIR_OP_PREFIX, affair_id);
    let mut ops = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
        let Some(op_hash) = key.strip_prefix(&prefix) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            ops.push((op_hash.to_string(), value));
        }
    }
    ops.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(ops)
}

/// 本副本存证链锚定时刻索引：`affair:{affairId}` 域内 (collection, id) →
/// 条目时间戳（§7.2 时间源）。O(链高) 顺序扫描——命令层最小实现；链高增长
/// 后如需分页/索引属存证模块的独立优化项。
pub(crate) fn affair_anchor_map<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> Result<HashMap<AnchorKey, i64>> {
    let domain = format!("affair:{affair_id}");
    let height = crate::evidence::get_evidence_height(storage)?;
    let mut map = HashMap::new();
    for seq in 1..=height {
        let Some(entry) = crate::evidence::get_evidence_entry(storage, seq)? else {
            continue;
        };
        if entry.domain == domain {
            map.insert((entry.collection, entry.id), entry.timestamp);
        }
    }
    Ok(map)
}

/// 规则版本的（公示期， 否决阈值）（§6.2：公示期是规则参数；版本已过 §5.6
/// 静态检查，形状不符时回退缺省 24h/1——防御口径与原 genesis 读法一致）。
fn rules_pub_params(rules: &Value) -> (i64, u64) {
    let delay_ms = rules
        .get("pubPeriod")
        .and_then(|p| p.get("delayMs"))
        .and_then(Value::as_i64)
        .unwrap_or(crate::affair::DEFAULT_PUB_PERIOD_MS);
    let veto_count = rules
        .get("pubPeriod")
        .and_then(|p| p.get("vetoThreshold"))
        .and_then(|v| v.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    (delay_ms, veto_count)
}

/// 声明记录的存证锚定时刻：全链扫描 op=put 且 payloadHash 与现行记录逐字节
/// 一致的条目，取最早时刻（同一内容多次锚定取最早 = 该内容声明存在的最早
/// 链上证据）。O(链高) 顺序扫描，与 affair_anchor_map 同口径。声明记录具体
/// 落在哪个 (domain, collection) 由组织侧写入路径决定，本查找只认内容哈希，
/// 不预设写入侧键域约定。
fn grant_anchor_ms<S: StorageBackend>(storage: &S, grant_value: &Value) -> Result<Option<i64>> {
    let target = crate::evidence::build_evidence_payload_hash(Some(grant_value));
    let height = crate::evidence::get_evidence_height(storage)?;
    let mut found: Option<i64> = None;
    for seq in 1..=height {
        let Some(entry) = crate::evidence::get_evidence_entry(storage, seq)? else {
            continue;
        };
        if entry.op != crate::evidence::EvidenceOp::Put {
            continue;
        }
        if entry.payload_hash == target {
            found = Some(found.map_or(entry.timestamp, |t| t.min(entry.timestamp)));
        }
    }
    Ok(found)
}

#[cfg(test)]
#[path = "affair_ops_tests.rs"]
mod tests;
