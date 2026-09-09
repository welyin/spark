//! A1 blob 层：内容寻址分块 + presence 副本账本 + 按需回补（个人域 K=3 模型）。
//!
//! 字节级权威：`wiki/protocol/p2p/personal-data-sync.md` §14；设计权威：
//! `docs/architecture/foundation/personal-data.md` §4.1–4.3。
//!
//! - 寻址：`cid = sha256hex(blob 内容)`；>1 MiB 切 256 KiB 定长 chunk，
//!   `chunkCid = sha256hex(chunk)`；manifest = `{v, cid, size, chunkSize, chunkCids[]}`；
//! - 存储键：`blob:meta:{cid}` / `blob:chunk:{chunkCid}`（本地键，不走反熵），
//!   `blob:presence:{cid}:{deviceUid}`（副本账本，纳入 pdsync 同步——见
//!   [`super::pdsync::CATEGORIES`] 的 `blob:presence` category）；
//! - 消息/文件记录只存 cid 引用；blob 层是叠加在 pdsync 核心数据通道之上的
//!   独立内容寻址层，既有通道零改动；
//! - 配额与驱逐属 A2：[`drop_local_chunks`] 是驱逐选择器的执行原语，
//!   [`presence`] 的逐块副本计数是其选择输入（副本 ≤K 永不驱逐在 A2 强制）。

pub mod evict;
mod fetch;
pub mod health;
pub mod migrate;
mod presence;
pub mod quota;

pub use evict::{
    EvictionReport, EvictionState, blob_evicted_key, clear_evicted, collect_eviction_states,
    evict_over_quota, gc_unreferenced, holder_rank, is_evicted, list_local_blobs, mark_evicted,
    plan_eviction, plan_gc, throttle_evict,
};
pub use fetch::{
    ChunkFetch, FetchPlan, FetchResponse, FetchTarget, IngestOutcome, ReadOutcome,
    build_fetch_body, ingest_chunk, ingest_manifest, parse_chunk_body, parse_fetch_body,
    plan_fetch, read_or_plan, record_holds_chunk, serve_fetch, throttle_fetch,
};
pub use health::{BlobHealth, QuotaStatusView, blob_health};
pub use migrate::{collect_references, reconcile_registrations, throttle_gc};
pub use presence::{
    PRESENCE_KEY_PREFIX, PresenceRecord, bitmap_decode, bitmap_encode, chunk_replica_counts,
    full_replica_count, list_presence, presence_key, presence_prefix_for, refresh_presence,
    replica_summary,
};
pub use quota::{
    BLOB_QUOTA_KEY, DEFAULT_QUOTA_MOBILE_BYTES, DEFAULT_QUOTA_PC_BYTES, K_REPLICAS,
    QuotaStatus, active_device_count, blob_access_key, blob_usage, get_access, get_blob_quota,
    k_target, quota_status, remote_blob_quota_key, set_blob_quota, touch_access,
};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::SyncResult;
use crate::storage::{BatchOperation, StorageBackend};

/// blob 层错误。
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// manifest 结构校验失败（§14.2）。
    #[error("blob manifest invalid: {0}")]
    InvalidManifest(String),
    /// 块/整体完整性校验失败（sha256 与寻址不符）。
    #[error("blob integrity mismatch: {0}")]
    Integrity(String),
    /// 本地数据损坏（装配终验失败）。
    #[error("blob corrupt: {0}")]
    Corrupt(String),
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 分块阈值：≤1 MiB 的 blob 为单块（chunkCid == cid）。
pub const CHUNK_THRESHOLD_BYTES: usize = 1024 * 1024;
/// 定长块：>1 MiB 的 blob 按 256 KiB 切分。
pub const CHUNK_SIZE_BYTES: usize = 256 * 1024;
/// 传输切片：单信封 data ≤ 240 KiB 原始字节（3 的倍数，base64 可直接拼接；
/// dm 单帧 1 MiB 约束，与 P6 `BLOB_CHUNK_BYTES` 同口径）。
pub const TRANSFER_SLICE_BYTES: usize = 240 * 1024;
/// 同一 chunkCid 拉取请求的最小间隔（防丢帧重试由读路径重规划驱动，节流防抖）。
pub const FETCH_THROTTLE_MS: i64 = 30_000;

/// manifest 键（本地键，不进同步流量）。
pub fn blob_meta_key(cid: &str) -> String {
    format!("blob:meta:{cid}")
}

/// 块内容键（本地键；值为 base64，storage 后端是 String 值，对齐
/// `plugindata::blob::save_blob` 的编码口径）。
pub fn blob_chunk_key(chunk_cid: &str) -> String {
    format!("blob:chunk:{chunk_cid}")
}

/// 装配中暂存键（base64 追加式拼接；本地键）。
pub fn blob_asm_key(chunk_cid: &str) -> String {
    format!("blob:asm:{chunk_cid}")
}

/// 拉取节流键（值 = 上次请求时间戳 ms；本地键）。
pub fn blob_freq_key(chunk_cid: &str) -> String {
    format!("blob:freq:{chunk_cid}")
}

/// sha256 小写 hex（cid/chunkCid 的统一推导）。
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

/// 64 位小写 hex 校验（cid/chunkCid 线形约束）。
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 分块规则（§14.1）：0 → 0 块；≤ 阈值 → 单块；> 阈值 → ceil(size / 256KiB)。
pub fn expected_chunk_count(size: u64) -> u64 {
    if size == 0 {
        0
    } else if size <= CHUNK_THRESHOLD_BYTES as u64 {
        1
    } else {
        size.div_ceil(CHUNK_SIZE_BYTES as u64)
    }
}

/// blob manifest（serde 字段序即线形字节序：v, cid, size, chunkSize, chunkCids）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobManifest {
    pub v: u32,
    pub cid: String,
    pub size: u64,
    pub chunk_size: u64,
    pub chunk_cids: Vec<String>,
}

impl BlobManifest {
    /// 规范 JSON（紧凑、字段序固定）——`blob:meta:{cid}` 的存储值与
    /// manifest 应答的逐字节形态。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 解析并做结构校验（§14.2）；任一不符返回 None。
    pub fn from_json(raw: &str) -> Option<Self> {
        let m: Self = serde_json::from_str(raw).ok()?;
        validate_manifest(&m).ok()?;
        Some(m)
    }

    /// 块数。
    pub fn chunk_count(&self) -> usize {
        self.chunk_cids.len()
    }
}

/// manifest 结构校验（§14.2 入库前必过）。
pub fn validate_manifest(m: &BlobManifest) -> Result<(), BlobError> {
    if m.v != 1 {
        return Err(BlobError::InvalidManifest(format!("v={}", m.v)));
    }
    if !is_hex64(&m.cid) {
        return Err(BlobError::InvalidManifest("cid not hex64".to_string()));
    }
    if m.chunk_size != CHUNK_SIZE_BYTES as u64 {
        return Err(BlobError::InvalidManifest(format!(
            "chunkSize={}",
            m.chunk_size
        )));
    }
    if m.chunk_cids.len() as u64 != expected_chunk_count(m.size) {
        return Err(BlobError::InvalidManifest(format!(
            "chunkCids len {} != expected {} for size {}",
            m.chunk_cids.len(),
            expected_chunk_count(m.size),
            m.size
        )));
    }
    for c in &m.chunk_cids {
        if !is_hex64(c) {
            return Err(BlobError::InvalidManifest("chunkCid not hex64".to_string()));
        }
    }
    // 单块恒等式：0 < size ≤ 阈值 → chunkCids[0] == cid
    if m.size > 0 && m.size <= CHUNK_THRESHOLD_BYTES as u64 && m.chunk_cids[0] != m.cid {
        return Err(BlobError::InvalidManifest(
            "single-chunk blob must have chunkCids[0] == cid".to_string(),
        ));
    }
    Ok(())
}

/// 由内容推导 manifest（cid + 逐块 chunkCid，纯函数）。
pub fn build_manifest(data: &[u8]) -> BlobManifest {
    let cid = sha256_hex(data);
    let size = data.len() as u64;
    let chunk_cids = if data.is_empty() {
        Vec::new()
    } else if data.len() <= CHUNK_THRESHOLD_BYTES {
        vec![cid.clone()]
    } else {
        data.chunks(CHUNK_SIZE_BYTES).map(sha256_hex).collect()
    };
    BlobManifest {
        v: 1,
        cid,
        size,
        chunk_size: CHUNK_SIZE_BYTES as u64,
        chunk_cids,
    }
}

/// 读 manifest（缺失/损坏 → None）。
pub fn get_manifest<S: StorageBackend>(storage: &S, cid: &str) -> SyncResult<Option<BlobManifest>> {
    let Some(raw) = storage.get(&blob_meta_key(cid))? else {
        return Ok(None);
    };
    Ok(BlobManifest::from_json(&raw))
}

/// 块是否已在本地。
pub fn has_chunk<S: StorageBackend>(storage: &S, chunk_cid: &str) -> bool {
    storage
        .get(&blob_chunk_key(chunk_cid))
        .ok()
        .flatten()
        .is_some()
}

/// 本机持有位图（按 manifest 顺序：true = 持有该块）。
pub fn held_chunk_indices<S: StorageBackend>(
    storage: &S,
    manifest: &BlobManifest,
) -> SyncResult<Vec<bool>> {
    let mut held = Vec::with_capacity(manifest.chunk_cids.len());
    for chunk_cid in &manifest.chunk_cids {
        held.push(has_chunk(storage, chunk_cid));
    }
    Ok(held)
}

/// 保存 blob：推导 manifest，落 `blob:meta:` + 全部 `blob:chunk:`（同一 batch），
/// 再刷新 presence（持有即做种 + 计入副本账本）。
///
/// 幂等：同内容同 cid，重写无害。
pub fn save_blob<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    data: &[u8],
    now_ms: i64,
) -> SyncResult<BlobManifest> {
    let manifest = build_manifest(data);
    let mut ops = vec![BatchOperation::put(
        blob_meta_key(&manifest.cid),
        manifest.to_json(),
    )];
    if data.len() <= CHUNK_THRESHOLD_BYTES {
        if !data.is_empty() {
            ops.push(BatchOperation::put(
                blob_chunk_key(&manifest.cid),
                B64.encode(data),
            ));
        }
    } else {
        for (i, chunk) in data.chunks(CHUNK_SIZE_BYTES).enumerate() {
            ops.push(BatchOperation::put(
                blob_chunk_key(&manifest.chunk_cids[i]),
                B64.encode(chunk),
            ));
        }
    }
    storage.batch(ops)?;
    refresh_presence(storage, node_id, device_uid, &manifest.cid, now_ms)?;
    quota::touch_access(storage, &manifest.cid, now_ms)?;
    // 同 cid 重新写入 = 显式意图，解除驱逐标记（§16.4）
    evict::clear_evicted(storage, &manifest.cid)?;
    Ok(manifest)
}

/// 装配读出（只读，不刷新 access）：本地齐块 → 按 manifest 拼接 →
/// `sha256(拼接) == cid` 终验。manifest 缺失或缺块 → `Ok(None)`。
/// 终验失败（本地数据损坏）→ `BlobError::Corrupt`（不静默返回假数据）。
///
/// 供 `plugindata::blob` 读穿回退（`&S` 上下文，A4 §16.2）等只读场景；
/// 读路径统一入口请用 [`read_blob`]（读出即刷新 LRU 龄期）。
pub fn read_blob_quiet<S: StorageBackend>(storage: &S, cid: &str) -> SyncResult<Option<Vec<u8>>> {
    let Some(manifest) = get_manifest(storage, cid)? else {
        return Ok(None);
    };
    let held = held_chunk_indices(storage, &manifest)?;
    if held.iter().any(|h| !h) {
        return Ok(None);
    }
    let mut data = Vec::with_capacity(manifest.size as usize);
    for chunk_cid in &manifest.chunk_cids {
        let Some(b64) = storage.get(&blob_chunk_key(chunk_cid))? else {
            return Ok(None);
        };
        let chunk = B64
            .decode(&b64)
            .map_err(|e| BlobError::Corrupt(format!("chunk {chunk_cid} base64: {e}")))?;
        data.extend_from_slice(&chunk);
    }
    if data.len() as u64 != manifest.size || sha256_hex(&data) != cid {
        return Err(BlobError::Corrupt(format!(
            "assembly verification failed for cid {cid}"
        ))
        .into());
    }
    Ok(Some(data))
}

/// 装配读出：[`read_blob_quiet`] + 读出成功即刷新 `blob:access:`
/// （LRU 龄期语义，A2 驱逐选择器输入）。
pub fn read_blob<S: StorageBackend>(
    storage: &mut S,
    cid: &str,
    now_ms: i64,
) -> SyncResult<Option<Vec<u8>>> {
    let Some(data) = read_blob_quiet(storage, cid)? else {
        return Ok(None);
    };
    quota::touch_access(storage, cid, now_ms)?;
    Ok(Some(data))
}

/// 层内完整持有判定（manifest 在 + 全部 chunk 键在；不装配不校验，
/// 供 `plugindata::blob::has_blob` 读穿的轻量探测）。
pub fn has_blob_complete<S: StorageBackend>(storage: &S, cid: &str) -> bool {
    let Ok(Some(manifest)) = get_manifest(storage, cid) else {
        return false;
    };
    manifest.chunk_cids.iter().all(|c| has_chunk(storage, c))
}

/// 弃块原语（A2 驱逐选择器的执行面；A1 用于「驱逐后回补」链路与本机主动释放）：
/// 删除该 blob 的全部 `blob:chunk:` 与装配暂存，**manifest 不动**（可随时回补），
/// 然后刷新 presence（位图全零 → 记录墓碑化随 dlog 传播移除）。
///
/// 返回删除的块数。manifest 缺失时无法枚举 chunkCids，返回 0（无操作）。
pub fn drop_local_chunks<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    cid: &str,
    now_ms: i64,
) -> SyncResult<usize> {
    let Some(manifest) = get_manifest(storage, cid)? else {
        return Ok(0);
    };
    let mut dropped = 0usize;
    let mut ops = Vec::new();
    for chunk_cid in &manifest.chunk_cids {
        let key = blob_chunk_key(chunk_cid);
        if storage.get(&key)?.is_some() {
            ops.push(BatchOperation::delete(key));
            dropped += 1;
        }
        // 中断的装配一并清理（等不到下一块永无意义）
        let asm_key = blob_asm_key(chunk_cid);
        if storage.get(&asm_key)?.is_some() {
            ops.push(BatchOperation::delete(asm_key));
        }
    }
    if !ops.is_empty() {
        storage.batch(ops)?;
    }
    storage.delete(&quota::blob_access_key(cid))?;
    refresh_presence(storage, node_id, device_uid, cid, now_ms)?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// 确定性内容（与 spec/vectors/blob.json 同规则）。
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn chunking_boundaries() {
        assert_eq!(expected_chunk_count(0), 0);
        assert_eq!(expected_chunk_count(1), 1);
        assert_eq!(
            expected_chunk_count(CHUNK_THRESHOLD_BYTES as u64 - 1),
            1,
            "阈值-1 单块"
        );
        assert_eq!(
            expected_chunk_count(CHUNK_THRESHOLD_BYTES as u64),
            1,
            "恰阈值单块"
        );
        assert_eq!(
            expected_chunk_count(CHUNK_THRESHOLD_BYTES as u64 + 1),
            5,
            "阈值+1 → 4×256KiB + 1B"
        );
    }

    #[test]
    fn manifest_build_and_canonical_json() {
        // 空 blob
        let m = build_manifest(&[]);
        assert_eq!(m.chunk_cids, Vec::<String>::new());
        assert_eq!(
            m.cid,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            m.to_json(),
            format!(
                "{{\"v\":1,\"cid\":\"{}\",\"size\":0,\"chunkSize\":262144,\"chunkCids\":[]}}",
                m.cid
            )
        );
        // 小 blob：单块且 chunkCid == cid
        let data = pattern(1000);
        let m = build_manifest(&data);
        assert_eq!(m.chunk_cids, vec![m.cid.clone()]);
        assert!(validate_manifest(&m).is_ok());
        // 规范 JSON 往返逐字节
        assert_eq!(BlobManifest::from_json(&m.to_json()).unwrap(), m);
        // 多分块
        let big = pattern(CHUNK_THRESHOLD_BYTES + 1);
        let m = build_manifest(&big);
        assert_eq!(m.chunk_cids.len(), 5);
        assert_eq!(m.chunk_cids[4], sha256_hex(&big[CHUNK_THRESHOLD_BYTES..]));
        assert!(validate_manifest(&m).is_ok());
    }

    #[test]
    fn manifest_validation_rejects_bad_shapes() {
        let data = pattern(100);
        let good = build_manifest(&data);
        let cases = [
            BlobManifest { v: 2, ..good.clone() },
            BlobManifest { cid: "ZZ".to_string(), ..good.clone() },
            BlobManifest { chunk_size: 1, ..good.clone() },
            BlobManifest { chunk_cids: vec![], ..good.clone() },
            BlobManifest {
                chunk_cids: vec!["a".repeat(64)],
                ..good.clone()
            },
        ];
        for (i, m) in cases.iter().enumerate() {
            assert!(validate_manifest(m).is_err(), "case {i} 应拒");
            assert!(BlobManifest::from_json(&m.to_json()).is_none());
        }
    }

    #[test]
    fn save_read_drop_roundtrip() {
        let mut s = MemoryStorage::new();
        let data = pattern(CHUNK_THRESHOLD_BYTES + 123);
        let m = save_blob(&mut s, "node-a", "uid-a", &data, 1000).unwrap();
        assert_eq!(m.chunk_cids.len(), 5);
        // 读回一致；读出即刷新 access（LRU 龄期）
        assert_eq!(
            read_blob(&mut s, &m.cid, 3000).unwrap().as_deref(),
            Some(data.as_slice())
        );
        assert_eq!(quota::get_access(&s, &m.cid).unwrap(), Some(3000));
        // 弃块：chunk 删除、manifest 保留、presence 墓碑化
        let dropped = drop_local_chunks(&mut s, "node-a", "uid-a", &m.cid, 2000).unwrap();
        assert_eq!(dropped, 5);
        assert!(get_manifest(&s, &m.cid).unwrap().is_some(), "manifest 不动");
        assert!(read_blob(&mut s, &m.cid, 4000).unwrap().is_none(), "缺块不可读");
        assert!(
            quota::get_access(&s, &m.cid).unwrap().is_none(),
            "弃块清除 access 键"
        );
        for c in &m.chunk_cids {
            assert!(!has_chunk(&s, c));
        }
    }

    #[test]
    fn read_rejects_corrupt_assembly() {
        let mut s = MemoryStorage::new();
        let data = pattern(1000);
        let m = save_blob(&mut s, "node-a", "uid-a", &data, 1000).unwrap();
        // 篡改存盘块（键不变、内容换掉）
        s.put(&blob_chunk_key(&m.cid), &B64.encode(b"tampered")).unwrap();
        assert!(matches!(
            read_blob(&mut s, &m.cid, 2000),
            Err(crate::sync::SyncError::Blob(BlobError::Corrupt(_)))
        ));
    }
}
