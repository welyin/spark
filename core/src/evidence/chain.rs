//! 存证链：哈希构建、链式追加与校验（desktop/src/main/db/evidence.ts）。
//!
//! 规格见 core/spec/sync-evidence.md §2；验收向量见 vectors/sync-evidence.json。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::canonical::normalize_object;
use crate::storage::{BatchOperation, StorageBackend};

/// 存证条目存储前缀。
pub const EVIDENCE_PREFIX: &str = "doc:evidence:proof:";
/// 存证链头指针键。
pub const EVIDENCE_HEAD_KEY: &str = "doc:evidence:head";

/// 存证模块错误。
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 存证模块 Result 别名。
pub type Result<T> = std::result::Result<T, EvidenceError>;

/// 存证操作类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceOp {
    /// 写入。
    Put,
    /// 删除。
    Delete,
}

impl EvidenceOp {
    /// TS 字符串形式（`"put"` / `"delete"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Delete => "delete",
        }
    }
}

/// 存证链条目。序列化字段名/顺序对齐 TS `JSON.stringify(entry)`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceEntry {
    /// 链内序号（从 1 递增）。
    pub seq: u64,
    /// 前一条目 hash，首条为 null。
    pub prev_hash: Option<String>,
    /// 数据域。
    pub domain: String,
    /// 集合名。
    pub collection: String,
    /// 文档 id。
    pub id: String,
    /// 操作类型。
    pub op: EvidenceOp,
    /// 数据哈希（domain/collection/id/op/payloadHash/metaHash 的复合哈希）。
    pub data_hash: String,
    /// 载荷哈希（payload 为 null/undefined 时为 null）。
    pub payload_hash: Option<String>,
    /// meta 哈希（meta 为 null/undefined 时为 null）。
    pub meta_hash: Option<String>,
    /// 条目哈希（缺省为空，便于先占位后计算）。
    #[serde(default)]
    pub hash: String,
    /// 写入时间戳（ms）。
    pub timestamp: i64,
    /// 写入节点 id。
    pub node_id: String,
}

/// 新建存证条目的输入（不含 seq/prevHash/hash，对齐 TS `Omit<EvidenceEntry, 'seq'|'hash'|'prevHash'>`）。
#[derive(Clone, Debug)]
pub struct NewEvidenceEntry {
    /// 数据域。
    pub domain: String,
    /// 集合名。
    pub collection: String,
    /// 文档 id。
    pub id: String,
    /// 操作类型。
    pub op: EvidenceOp,
    /// 数据哈希。
    pub data_hash: String,
    /// 载荷哈希。
    pub payload_hash: Option<String>,
    /// meta 哈希。
    pub meta_hash: Option<String>,
    /// 写入时间戳（ms）。
    pub timestamp: i64,
    /// 写入节点 id。
    pub node_id: String,
}

impl NewEvidenceEntry {
    /// 由原始 payload/meta 计算各哈希后构造（标准用法，对齐 TS 各调用点的组合方式）。
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        domain: impl Into<String>,
        collection: impl Into<String>,
        id: impl Into<String>,
        op: EvidenceOp,
        payload: Option<&Value>,
        meta: Option<&Value>,
        timestamp: i64,
        node_id: impl Into<String>,
    ) -> Self {
        let domain = domain.into();
        let collection = collection.into();
        let id = id.into();
        let payload_hash = build_evidence_payload_hash(payload);
        let meta_hash = build_evidence_meta_hash(meta);
        let data_hash = build_evidence_data_hash(
            &domain,
            &collection,
            &id,
            op,
            payload_hash.as_deref(),
            meta_hash.as_deref(),
        );
        Self {
            domain,
            collection,
            id,
            op,
            data_hash,
            payload_hash,
            meta_hash,
            timestamp,
            node_id: node_id.into(),
        }
    }
}

/// 链头指针 `{seq, hash}`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceHead {
    /// 当前链高。
    pub seq: u64,
    /// 链头条目 hash。
    pub hash: String,
}

/// sha256 hex 摘要。
pub fn sha256_hex(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// `buildEvidencePayloadHash`：payload 为 null/undefined → `None`（短路，不哈希）。
pub fn build_evidence_payload_hash(payload: Option<&Value>) -> Option<String> {
    payload.map(|p| sha256_hex(&normalize_object(p)))
}

/// `buildEvidenceMetaHash`：meta 为 null/undefined → `None`（短路，不哈希）。
pub fn build_evidence_meta_hash(meta: Option<&Value>) -> Option<String> {
    meta.map(|m| sha256_hex(&normalize_object(m)))
}

/// `buildEvidenceDataHash`：sha256(normalizeObject({domain, collection, id, op, payloadHash, metaHash}))。
pub fn build_evidence_data_hash(
    domain: &str,
    collection: &str,
    id: &str,
    op: EvidenceOp,
    payload_hash: Option<&str>,
    meta_hash: Option<&str>,
) -> String {
    let payload = json!({
        "domain": domain,
        "collection": collection,
        "id": id,
        "op": op.as_str(),
        "payloadHash": payload_hash,
        "metaHash": meta_hash,
    });
    sha256_hex(&normalize_object(&payload))
}

/// `buildEvidenceEntryHash`：对条目除 `hash` 外的 11 个字段做 normalize + sha256。
pub fn build_evidence_entry_hash(entry: &EvidenceEntry) -> String {
    let payload = json!({
        "seq": entry.seq,
        "prevHash": entry.prev_hash,
        "domain": entry.domain,
        "collection": entry.collection,
        "id": entry.id,
        "op": entry.op.as_str(),
        "dataHash": entry.data_hash,
        "payloadHash": entry.payload_hash,
        "metaHash": entry.meta_hash,
        "timestamp": entry.timestamp,
        "nodeId": entry.node_id,
    });
    sha256_hex(&normalize_object(&payload))
}

/// `evidenceKey`：`doc:evidence:proof:{seq 左补零至 12 位}`。
pub fn evidence_key(seq: u64) -> String {
    format!("{EVIDENCE_PREFIX}{seq:012}")
}

/// `getEvidenceHead`：读取链头；缺失或损坏时返回 `Ok(None)`。
pub fn get_evidence_head<S: StorageBackend>(storage: &S) -> Result<Option<EvidenceHead>> {
    let Some(raw) = storage.get(EVIDENCE_HEAD_KEY)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// `buildNextEvidenceEntry`：据链头推导 seq/prevHash 并计算条目 hash（不落盘）。
pub fn build_next_evidence_entry<S: StorageBackend>(
    storage: &S,
    entry: NewEvidenceEntry,
) -> Result<EvidenceEntry> {
    let head = get_evidence_head(storage)?;
    let seq = head.as_ref().map_or(1, |h| h.seq + 1);
    let prev_hash = head.map(|h| h.hash);
    let mut new_entry = EvidenceEntry {
        seq,
        prev_hash,
        domain: entry.domain,
        collection: entry.collection,
        id: entry.id,
        op: entry.op,
        data_hash: entry.data_hash,
        payload_hash: entry.payload_hash,
        meta_hash: entry.meta_hash,
        hash: String::new(),
        timestamp: entry.timestamp,
        node_id: entry.node_id,
    };
    new_entry.hash = build_evidence_entry_hash(&new_entry);
    Ok(new_entry)
}

/// `evidenceBatchOperations`：条目 + 头指针的批量操作。
pub fn evidence_batch_operations(entry: &EvidenceEntry) -> Result<Vec<BatchOperation>> {
    Ok(vec![
        BatchOperation::put(evidence_key(entry.seq), serde_json::to_string(entry)?),
        BatchOperation::put(
            EVIDENCE_HEAD_KEY,
            serde_json::to_string(&EvidenceHead {
                seq: entry.seq,
                hash: entry.hash.clone(),
            })?,
        ),
    ])
}

/// `appendEvidence`：构建条目并批量落盘（条目 + 头指针）。
pub fn append_evidence<S: StorageBackend>(
    storage: &mut S,
    entry: NewEvidenceEntry,
) -> Result<EvidenceEntry> {
    let new_entry = build_next_evidence_entry(storage, entry)?;
    let ops = evidence_batch_operations(&new_entry)?;
    storage.batch(ops)?;
    Ok(new_entry)
}

/// `getEvidenceEntry`：按 seq 读取条目；缺失或损坏时返回 `Ok(None)`。
pub fn get_evidence_entry<S: StorageBackend>(storage: &S, seq: u64) -> Result<Option<EvidenceEntry>> {
    let Some(raw) = storage.get(&evidence_key(seq))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// `verifyEvidenceChain`：从 1 遍历到 head.seq，逐条验 prevHash 与重算 hash。
pub fn verify_evidence_chain<S: StorageBackend>(storage: &S) -> Result<bool> {
    let Some(head) = get_evidence_head(storage)? else {
        return Ok(true);
    };
    let mut prev_hash: Option<String> = None;
    for seq in 1..=head.seq {
        let Some(entry) = get_evidence_entry(storage, seq)? else {
            return Ok(false);
        };
        if entry.prev_hash != prev_hash {
            return Ok(false);
        }
        if entry.hash != build_evidence_entry_hash(&entry) {
            return Ok(false);
        }
        prev_hash = Some(entry.hash);
    }
    Ok(true)
}

/// `getEvidenceHeadHash`：链头 hash，空链返回 `None`。
pub fn get_evidence_head_hash<S: StorageBackend>(storage: &S) -> Result<Option<String>> {
    Ok(get_evidence_head(storage)?.map(|h| h.hash))
}

/// `getEvidenceHeight`：链高，空链返回 0。
pub fn get_evidence_height<S: StorageBackend>(storage: &S) -> Result<u64> {
    Ok(get_evidence_head(storage)?.map_or(0, |h| h.seq))
}

/// `verifyEvidenceHashMatchesRemote`：本地链头 hash 与远端比较。
pub fn verify_evidence_hash_matches_remote<S: StorageBackend>(
    storage: &S,
    remote_hash: &str,
) -> Result<bool> {
    Ok(get_evidence_head_hash(storage)?.as_deref() == Some(remote_hash))
}
