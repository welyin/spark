//! `applyRemoteUpdate`：按集合声明的 syncStrategy 应用远端变更。
//!
//! 逐行对齐 desktop/src/main/db/sync.ts：
//! - append-only 仅接受新文档（幂等去重时合并 vv/ts 促进收敛）
//! - lww 按版本向量判定新旧，并发冲突按时间戳裁决
//! - purge 水位线拦截经 `PurgeWatermark` 接口注入（watermark 模块由 data-mgmt 实现）

use std::collections::BTreeMap;

use serde_json::Value;

use crate::evidence::{
    EvidenceOp, NewEvidenceEntry, build_evidence_payload_hash, build_next_evidence_entry,
    evidence_batch_operations,
};
use crate::schema::{
    CollectionSchemaDeclaration, ResolvedCollectionPolicy, SyncStrategy, resolve_collection_policy,
    sanitize_schema_hint,
};
use crate::storage::{BatchOperation, StorageBackend};
use crate::sync::meta::{
    CompareResult, DocMeta, RemoteMeta, compare_version_vectors, get_meta, merge_version_vectors,
    meta_key, resolve_conflict_by_lww,
};
use crate::sync::SyncResult;

/// 集合适配器：对应 TS `collectionInstance` 被鸭子类型访问的 `get` 与私有方法
/// `docKey` / `indexKey` / `buildIndexMap`。由调用方（如 data-mgmt 的 collection）实现。
pub trait CollectionAdapter {
    /// `collectionInstance.get(id)`：读取本地文档（经 `storage` 读 `doc_key`），不存在返回 `Ok(None)`。
    fn get(&self, storage: &dyn StorageBackend, id: &str) -> SyncResult<Option<Value>>;

    /// `docKey(id)`：主键文档键（TS 为 `doc:{domain}:{collection}:{id}`）。
    fn doc_key(&self, id: &str) -> String;

    /// `indexKey(indexName, indexValue, id)`：二级索引键
    /// （TS 为 `idx:{domain}:{collection}:{indexName}:{encodeURIComponent(value)}:{id}`）。
    fn index_key(&self, index_name: &str, index_value: &str, id: &str) -> String;

    /// `buildIndexMap(doc)`：从文档构建索引映射（field → value）；`None` 返回空映射。
    fn build_index_map(&self, doc: Option<&Value>) -> BTreeMap<String, String>;
}

/// purge 水位线拦截接口：远端 meta 时间戳早于水位线时拒绝落地，防止已清理数据回灌。
///
/// 由后续 data-mgmt 的 watermark 模块实现；`apply_remote_update` 未注入时不拦截
/// （等价于水位线记录不存在）。
pub trait PurgeWatermark {
    /// `isPurgedByWatermark(db, domain, collection, remoteTs)`。
    fn is_purged_by_watermark(
        &self,
        storage: &mut dyn StorageBackend,
        domain: &str,
        collection: &str,
        remote_ts: i64,
    ) -> SyncResult<bool>;
}

/// `applyRemoteUpdate` 的可选项。
#[derive(Default)]
pub struct ApplyRemoteOptions<'a> {
    /// 同步消息携带的策略声明副本：仅当本地未声明时作为本次应用的兜底策略
    /// （瞬时生效，不持久化；本地已声明的策略始终优先）。
    pub schema: Option<CollectionSchemaDeclaration>,
    /// purge 水位线检查（未注入则不拦截）。
    pub watermark: Option<&'a dyn PurgeWatermark>,
    /// 存证条目时间戳（对齐 TS `Date.now()`，由调用方注入）。
    pub now_ms: i64,
}

/// `applyRemoteUpdate` 的裁决结果（TS 返回 void；此处用于观测各分支）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 水位线拦截，未落地。
    PurgedByWatermark,
    /// append-only：远端删除被拒绝（告警）。
    AppendOnlyDeleteRejected,
    /// append-only：本地无此文档，已接受写入。
    AppendOnlyAccepted,
    /// append-only：载荷一致，幂等去重；`meta_updated` 表示 vv/ts 合并后是否回写了 meta。
    AppendOnlyDeduplicated {
        /// 合并后 meta 有变化并已回写。
        meta_updated: bool,
    },
    /// append-only：载荷冲突，保留本地（告警）。
    AppendOnlyConflictKeptLocal,
    /// lww：远端版本更新（cmp == remote），已落地。
    LwwRemoteApplied,
    /// lww：本地版本更新（cmp == local），未动。
    LwwLocalKept,
    /// lww：版本相等（cmp == equal），未动。
    LwwEqualNoop,
    /// lww：并发冲突远端时间戳胜出，已落地。
    LwwConcurrentRemoteApplied,
    /// lww：并发冲突本地胜出或时间戳相等，未动。
    LwwConcurrentLocalKept,
}

/// `applyRemoteUpdate`：将远端更新合并到本地集合。
///
/// `remote_payload` 为 `None` 表示远端删除。`remote_meta.node_id` 缺失时存证条目
/// nodeId 记为 `"remote-node"`（对齐 TS `remoteMeta.nodeId ?? 'remote-node'`）。
#[allow(clippy::too_many_arguments)]
pub fn apply_remote_update<S: StorageBackend, A: CollectionAdapter>(
    storage: &mut S,
    adapter: &A,
    domain: &str,
    collection: &str,
    id: &str,
    remote_payload: Option<&Value>,
    remote_meta: &RemoteMeta,
    options: ApplyRemoteOptions<'_>,
) -> SyncResult<ApplyOutcome> {
    // purge 水位线拦截：本地手动清理过的时代（remoteMeta.ts 早于水位线）拒绝落地，
    // 防止已清理数据经推送/反熵拉取回灌。
    if let Some(watermark) = options.watermark
        && watermark.is_purged_by_watermark(storage, domain, collection, remote_meta.ts)?
    {
        return Ok(ApplyOutcome::PurgedByWatermark);
    }

    // schema hint 仅 sanitize 后作兜底；解析生效策略（本地声明始终优先）
    let fallback = sanitize_schema_hint(options.schema.as_ref());
    let policy = resolve_collection_policy(storage, domain, collection, fallback.as_ref())?;

    if policy.sync_strategy == SyncStrategy::AppendOnly {
        return apply_remote_append_only(
            storage,
            adapter,
            domain,
            collection,
            id,
            remote_payload,
            remote_meta,
            &policy,
            options.now_ms,
        );
    }
    apply_remote_lww(
        storage,
        adapter,
        domain,
        collection,
        id,
        remote_payload,
        remote_meta,
        &policy,
        options.now_ms,
    )
}

/// append-only 集合的远端应用：
/// - 本地不存在该文档：接受写入（附 meta 与存证）
/// - 本地已存在且载荷一致：幂等去重，合并版本向量促进收敛
/// - 本地已存在但载荷冲突 / 远端删除：拒绝（不覆盖、不删除）
#[allow(clippy::too_many_arguments)]
fn apply_remote_append_only<S: StorageBackend, A: CollectionAdapter>(
    storage: &mut S,
    adapter: &A,
    domain: &str,
    collection: &str,
    id: &str,
    remote_payload: Option<&Value>,
    remote_meta: &RemoteMeta,
    policy: &ResolvedCollectionPolicy,
    now_ms: i64,
) -> SyncResult<ApplyOutcome> {
    let Some(payload) = remote_payload else {
        // append-only 集合不删除：丢弃远端删除
        return Ok(ApplyOutcome::AppendOnlyDeleteRejected);
    };

    let local = adapter.get(storage, id)?;
    let local_meta = get_meta(storage, domain, collection, id)?;

    let Some(local_doc) = local else {
        // 本地无此文档：接受写入（doc + meta + 索引 + evidence[op=put]）
        let stored_meta = DocMeta {
            vv: remote_meta.vv.clone(),
            ts: remote_meta.ts,
            ..DocMeta::default()
        };
        let mut ops = vec![
            BatchOperation::put(adapter.doc_key(id), serde_json::to_string(payload)?),
            BatchOperation::put(meta_key(domain, collection, id), serde_json::to_string(&stored_meta)?),
        ];
        for (field, value) in adapter.build_index_map(Some(payload)) {
            ops.push(BatchOperation::put(adapter.index_key(&field, &value, id), ""));
        }
        if policy.enable_evidence {
            // 注意：metaHash 对入参 remoteMeta（{vv, ts, nodeId?}）计算，而非落盘的 {vv, ts}
            let remote_meta_value = serde_json::to_value(remote_meta)?;
            let entry = build_next_evidence_entry(
                storage,
                NewEvidenceEntry::from_parts(
                    domain,
                    collection,
                    id,
                    EvidenceOp::Put,
                    Some(payload),
                    Some(&remote_meta_value),
                    now_ms,
                    remote_meta.node_id.as_deref().unwrap_or("remote-node"),
                ),
            )?;
            ops.extend(evidence_batch_operations(&entry)?);
        }
        storage.batch(ops)?;
        return Ok(ApplyOutcome::AppendOnlyAccepted);
    };

    if build_evidence_payload_hash(Some(&local_doc)) == build_evidence_payload_hash(Some(payload)) {
        // 幂等去重：合并 vv（取大）与 ts（取大），有变化才写 meta
        let local_vv = local_meta.as_ref().map(|m| m.vv.clone());
        let local_ts = local_meta.as_ref().map_or(0, |m| m.ts);
        let merged_vv = merge_version_vectors(local_vv.as_ref(), Some(&remote_meta.vv));
        let merged_ts = local_ts.max(remote_meta.ts);
        let changed = merged_vv != local_vv.unwrap_or_default() || merged_ts != local_ts;
        if changed {
            let merged_meta = DocMeta {
                vv: merged_vv,
                ts: merged_ts,
                ..DocMeta::default()
            };
            storage.put(
                &meta_key(domain, collection, id),
                &serde_json::to_string(&merged_meta)?,
            )?;
        }
        return Ok(ApplyOutcome::AppendOnlyDeduplicated {
            meta_updated: changed,
        });
    }

    // 载荷冲突：保留本地
    Ok(ApplyOutcome::AppendOnlyConflictKeptLocal)
}

/// lww 集合的远端应用：版本向量判定新旧，并发冲突按时间戳裁决。
#[allow(clippy::too_many_arguments)]
fn apply_remote_lww<S: StorageBackend, A: CollectionAdapter>(
    storage: &mut S,
    adapter: &A,
    domain: &str,
    collection: &str,
    id: &str,
    remote_payload: Option<&Value>,
    remote_meta: &RemoteMeta,
    policy: &ResolvedCollectionPolicy,
    now_ms: i64,
) -> SyncResult<ApplyOutcome> {
    let local_meta = get_meta(storage, domain, collection, id)?;
    let cmp = compare_version_vectors(
        local_meta.as_ref().map(|m| &m.vv),
        Some(&remote_meta.vv),
    );
    match cmp {
        CompareResult::Remote => {
            apply_lww_remote_win(storage, adapter, domain, collection, id, remote_payload, remote_meta, policy, now_ms)?;
            Ok(ApplyOutcome::LwwRemoteApplied)
        }
        CompareResult::Local => Ok(ApplyOutcome::LwwLocalKept),
        CompareResult::Equal => Ok(ApplyOutcome::LwwEqualNoop),
        CompareResult::Concurrent => {
            let winner = resolve_conflict_by_lww(local_meta.as_ref().map(|m| m.ts), Some(remote_meta.ts));
            if winner == CompareResult::Remote {
                apply_lww_remote_win(storage, adapter, domain, collection, id, remote_payload, remote_meta, policy, now_ms)?;
                return Ok(ApplyOutcome::LwwConcurrentRemoteApplied);
            }
            Ok(ApplyOutcome::LwwConcurrentLocalKept)
        }
    }
}

/// lww 远端胜出落地：put 写 doc + 索引 diff + meta + evidence；
/// delete 删 doc + 索引，写 tombstone meta `{vv, ts, tombstone: true}` + evidence。
#[allow(clippy::too_many_arguments)]
fn apply_lww_remote_win<S: StorageBackend, A: CollectionAdapter>(
    storage: &mut S,
    adapter: &A,
    domain: &str,
    collection: &str,
    id: &str,
    remote_payload: Option<&Value>,
    remote_meta: &RemoteMeta,
    policy: &ResolvedCollectionPolicy,
    now_ms: i64,
) -> SyncResult<()> {
    match remote_payload {
        None => {
            let local = adapter.get(storage, id)?;
            let mut ops = vec![];
            if let Some(doc) = &local {
                ops.push(BatchOperation::delete(adapter.doc_key(id)));
                for (field, value) in adapter.build_index_map(Some(doc)) {
                    ops.push(BatchOperation::delete(adapter.index_key(&field, &value, id)));
                }
            }
            let tombstone = DocMeta {
                vv: remote_meta.vv.clone(),
                ts: remote_meta.ts,
                node_id: None,
                tombstone: Some(true),
            };
            // 落盘用结构体序列化（字段序 vv,ts,tombstone 对齐 TS JSON.stringify）；
            // 存证 metaHash 的输入等价于 JSON.parse(tombstoneValue)
            let tombstone_value = serde_json::to_value(&tombstone)?;
            ops.push(BatchOperation::put(
                meta_key(domain, collection, id),
                serde_json::to_string(&tombstone)?,
            ));
            if policy.enable_evidence {
                let entry = build_next_evidence_entry(
                    storage,
                    NewEvidenceEntry::from_parts(
                        domain,
                        collection,
                        id,
                        EvidenceOp::Delete,
                        None,
                        Some(&tombstone_value),
                        now_ms,
                        remote_meta.node_id.as_deref().unwrap_or("remote-node"),
                    ),
                )?;
                ops.extend(evidence_batch_operations(&entry)?);
            }
            storage.batch(ops)?;
        }
        Some(payload) => {
            let existing = adapter.get(storage, id)?;
            let old_index_map = adapter.build_index_map(existing.as_ref());
            let new_index_map = adapter.build_index_map(Some(payload));
            let mut ops = vec![BatchOperation::put(
                adapter.doc_key(id),
                serde_json::to_string(payload)?,
            )];
            // 删除旧索引中已变化的项
            for (field, old_value) in &old_index_map {
                if new_index_map.get(field) != Some(old_value) {
                    ops.push(BatchOperation::delete(adapter.index_key(field, old_value, id)));
                }
            }
            // 新增索引项
            for (field, new_value) in &new_index_map {
                if old_index_map.get(field) != Some(new_value) {
                    ops.push(BatchOperation::put(adapter.index_key(field, new_value, id), ""));
                }
            }
            let meta = DocMeta {
                vv: remote_meta.vv.clone(),
                ts: remote_meta.ts,
                ..DocMeta::default()
            };
            let meta_value = serde_json::to_value(&meta)?;
            ops.push(BatchOperation::put(
                meta_key(domain, collection, id),
                serde_json::to_string(&meta)?,
            ));
            if policy.enable_evidence {
                let entry = build_next_evidence_entry(
                    storage,
                    NewEvidenceEntry::from_parts(
                        domain,
                        collection,
                        id,
                        EvidenceOp::Put,
                        Some(payload),
                        Some(&meta_value),
                        now_ms,
                        remote_meta.node_id.as_deref().unwrap_or("remote-node"),
                    ),
                )?;
                ops.extend(evidence_batch_operations(&entry)?);
            }
            storage.batch(ops)?;
        }
    }
    Ok(())
}
