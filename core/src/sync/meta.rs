//! 同步元数据与版本向量原语（desktop/src/main/db/sync.ts）。
//!
//! meta 形状：`{vv: {nodeId: counter}, ts: number, nodeId?}`，
//! tombstone：`{vv, ts, tombstone: true}`，存储键 `meta:{domain}:{collection}:{id}`。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::storage::StorageBackend;

/// 版本向量：nodeId → 逻辑计数器。
pub type VersionVector = BTreeMap<String, i64>;

/// 文档同步 meta（持久化形态）。
///
/// 序列化时 `nodeId`/`tombstone` 缺省不输出，对齐 TS 各写入路径：
/// `{vv, ts}`（常规）、`{vv, ts, nodeId}`（本地写入）、`{vv, ts, tombstone: true}`（墓碑）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMeta {
    /// 版本向量。
    #[serde(default)]
    pub vv: VersionVector,
    /// 最后写入时间戳（ms）。
    #[serde(default)]
    pub ts: i64,
    /// 最后写入节点 id（仅本地写入路径携带）。
    #[serde(rename = "nodeId", default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// 墓碑标记（仅删除路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tombstone: Option<bool>,
}

/// 同步消息携带的远端 meta：`{vv, ts, nodeId?}`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMeta {
    /// 版本向量。
    #[serde(default)]
    pub vv: VersionVector,
    /// 远端写入时间戳（ms）。
    #[serde(default)]
    pub ts: i64,
    /// 远端节点 id。
    #[serde(rename = "nodeId", default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

/// 版本向量比较结果 / LWW 裁决结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareResult {
    /// 本地领先（或本地时间戳更大）。
    Local,
    /// 远端领先（或远端时间戳更大）。
    Remote,
    /// 并发（各有更大的分量）。
    Concurrent,
    /// 相等。
    Equal,
}

impl CompareResult {
    /// TS 字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Concurrent => "concurrent",
            Self::Equal => "equal",
        }
    }
}

/// `metaKey`：`meta:{domain}:{collection}:{id}`。
pub fn meta_key(domain: &str, collection: &str, id: &str) -> String {
    format!("meta:{domain}:{collection}:{id}")
}

/// `getMeta`：读取 meta；缺失或损坏时返回 `Ok(None)`。
pub fn get_meta<S: StorageBackend>(
    storage: &S,
    domain: &str,
    collection: &str,
    id: &str,
) -> crate::sync::SyncResult<Option<DocMeta>> {
    let Some(raw) = storage.get(&meta_key(domain, collection, id))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// `setMeta`：写入 meta。
pub fn set_meta<S: StorageBackend>(
    storage: &mut S,
    domain: &str,
    collection: &str,
    id: &str,
    meta: &DocMeta,
) -> crate::sync::SyncResult<()> {
    storage.put(&meta_key(domain, collection, id), &serde_json::to_string(meta)?)?;
    Ok(())
}

/// `generateUpdatedMeta`：在既有 meta 上递增本节点计数并刷新时间戳。
///
/// `now_ms` 由调用方注入（对齐 TS `Date.now()`）；既有 meta 的其余字段（如
/// tombstone）原样保留——逐行对齐 TS 行为。
pub fn generate_updated_meta<S: StorageBackend>(
    storage: &S,
    node_id: &str,
    domain: &str,
    collection: &str,
    id: &str,
    now_ms: i64,
) -> crate::sync::SyncResult<DocMeta> {
    let mut meta = get_meta(storage, domain, collection, id)?.unwrap_or_default();
    *meta.vv.entry(node_id.to_string()).or_insert(0) += 1;
    meta.ts = now_ms;
    meta.node_id = Some(node_id.to_string());
    Ok(meta)
}

/// `compareVersionVectors`：逐 key 取大比较；双 null / 双空 → `Equal`。
pub fn compare_version_vectors(
    local: Option<&VersionVector>,
    remote: Option<&VersionVector>,
) -> CompareResult {
    if local.is_none() && remote.is_none() {
        return CompareResult::Equal;
    }
    let mut local_greater = false;
    let mut remote_greater = false;
    let mut keys: BTreeMap<&str, ()> = BTreeMap::new();
    if let Some(l) = local {
        keys.extend(l.keys().map(|k| (k.as_str(), ())));
    }
    if let Some(r) = remote {
        keys.extend(r.keys().map(|k| (k.as_str(), ())));
    }
    for k in keys.into_keys() {
        let lv = local.and_then(|l| l.get(k)).copied().unwrap_or(0);
        let rv = remote.and_then(|r| r.get(k)).copied().unwrap_or(0);
        if lv > rv {
            local_greater = true;
        }
        if rv > lv {
            remote_greater = true;
        }
    }
    match (local_greater, remote_greater) {
        (true, false) => CompareResult::Local,
        (false, true) => CompareResult::Remote,
        (true, true) => CompareResult::Concurrent,
        (false, false) => CompareResult::Equal,
    }
}

/// `resolveConflictByLWW`：null 按 0；严格比较，相等 → `Equal`。
pub fn resolve_conflict_by_lww(local_ts: Option<i64>, remote_ts: Option<i64>) -> CompareResult {
    let l = local_ts.unwrap_or(0);
    let r = remote_ts.unwrap_or(0);
    if r > l {
        return CompareResult::Remote;
    }
    if l > r {
        return CompareResult::Local;
    }
    CompareResult::Equal
}

/// `mergeVersionVectors`：逐 nodeId 取 max（append-only 幂等去重后促进收敛）。
pub fn merge_version_vectors(
    local: Option<&VersionVector>,
    remote: Option<&VersionVector>,
) -> VersionVector {
    let mut merged = remote.cloned().unwrap_or_default();
    for (node_id, counter) in local.into_iter().flatten() {
        let entry = merged.entry(node_id.clone()).or_insert(0);
        *entry = (*entry).max(*counter);
    }
    merged
}
