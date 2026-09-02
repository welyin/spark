//! orgsync 摘要折叠、增量采集、墓碑采集与 diff 裁决。
//!
//! 折叠：扫描集合全部数据键域 + 集合声明记录（`org:coll:`）的 pmeta 逐条
//! merge 取 max。增量：按 knownVv 采集本地领先/并发记录 + 未确认墓碑。
//! diff：对比本地折叠 vv 与对端 hello 摘要，裁决落后/领先/并发/相等。

use serde_json::{Map, Value, json};

use crate::org::OrganizationRecord;
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::{
    CompareResult, VersionVector, compare_version_vectors, merge_version_vectors,
};

use super::builtin::collection_data_prefixes;
use super::dlog::org_dlog_entries_after;
use super::envelope::OrgsyncRecord;

// ── hello 摘要折叠 ──────────────────────────────────────────────────────

/// 收集某个 org 集合的合并折叠 vv：扫描该集合**全部数据键域**（插件 `orgd:`
/// 或内建存量键前缀）下各记录的 pmeta，逐条 merge 取 max。
///
/// F2-P1：声明（`org:coll:`）与授权名单（`org:acl:`）已并入 org:structure
/// 键域（builtin.rs），随 all-members 集合折叠——不再随所属集合流量
/// per-collection 携带（避免双通道重复折叠）。
pub fn collect_org_collection_vv<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> SyncResult<VersionVector> {
    let mut folded = VersionVector::new();
    for prefix in collection_data_prefixes(org_id, name, version) {
        let meta_prefix = format!("pmeta:{}", prefix);
        for (meta_key, raw) in storage.scan(&ScanOptions::prefix(&meta_prefix))? {
            let Some(record_key) = meta_key.strip_prefix("pmeta:") else {
                continue;
            };
            if !record_key.starts_with(&prefix) {
                continue;
            }
            if let Ok(meta) = serde_json::from_str::<crate::sync::meta::DocMeta>(&raw) {
                folded = merge_version_vectors(Some(&folded), Some(&meta.vv));
            }
        }
    }
    Ok(folded)
}

/// 构建所有 org 集合的折叠 vv 摘要（hello 的 `collections` 字段）。
/// 返回 `{ collection_name: { vv, dlogAck } }`。
///
/// **按收件人逐成员生成**（B2）：`dlogAck` = 我已收讫**对端该集合删除日志**
/// 的最大序号 = `org_dlog_get_seen(recipient)`（按 (rootId, peerId) 设备粒度）。
/// 群发时不得克隆同一 body——每收件人的 dlogAck 不同。
pub fn collect_org_collections<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    // 每个集合的 (name, version) 列表
    collections: &[(String, String)],
    recipient_root_id: &str,
    recipient_peer_id: &str,
) -> SyncResult<Map<String, Value>> {
    let mut map = Map::new();
    for (name, version) in collections {
        let vv = collect_org_collection_vv(storage, org_id, name, version)?;
        let dlog_ack = super::dlog::org_dlog_get_seen(
            storage,
            org_id,
            name,
            version,
            recipient_root_id,
            recipient_peer_id,
        )?;
        map.insert(
            format!("{name}@v{version}"),
            json!({ "vv": vv, "dlogAck": dlog_ack }),
        );
    }
    Ok(map)
}

// ── diff 裁决 ────────────────────────────────────────────────────────────

/// 一个 org 集合的 diff 结论。
#[derive(Clone, Debug)]
pub enum OrgDiffOutcome {
    /// 本地落后：发 `orgsync-need`。
    LocalBehind { local_vv: VersionVector },
    /// 本地领先：主动推 `orgsync-data`。
    LocalAhead,
    /// 并发：双向交换。
    Concurrent,
    /// 相等：不动。
    Equal,
}

/// 对比本地折叠 vv 与对端 hello 摘要中的折叠 vv。
pub fn diff_org_collection(local_vv: &VersionVector, remote_vv: &VersionVector) -> OrgDiffOutcome {
    match compare_version_vectors(Some(local_vv), Some(remote_vv)) {
        CompareResult::Remote => OrgDiffOutcome::LocalBehind {
            local_vv: local_vv.clone(),
        },
        CompareResult::Local => OrgDiffOutcome::LocalAhead,
        CompareResult::Concurrent => OrgDiffOutcome::Concurrent,
        CompareResult::Equal => OrgDiffOutcome::Equal,
    }
}

// ── 增量采集 ─────────────────────────────────────────────────────────────

/// 按 `knownVv` 采集 org 集合增量（need 的处理）：扫描该集合全部数据键域
/// 记录，凡本地 pmeta 相对 knownVv 不是 `Remote`/`Equal` → 纳入返回。
pub fn collect_org_incremental<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    known_vv: &VersionVector,
    dlog_ack: u64,
) -> SyncResult<Vec<OrgsyncRecord>> {
    let mut records = Vec::new();
    for prefix in collection_data_prefixes(org_id, name, version) {
        for (key, raw_value) in storage.scan(&ScanOptions::prefix(&prefix))? {
            let meta = match crate::sync::get_personal_meta(storage, &key)? {
                Some(m) => m,
                None => continue,
            };
            if crate::sync::is_tombstone(&meta) {
                continue;
            }
            match compare_version_vectors(Some(&meta.vv), Some(known_vv)) {
                CompareResult::Remote | CompareResult::Equal => continue,
                _ => {
                    let value = match serde_json::from_str(&raw_value) {
                        Ok(v) => v,
                        Err(error) => {
                            eprintln!("[orgsync] skip corrupted record {key}: {error}");
                            continue;
                        }
                    };
                    records.push(OrgsyncRecord {
                        key,
                        value,
                        meta,
                        dseq: None,
                    });
                }
            }
        }
    }
    // B5/F2-P1：集合声明（org:coll:）与授权名单（org:acl:）已并入
    // org:structure 键域（builtin.rs），随 all-members 集合的折叠/增量全员
    // 同步——不再随所属集合流量 per-collection 携带（避免双通道重复；
    // 声明收敛合入见 inbound_dm/orgsync.rs 的 decl 分支）。
    // 墓碑增量：删除日志驱动
    records.extend(collect_org_tombstones_after(
        storage, org_id, name, version, dlog_ack,
    )?);
    Ok(records)
}

/// 采集 org 域未确认墓碑。
pub fn collect_org_tombstones_after<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    dlog_ack: u64,
) -> SyncResult<Vec<OrgsyncRecord>> {
    let data_prefixes = collection_data_prefixes(org_id, name, version);
    let decl_key = crate::plugindata::org_decl_key(org_id, name, version);
    let acl_key = super::access::acl_key(org_id, name, version);
    let mut records = Vec::new();
    for (seq, record_key) in org_dlog_entries_after(storage, org_id, name, version, dlog_ack)? {
        // B5：数据键（任一数据键域前缀）与声明键（org:coll:）/授权名单键
        // （org:acl:，O4）的墓碑都算该集合删除日志。
        let in_data_domain = data_prefixes.iter().any(|p| record_key.starts_with(p));
        if !in_data_domain && record_key != decl_key && record_key != acl_key {
            continue;
        }
        let Ok(Some(meta)) = crate::sync::get_personal_meta(storage, &record_key) else {
            continue;
        };
        if !crate::sync::is_tombstone(&meta) {
            continue;
        }
        records.push(OrgsyncRecord {
            key: record_key,
            value: Value::Null,
            meta,
            dseq: Some(seq),
        });
    }
    Ok(records)
}

// ── 角色履职声明 ────────────────────────────────────────────────────────

/// 构建本机角色履职声明（用于 orgsync-hello 的 `roles` 字段）。
/// 返回如 `["data", "gateway"]`。
pub fn self_roles(record: &OrganizationRecord, self_root_id: &str, now_ms: i64) -> Vec<String> {
    let mut roles = Vec::new();
    if crate::org::roles::is_data_account(record, self_root_id) {
        roles.push("data".to_string());
    }
    if crate::org::roles::is_gateway_active(record, self_root_id, now_ms) {
        roles.push("gateway".to_string());
    }
    roles
}
