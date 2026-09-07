//! affairsync-data 入站应用（wiki/protocol/community/affair-sync.md §4）。
//!
//! 每条记录过分链校验后落库：创世走 `verify_genesis` 全链（含 §5.6 静态检查），
//! 操作走 `verify_op` 入站校验链（affair.md §3.2）+ opHash 复算 + §11 主持人
//! 门槛 + 因果见证（prevOpHash / vote / objection 指向未知 → 持久暂存待补，
//! 不因乱序拒收）。接受的记录：本体 + pmeta + 逐条存证锚定（domain =
//! `affair:{affairId}`，创世 collection=genesis / 操作 collection=ops，
//! affair.md §3.3）+ DAG 头维护（`affair:head:`）。
//!
//! 新鲜度口径（affair.md §3.1）：declaredAt ±10min 门槛只压实时提交——本地
//! 写入入口（`ingest_local_entries`）执行；远端复制与 drain 复检走
//! `verify_op_replicated` 豁免新鲜度（复制保真靠签名 + opHash 链，历史补齐
//! 不被时间窗锁死）。
//!
//! 持久化 ingest 与 C1 `OpLog`（纯内存）同语义，差异在状态落盘：已知集 =
//! `affair:op:` 键域扫描，暂存 = `affairsync:pend:` 键域，头 = `affair:head:`
//! 键。affair 模块实现不在本面层改动（C1 边界）。

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::affair::{
    OpType, affair_head_key, affair_op_key, affair_record_key, compute_op_hash, verify_genesis,
};
use crate::evidence::{EvidenceOp, NewEvidenceEntry, build_next_evidence_entry};
use crate::storage::{BatchOperation, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::{DocMeta, generate_updated_meta};

use super::collect::affair_ops_data_prefix;
use super::drain::{drain_pending, stage_pending};
use super::envelope::{AffairsyncRecord, record_key_in_scope};
use super::follow::is_following;
use super::keys::affair_pend_prefix;

/// 存证域前缀（affair.md §3.3：`domain = "affair:{affairId}"`）。
pub(super) fn evidence_domain(affair_id: &str) -> String {
    format!("affair:{affair_id}")
}

/// 入站应用结果观测。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct AffairApplySummary {
    /// 目标事务。
    pub affair_id: String,
    /// 本批新接受（创世 + 操作）。
    pub accepted: usize,
    /// 已知条目重复入站。
    pub duplicates: usize,
    /// 本批新暂存待补。
    pub pending: usize,
    /// 拒收（fail-closed，含白名单违例与校验链失败）。
    pub rejected: usize,
    /// drain 阶段从暂存补入。
    pub drained: usize,
}

/// 单条入站条目的判定（内部）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Check {
    Accept,
    Duplicate,
    Pending,
    Reject,
}

/// 入站条目：远端记录（带 meta）或本地条目（meta 在接受时生成本机序号）。
#[derive(Clone, Debug)]
enum Incoming {
    Remote(AffairsyncRecord),
    Local(Value),
}

/// 应用一批评入站记录（affairsync-data 合入，§4）。
///
/// 门槛：本机已关注该事务（关注才复制）；任一记录 key 越白名单整批拒收
/// （§3.3 红线）。远端 meta 原样落盘。
pub fn apply_affairsync_records<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    records: &[AffairsyncRecord],
    now_ms: i64,
) -> SyncResult<AffairApplySummary> {
    let mut summary = AffairApplySummary {
        affair_id: affair_id.to_string(),
        ..AffairApplySummary::default()
    };
    if !is_following(storage, affair_id)? {
        log::info!("[AFFAIRSYNC] data rejected: not following affair={affair_id}");
        summary.rejected = records.len();
        return Ok(summary);
    }
    if records
        .iter()
        .any(|r| !record_key_in_scope(affair_id, &r.key))
    {
        log::warn!("[AFFAIRSYNC] data rejected: key out of scope affair={affair_id}");
        summary.rejected = records.len();
        return Ok(summary);
    }
    let incoming: Vec<Incoming> = records
        .iter()
        .map(|r| Incoming::Remote(r.clone()))
        .collect();
    apply_inner(storage, None, affair_id, &incoming, now_ms, &mut summary)?;
    Ok(summary)
}

/// 本地写入入口（发起方自举）：与远端批走同一校验链，meta 用本机
/// per-node 单调序号生成（vv_seq 与记录同事务提交）。调用方须已关注该事务。
pub fn ingest_local_entries<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    affair_id: &str,
    entries: &[Value],
    now_ms: i64,
) -> SyncResult<AffairApplySummary> {
    let mut summary = AffairApplySummary {
        affair_id: affair_id.to_string(),
        ..AffairApplySummary::default()
    };
    if !is_following(storage, affair_id)? {
        return Err(crate::sync::SyncError::Adapter(
            "cannot ingest local entries for an unfollowed affair".to_string(),
        ));
    }
    let incoming: Vec<Incoming> = entries.iter().map(|e| Incoming::Local(e.clone())).collect();
    apply_inner(
        storage,
        Some(node_id),
        affair_id,
        &incoming,
        now_ms,
        &mut summary,
    )?;
    Ok(summary)
}

/// 应用主流程：逐条判定 → 持久化 → drain 暂存循环 → 写回 DAG 头。
fn apply_inner<S: StorageBackend>(
    storage: &mut S,
    local_node_id: Option<&str>,
    affair_id: &str,
    incoming: &[Incoming],
    now_ms: i64,
    summary: &mut AffairApplySummary,
) -> SyncResult<()> {
    let mut state = AffairState::load(storage, affair_id)?;
    for item in incoming {
        match item {
            Incoming::Remote(record) => {
                apply_one_remote(storage, affair_id, record, now_ms, &mut state, summary)?
            }
            Incoming::Local(entry) => apply_one_local(
                storage,
                local_node_id,
                affair_id,
                entry,
                now_ms,
                &mut state,
                summary,
            )?,
        }
    }
    drain_pending(
        storage,
        local_node_id,
        affair_id,
        now_ms,
        &mut state,
        summary,
    )?;
    state.store_heads(storage)
}

/// 事务落盘状态（一次应用批内的内存视图，批末统一写头）。
pub(super) struct AffairState {
    /// 事务 id。
    pub(super) affair_id: String,
    /// 创世记录存在且可解析时的主持人（创世 initiator.identity，§11 门槛）。
    pub(super) moderator: Option<String>,
    /// 本地已知 opHash 集（`affair:op:` 键域）。
    pub(super) known: BTreeSet<String>,
    /// 暂存 opHash 集（`affairsync:pend:` 键域）。
    pub(super) staged: BTreeSet<String>,
    /// DAG 头集合（`affair:head:`）。
    pub(super) heads: BTreeSet<String>,
}

impl AffairState {
    fn load<S: StorageBackend>(storage: &S, affair_id: &str) -> SyncResult<Self> {
        let moderator = storage
            .get(&affair_record_key(affair_id))?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| crate::affair::parse_genesis(&value).ok())
            .map(|genesis| genesis.initiator.identity);
        let ops_prefix = affair_ops_data_prefix(affair_id);
        let mut known = BTreeSet::new();
        for (key, _) in storage.scan(&crate::storage::ScanOptions::prefix(&ops_prefix))? {
            if let Some(op_hash) = key.strip_prefix(&ops_prefix) {
                known.insert(op_hash.to_string());
            }
        }
        let pend_prefix = affair_pend_prefix(affair_id);
        let mut staged = BTreeSet::new();
        for (key, _) in storage.scan(&crate::storage::ScanOptions::prefix(&pend_prefix))? {
            if let Some(op_hash) = key.strip_prefix(&pend_prefix) {
                staged.insert(op_hash.to_string());
            }
        }
        let heads = storage
            .get(&affair_head_key(affair_id))?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .map(|value| {
                value
                    .get("heads")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        Ok(Self {
            affair_id: affair_id.to_string(),
            moderator,
            known,
            staged,
            heads,
        })
    }

    /// 因果见证目标是否已知：创世 affairId、已知条目或暂存条目（§3.2）。
    fn target_known(&self, hash: &str) -> bool {
        hash == self.affair_id || self.known.contains(hash) || self.staged.contains(hash)
    }

    fn store_heads<S: StorageBackend>(&self, storage: &mut S) -> SyncResult<()> {
        let heads: Vec<String> = self.heads.iter().cloned().collect();
        storage.put(
            &affair_head_key(&self.affair_id),
            &json!({ "heads": heads }).to_string(),
        )?;
        Ok(())
    }

    /// 接受操作后的簿记：入已知集、维护 DAG 头（移除被引 prev、加入新头）。
    fn note_accepted(&mut self, prev_op_hash: &str, op_hash: &str) {
        self.known.insert(op_hash.to_string());
        if prev_op_hash != self.affair_id {
            self.heads.remove(prev_op_hash);
        }
        self.heads.insert(op_hash.to_string());
        self.staged.remove(op_hash);
    }
}

/// 操作入站判定链：opHash 复算 → 重复 → verify_op（结构/affairId/验签/
/// payload；`enforce_freshness` = 实时提交才压 declaredAt 新鲜度，复制与
/// drain 复检豁免，affair.md §3.1）→ §11 主持人门槛 → 因果见证（未知暂存）。
pub(super) fn check_op(
    state: &AffairState,
    entry: &Value,
    op_hash: &str,
    now_ms: i64,
    enforce_freshness: bool,
) -> (Check, String) {
    let Ok(computed) = compute_op_hash(entry) else {
        return (Check::Reject, "malformed-op".to_string());
    };
    if computed != op_hash {
        return (Check::Reject, "op-hash-mismatch".to_string());
    }
    if state.known.contains(op_hash) {
        return (Check::Duplicate, String::new());
    }
    let verified = if enforce_freshness {
        crate::affair::verify_op(entry, &state.affair_id, now_ms)
    } else {
        crate::affair::verify_op_replicated(entry, &state.affair_id)
    };
    let Ok((op, link_target)) = verified else {
        return (Check::Reject, "verify-op-failed".to_string());
    };
    // §11：moderate/meta-revise 仅主持人；创世缺失时主持人不可判定 → 暂存
    if matches!(op.op_type, OpType::Moderate | OpType::MetaRevise) {
        match &state.moderator {
            Some(moderator) if op.actor.identity != *moderator => {
                return (Check::Reject, "not-moderator".to_string());
            }
            None => return (Check::Pending, String::new()),
            _ => {}
        }
    }
    if !state.target_known(&op.prev_op_hash) || link_target.is_some_and(|t| !state.target_known(&t))
    {
        return (Check::Pending, String::new());
    }
    (Check::Accept, String::new())
}

/// 持久化一条已接受的操作：本体 + pmeta + 存证 + 本机 vv_seq（本地写入时），
/// 同一 batch 原子提交；簿记在 state 内完成。
#[allow(clippy::too_many_arguments)]
pub(super) fn persist_accepted_op<S: StorageBackend>(
    storage: &mut S,
    state: &mut AffairState,
    entry: &Value,
    op_hash: &str,
    meta: &DocMeta,
    evidence_node_id: &str,
    now_ms: i64,
    local_seq: Option<(String, i64)>,
) -> SyncResult<()> {
    let prev_op_hash = entry
        .get("prevOpHash")
        .and_then(Value::as_str)
        .unwrap_or(&state.affair_id)
        .to_string();
    let mut ops = vec![
        BatchOperation::put(affair_op_key(&state.affair_id, op_hash), entry.to_string()),
        BatchOperation::put(
            crate::sync::personal_meta_key(&affair_op_key(&state.affair_id, op_hash)),
            serde_json::to_string(meta)?,
        ),
    ];
    if let Some((node_id, seq)) = local_seq {
        ops.push(crate::sync::personal::vv_seq_batch_op(&node_id, seq));
    }
    let meta_value = serde_json::to_value(meta)?;
    let evidence = build_next_evidence_entry(
        storage,
        NewEvidenceEntry::from_parts(
            evidence_domain(&state.affair_id),
            "ops",
            op_hash,
            EvidenceOp::Put,
            Some(entry),
            Some(&meta_value),
            now_ms,
            evidence_node_id,
        ),
    )?;
    ops.extend(crate::evidence::evidence_batch_operations(&evidence)?);
    storage.batch(ops)?;
    state.note_accepted(&prev_op_hash, op_hash);
    Ok(())
}

/// 处理一条远端记录（创世或操作）。
fn apply_one_remote<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    record: &AffairsyncRecord,
    now_ms: i64,
    state: &mut AffairState,
    summary: &mut AffairApplySummary,
) -> SyncResult<()> {
    if record.key == affair_record_key(affair_id) {
        if apply_genesis(
            storage,
            affair_id,
            &record.value,
            Some(&record.meta),
            now_ms,
            None,
            summary,
        )? {
            refresh_moderator(state, &record.value);
        }
        return Ok(());
    }
    let op_hash = record.key[affair_ops_data_prefix(affair_id).len()..].to_string();
    // 远端复制：豁免 declaredAt 新鲜度（affair.md §3.1）
    match check_op(state, &record.value, &op_hash, now_ms, false) {
        (Check::Accept, _) => {
            let node_id = record
                .meta
                .node_id
                .clone()
                .unwrap_or_else(|| "remote-node".to_string());
            persist_accepted_op(
                storage,
                state,
                &record.value,
                &op_hash,
                &record.meta,
                &node_id,
                now_ms,
                None,
            )?;
            summary.accepted += 1;
        }
        (Check::Duplicate, _) => summary.duplicates += 1,
        (Check::Pending, _) => {
            stage_pending(
                storage,
                affair_id,
                &record.value,
                &op_hash,
                Some(&record.meta),
            )?;
            state.staged.insert(op_hash);
            summary.pending += 1;
        }
        (Check::Reject, reason) => {
            log::info!("[AFFAIRSYNC] op rejected affair={affair_id} reason={reason}");
            summary.rejected += 1;
        }
    }
    Ok(())
}

/// 处理一条本地条目（发起方自举）。
#[allow(clippy::too_many_arguments)]
fn apply_one_local<S: StorageBackend>(
    storage: &mut S,
    local_node_id: Option<&str>,
    affair_id: &str,
    entry: &Value,
    now_ms: i64,
    state: &mut AffairState,
    summary: &mut AffairApplySummary,
) -> SyncResult<()> {
    let Some(node_id) = local_node_id else {
        return Err(crate::sync::SyncError::Adapter(
            "local ingest requires node_id".to_string(),
        ));
    };
    if entry.get("affairV").and_then(Value::as_u64) == Some(1) {
        if apply_genesis(
            storage,
            affair_id,
            entry,
            None,
            now_ms,
            Some(node_id),
            summary,
        )? {
            refresh_moderator(state, entry);
        }
        return Ok(());
    }
    let op_hash = compute_op_hash(entry)
        .map_err(|_| crate::sync::SyncError::Adapter("malformed local op".to_string()))?;
    // 本地实时提交：执行 declaredAt 新鲜度门槛（affair.md §3.1）
    match check_op(state, entry, &op_hash, now_ms, true) {
        (Check::Accept, _) => {
            let (meta, seq) = generate_updated_meta(
                storage,
                node_id,
                &evidence_domain(affair_id),
                "ops",
                &op_hash,
                now_ms,
            )?;
            persist_accepted_op(
                storage,
                state,
                entry,
                &op_hash,
                &meta,
                node_id,
                now_ms,
                Some((node_id.to_string(), seq)),
            )?;
            summary.accepted += 1;
        }
        (Check::Duplicate, _) => summary.duplicates += 1,
        (Check::Pending, _) => {
            stage_pending(storage, affair_id, entry, &op_hash, None)?;
            state.staged.insert(op_hash);
            summary.pending += 1;
        }
        (Check::Reject, reason) => {
            return Err(crate::sync::SyncError::Adapter(format!(
                "local op rejected: {reason}"
            )));
        }
    }
    Ok(())
}

/// 创世记录处理（远端/本地共用）：本地已有 → 载荷一致性幂等去重，冲突拒收；
/// 本地缺失 → verify_genesis 全链（含 §5.6 静态检查）→ 落库 + 存证。
/// 返回 true 表示本批新接受（调用方据此刷新内存视图的主持人）。
#[allow(clippy::too_many_arguments)]
fn apply_genesis<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    value: &Value,
    remote_meta: Option<&DocMeta>,
    now_ms: i64,
    local_node_id: Option<&str>,
    summary: &mut AffairApplySummary,
) -> SyncResult<bool> {
    let rec_key = affair_record_key(affair_id);
    if let Some(existing_raw) = storage.get(&rec_key)? {
        let existing_value = serde_json::from_str::<Value>(&existing_raw).ok();
        let existing_hash = crate::evidence::build_evidence_payload_hash(existing_value.as_ref());
        let incoming_hash = crate::evidence::build_evidence_payload_hash(Some(value));
        if existing_hash == incoming_hash {
            summary.duplicates += 1;
        } else {
            log::info!("[AFFAIRSYNC] genesis conflict affair={affair_id}, keep local");
            summary.rejected += 1;
        }
        return Ok(false);
    }
    let Ok((_, computed_id, _)) = verify_genesis(value) else {
        log::info!("[AFFAIRSYNC] genesis verify failed affair={affair_id}");
        summary.rejected += 1;
        return Ok(false);
    };
    if computed_id != affair_id {
        log::info!("[AFFAIRSYNC] genesis affairId mismatch");
        summary.rejected += 1;
        return Ok(false);
    }
    // meta：远端原样落盘；本地用 per-node 单调序号生成
    let (meta, local_seq, evidence_node_id) = match (remote_meta, local_node_id) {
        (Some(m), _) => (
            m.clone(),
            None,
            m.node_id.clone().unwrap_or_else(|| "remote-node".into()),
        ),
        (None, Some(node_id)) => {
            let (meta, seq) = generate_updated_meta(
                storage,
                node_id,
                &evidence_domain(affair_id),
                "genesis",
                affair_id,
                now_ms,
            )?;
            (meta, Some((node_id.to_string(), seq)), node_id.to_string())
        }
        (None, None) => {
            return Err(crate::sync::SyncError::Adapter(
                "genesis requires remote meta or local node_id".to_string(),
            ));
        }
    };
    let mut ops = vec![
        BatchOperation::put(rec_key.clone(), value.to_string()),
        BatchOperation::put(
            crate::sync::personal_meta_key(&rec_key),
            serde_json::to_string(&meta)?,
        ),
    ];
    if let Some((node_id, seq)) = local_seq {
        ops.push(crate::sync::personal::vv_seq_batch_op(&node_id, seq));
    }
    let meta_value = serde_json::to_value(&meta)?;
    let evidence = build_next_evidence_entry(
        storage,
        NewEvidenceEntry::from_parts(
            evidence_domain(affair_id),
            "genesis",
            affair_id,
            EvidenceOp::Put,
            Some(value),
            Some(&meta_value),
            now_ms,
            evidence_node_id,
        ),
    )?;
    ops.extend(crate::evidence::evidence_batch_operations(&evidence)?);
    storage.batch(ops)?;
    summary.accepted += 1;
    Ok(true)
}

/// 创世接受后刷新内存视图的主持人（§11 门槛的同批判定用：本批内先于
/// 创世加载 state 时 moderator 为空，创世落库后即可判定）。
fn refresh_moderator(state: &mut AffairState, genesis_value: &Value) {
    if let Ok(genesis) = crate::affair::parse_genesis(genesis_value) {
        state.moderator = Some(genesis.initiator.identity);
    }
}
