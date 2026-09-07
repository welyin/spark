//! 乱序暂存与 drain（affair-sync §4）：prevOpHash / vote / objection 指向未知
//! 条目的操作持久暂存于 `affairsync:pend:{affairId}:{opHash}`，每批合入后
//! 重扫做 drain 循环，指向补齐即入有效集（语义同 C1 `OpLog` 的 pending/drain，
//! 持久化归本面层）。
//!
//! 暂存值线形：`{"entry": <操作条目>, "meta"?: <DocMeta>}`——meta 缺省表示
//! 本地起源（drain 接受时用本机 per-node 序号生成）；远端暂存 meta 随条目
//! 原样保存，drain 接受时落盘。declaredAt 新鲜度只压实时提交（affair.md
//! §3.1）：drain 复检豁免时间窗（复制保真靠签名 + opHash 链），失效条目
//! （验签/结构/affairId 不符）fail-closed 丢弃（从暂存删除，不再入队）。

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::{DocMeta, generate_updated_meta};

use super::apply::{
    AffairApplySummary, AffairState, Check, check_op, evidence_domain, persist_accepted_op,
};
use super::keys::{affair_pend_key, affair_pend_prefix};
/// 暂存一条待补操作（§4：未知则暂存待补，不因乱序拒收）。
pub(super) fn stage_pending<S: StorageBackend>(
    storage: &mut S,
    affair_id: &str,
    entry: &Value,
    op_hash: &str,
    meta: Option<&DocMeta>,
) -> SyncResult<()> {
    let value = match meta {
        Some(m) => serde_json::json!({ "entry": entry, "meta": m }),
        None => serde_json::json!({ "entry": entry }),
    };
    storage.put(&affair_pend_key(affair_id, op_hash), &value.to_string())?;
    Ok(())
}

/// drain 循环：重扫暂存区，指向补齐即入有效集；失效/过期/重复条目从暂存删除。
#[allow(clippy::too_many_arguments)]
pub(super) fn drain_pending<S: StorageBackend>(
    storage: &mut S,
    local_node_id: Option<&str>,
    affair_id: &str,
    now_ms: i64,
    state: &mut AffairState,
    summary: &mut AffairApplySummary,
) -> SyncResult<()> {
    loop {
        let pend_prefix = affair_pend_prefix(affair_id);
        let staged: Vec<(String, String)> = storage
            .scan(&ScanOptions::prefix(&pend_prefix))?
            .into_iter()
            .filter_map(|(key, raw)| {
                key.strip_prefix(&pend_prefix)
                    .map(|op_hash| (op_hash.to_string(), raw))
            })
            .collect();
        if staged.is_empty() {
            return Ok(());
        }
        let mut progress = 0usize;
        for (op_hash, raw) in staged {
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                drop_staged(storage, state, affair_id, &op_hash)?;
                continue;
            };
            let Some(entry) = value.get("entry").cloned() else {
                drop_staged(storage, state, affair_id, &op_hash)?;
                continue;
            };
            // drain 复检：豁免 declaredAt 新鲜度（affair.md §3.1；暂存条目
            // 可能已驻留多时，复检卡时间窗会把合法历史操作挡在有效集外）
            match check_op(state, &entry, &op_hash, now_ms, false) {
                (Check::Accept, _) => {
                    let resolved =
                        resolve_meta(storage, local_node_id, affair_id, &value, &op_hash, now_ms);
                    let Some((meta, local_seq, evidence_node_id)) = resolved else {
                        drop_staged(storage, state, affair_id, &op_hash)?;
                        continue;
                    };
                    persist_accepted_op(
                        storage,
                        state,
                        &entry,
                        &op_hash,
                        &meta,
                        &evidence_node_id,
                        now_ms,
                        local_seq,
                    )?;
                    storage.delete(&affair_pend_key(affair_id, &op_hash))?;
                    summary.drained += 1;
                    progress += 1;
                }
                (Check::Duplicate, _) => drop_staged(storage, state, affair_id, &op_hash)?,
                (Check::Pending, _) => {}
                (Check::Reject, _) => {
                    drop_staged(storage, state, affair_id, &op_hash)?;
                    summary.rejected += 1;
                    progress += 1;
                }
            }
        }
        if progress == 0 {
            return Ok(());
        }
    }
}

/// 从暂存删除并同步内存视图。
fn drop_staged<S: StorageBackend>(
    storage: &mut S,
    state: &mut AffairState,
    affair_id: &str,
    op_hash: &str,
) -> SyncResult<()> {
    storage.delete(&affair_pend_key(affair_id, op_hash))?;
    state.staged.remove(op_hash);
    Ok(())
}

/// 解析暂存条目的 meta：远端原样携带；本地起源现生成（无 local_node_id
/// 且未携带 meta → None，调用方丢弃该暂存条目）。
///
/// meta 键缺失（本地起源暂存线形本就不带 meta）或解析失败一律落本机兜底
/// 分支现生成——不得用 `?` 短路，否则本地暂存条目恰在 drain 判 Accept 的
/// 可入集时刻被静默删除（评审社区 C4 发现 1）。
fn resolve_meta<S: StorageBackend>(
    storage: &S,
    local_node_id: Option<&str>,
    affair_id: &str,
    staged_value: &Value,
    op_hash: &str,
    now_ms: i64,
) -> Option<(DocMeta, Option<(String, i64)>, String)> {
    if let Some(raw_meta) = staged_value.get("meta") {
        if let Ok(meta) = serde_json::from_value::<DocMeta>(raw_meta.clone()) {
            let node_id = meta.node_id.clone().unwrap_or_else(|| "remote-node".into());
            return Some((meta, None, node_id));
        }
    }
    let node_id = local_node_id?;
    let (meta, seq) = generate_updated_meta(
        storage,
        node_id,
        &evidence_domain(affair_id),
        "ops",
        op_hash,
        now_ms,
    )
    .ok()?;
    Some((meta, Some((node_id.to_string(), seq)), node_id.to_string()))
}
