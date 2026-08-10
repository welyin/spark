//! P6 内建 blob：内容哈希寻址的大文件存取与按需拉取。
//!
//! 设计定稿见 wiki `design/plugin-data-api.md` §4 / `personal-data-sync.md` §6.1：
//!
//! - `save_blob(bytes) → hash`：SHA-256 hex 寻址，记录内以
//!   `{"$blob": hash, "name", "size", "mime"}` 引用；
//! - 本体**不走反熵**（`blob:` 不在 pdsync category）：记录先行同步，引用
//!   对象中的 `$blob` 标记是拉取依据；
//! - 全局唯一策略：PC eager（合入/hello 调和时发现缺失即拉）、手机 lazy
//!   （`read_blob` 未命中置 want 标记，调和时拉取）；拉取后经哈希校验才可用；
//! - 传输：`pdsync-attachment-req/resp` dm 信封（自设备间，验签同 pdsync），
//!   定长分块——块长取 3 的倍数，接收侧 base64 字符串直接追加即完成拼接；
//! - 大小上限 10 MiB（与 sys.fetch 同口径）；GC（无引用回收）后续切片。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::storage::{ScanOptions, StorageBackend};

use super::{PlugindataError, Result};

/// blob 大小上限（10 MiB，与 sys.fetch `FETCH_MAX_BODY` 同口径）。
pub const BLOB_MAX_BYTES: usize = 10 * 1024 * 1024;

/// 传输分块：240 KiB 原始字节（= 3 × 81920，保证各块 base64 可直接拼接；
/// base64 后约 320 KiB，远低于 dm 单帧 1 MiB 上限）。
pub const BLOB_CHUNK_BYTES: usize = 240 * 1024;

/// 同一 hash 拉取请求的最小间隔（防丢帧重试由周期性调和驱动，节流防抖）。
pub const BLOB_REQ_THROTTLE_MS: i64 = 30_000;

/// 本体键。
pub fn blob_data_key(hash: &str) -> String {
    format!("blob:data:{hash}")
}

/// 接收中暂存键（base64 追加式拼接）。
pub fn blob_part_key(hash: &str) -> String {
    format!("blob:part:{hash}")
}

/// 拉取节流键（值 = 上次请求时间戳 ms）。
pub fn blob_req_key(hash: &str) -> String {
    format!("blob:req:{hash}")
}

/// want 标记键（lazy 拉取意图；调和时消费）。
pub fn blob_want_key(hash: &str) -> String {
    format!("blob:want:{hash}")
}

/// 无引用首见标记键（GC 宽限期起点；值 = 首见时间戳 ms）。
pub fn blob_unref_key(hash: &str) -> String {
    format!("blob:unref:{hash}")
}

/// GC 宽限期：无引用持续 7 天才回收（对齐"远端记录删除后由同持有成员
/// 重新拉取"的最坏时钟偏移与离线窗口；记录墓碑永存，重拉永远可恢复）。
pub const BLOB_GC_GRACE_MS: i64 = 7 * 24 * 3600 * 1000;

/// 无引用 blob 回收（宽限期两段式）：
/// - 引用面 = 全部 `pdoc:` + `ldoc:` 记录值的 `$blob` 递归提取；
/// - 本体在引用面内 → 清 unref 标记；
/// - 本体不在引用面且无 unref 标记 → 置标记（首见）；
/// - 无引用且标记龄期 ≥ [`BLOB_GC_GRACE_MS`] → 删除本体 + 标记；
/// - 引用面外的 `blob:part:`（中断的装配）与 `blob:want:` 同步清理；
/// - 墓碑记录（值为 null 的中间件写）不在引用面——删除传播后 blob 进入
///   宽限期，而非即刻回收（远端可能仍有引用、重拉可恢复）。
///
/// 返回回收的本体 hash 列表（日志/测试用）。
pub fn gc_blobs<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<Vec<String>> {
    // 1) 引用面
    let mut referenced = std::collections::BTreeSet::new();
    for prefix in ["pdoc:", "ldoc:"] {
        for (_key, raw) in storage.scan(&ScanOptions::prefix(prefix))? {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                for hash in blob_refs_in(&value) {
                    referenced.insert(hash);
                }
            }
        }
    }
    // 2) 本体键：按引用与否分流
    let mut collected = Vec::new();
    for (key, _v) in storage.scan(&ScanOptions::prefix("blob:data:"))? {
        let hash = key.trim_start_matches("blob:data:").to_string();
        if referenced.contains(&hash) {
            let _ = storage.delete(&blob_unref_key(&hash));
            continue;
        }
        let unref_key = blob_unref_key(&hash);
        match storage.get(&unref_key)? {
            None => {
                storage.put(&unref_key, &now_ms.to_string())?;
            }
            Some(raw) => {
                let since = raw.parse::<i64>().unwrap_or(now_ms);
                if now_ms - since >= BLOB_GC_GRACE_MS {
                    storage.delete(&key)?;
                    storage.delete(&unref_key)?;
                    collected.push(hash);
                }
            }
        }
    }
    // 3) 中断装配与孤儿 want：引用面外即清理（part 等不到下一块永无意义；
    // want 无引用来源说明记录已被删）
    for prefix in ["blob:part:", "blob:want:"] {
        let stale: Vec<String> = storage
            .scan(&ScanOptions::prefix(prefix))?
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| !referenced.contains(k.trim_start_matches(prefix)))
            .collect();
        for key in stale {
            storage.delete(&key)?;
        }
    }
    Ok(collected)
}

/// save_blob 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobInfo {
    /// 内容哈希（SHA-256 hex 小写）。
    pub hash: String,
    /// 原始字节数。
    pub size: u64,
}

/// 记录中的 blob 引用标记：`{"$blob": hash, ...}`。
pub const BLOB_REF_FIELD: &str = "$blob";

/// 保存 blob 本体（幂等：同内容同 hash，重写无害）。
pub fn save_blob<S: StorageBackend>(storage: &mut S, data: &[u8]) -> Result<BlobInfo> {
    if data.len() > BLOB_MAX_BYTES {
        return Err(PlugindataError::Blob(format!(
            "blob exceeds {} bytes",
            BLOB_MAX_BYTES
        )));
    }
    let hash = hex::encode(sha2::Sha256::digest(data));
    storage.put(&blob_data_key(&hash), &B64.encode(data))?;
    Ok(BlobInfo {
        hash,
        size: data.len() as u64,
    })
}

/// 读本体（命中 → base64；未命中 → None）。
pub fn read_blob<S: StorageBackend>(storage: &S, hash: &str) -> Result<Option<String>> {
    storage.get(&blob_data_key(hash)).map_err(Into::into)
}

/// 本体是否已在本地。
pub fn has_blob<S: StorageBackend>(storage: &S, hash: &str) -> bool {
    storage.get(&blob_data_key(hash)).ok().flatten().is_some()
}

/// 置 want 标记（lazy 拉取意图，调和时消费）。
pub fn mark_want<S: StorageBackend>(storage: &mut S, hash: &str) -> Result<()> {
    storage.put(&blob_want_key(hash), "1")?;
    Ok(())
}

/// 从记录值 JSON 中递归收集 blob 引用 hash（`{"$blob": "..."}` 对象）。
pub fn blob_refs_in(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_refs(value, &mut out);
    out
}

fn collect_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(hash) = map.get(BLOB_REF_FIELD).and_then(serde_json::Value::as_str)
                && !hash.is_empty()
            {
                out.push(hash.to_string());
            }
            for v in map.values() {
                collect_refs(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_refs(v, out);
            }
        }
        _ => {}
    }
}

/// 调和扫描：全部 `pdoc:` 记录值中引用而本地缺失的 blob hash（去重）。
/// 另并入 want 标记（lazy 拉取意图）。调用方按设备类决定是否扫描记录引用
/// （PC eager 扫、手机只取 want）。
pub fn missing_blobs<S: StorageBackend>(
    storage: &S,
    scan_records: bool,
) -> Result<Vec<String>> {
    let mut missing = std::collections::BTreeSet::new();
    if scan_records {
        for (_key, raw) in storage.scan(&ScanOptions::prefix("pdoc:"))? {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                for hash in blob_refs_in(&value) {
                    if !has_blob(storage, &hash) {
                        missing.insert(hash);
                    }
                }
            }
        }
    }
    for (key, _v) in storage.scan(&ScanOptions::prefix("blob:want:"))? {
        let hash = key.trim_start_matches("blob:want:");
        if !hash.is_empty() && !has_blob(storage, hash) {
            missing.insert(hash.to_string());
        }
    }
    Ok(missing.into_iter().collect())
}

/// 节流判定：距上次请求不足 [`BLOB_REQ_THROTTLE_MS`] → false（调用方应跳过）。
/// 通过则记录本次时间。
pub fn throttle_request<S: StorageBackend>(storage: &mut S, hash: &str, now_ms: i64) -> Result<bool> {
    if let Some(raw) = storage.get(&blob_req_key(hash))?
        && let Ok(last) = raw.parse::<i64>()
        && now_ms - last < BLOB_REQ_THROTTLE_MS
    {
        return Ok(false);
    }
    storage.put(&blob_req_key(hash), &now_ms.to_string())?;
    Ok(true)
}

/// 服务方：取 `[offset, offset+BLOB_CHUNK_BYTES)` 的 base64 块。
/// 返回 `(chunk_base64, total_bytes)`；hash 缺失或 offset 越界 → None。
pub fn serve_chunk<S: StorageBackend>(
    storage: &S,
    hash: &str,
    offset: usize,
) -> Result<Option<(String, u64)>> {
    let Some(b64) = storage.get(&blob_data_key(hash))? else {
        return Ok(None);
    };
    let data = B64
        .decode(&b64)
        .map_err(|e| PlugindataError::Blob(format!("stored blob base64 decode failed: {e}")))?;
    if offset > data.len() {
        return Ok(None);
    }
    let end = (offset + BLOB_CHUNK_BYTES).min(data.len());
    // 非尾块必然整块（3 的倍数），base64 可拼接；尾块任意长度
    Ok(Some((B64.encode(&data[offset..end]), data.len() as u64)))
}

/// 拉取方：合入一块。`offset` 必须等于当前已收字节数（顺序拼接）；
/// 收齐后校验 SHA-256，通过则提升为本体并清理暂存/want/节流键。
/// 返回 true = 已完成。
pub fn ingest_chunk<S: StorageBackend>(
    storage: &mut S,
    hash: &str,
    offset: usize,
    chunk_b64: &str,
    total_bytes: u64,
) -> Result<bool> {
    let chunk = B64
        .decode(chunk_b64)
        .map_err(|e| PlugindataError::Blob(format!("chunk base64 decode failed: {e}")))?;
    let part_key = blob_part_key(hash);
    // 已收 base64 字符数 → 原始字节数（暂存段皆整 4 字符、3 的倍数字节）
    let received_bytes = storage.get(&part_key)?.map(|s| s.len() / 4 * 3).unwrap_or(0);
    if offset != received_bytes {
        // 乱序/重复块：静默忽略，等待按序重发（节流驱动的下轮调和兜底）
        return Ok(false);
    }
    let mut joined = storage.get(&part_key)?.unwrap_or_default();
    joined.push_str(chunk_b64);
    if offset + chunk.len() < total_bytes as usize {
        storage.put(&part_key, &joined)?;
        return Ok(false);
    }
    // 收齐：尺寸与哈希校验后提升
    let data = B64
        .decode(&joined)
        .map_err(|e| PlugindataError::Blob(format!("assembled blob decode failed: {e}")))?;
    if data.len() as u64 != total_bytes {
        storage.delete(&part_key)?;
        return Err(PlugindataError::Blob(
            "blob size mismatch after assembly".to_string(),
        ));
    }
    if hex::encode(sha2::Sha256::digest(&data)) != hash {
        storage.delete(&part_key)?;
        return Err(PlugindataError::Blob(
            "blob hash mismatch after assembly".to_string(),
        ));
    }
    storage.put(&blob_data_key(hash), &joined)?;
    storage.delete(&part_key)?;
    let _ = storage.delete(&blob_want_key(hash));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn save_read_roundtrip_and_dedup() {
        let mut s = MemoryStorage::new();
        let data = b"hello blob".repeat(100);
        let a = save_blob(&mut s, &data).unwrap();
        let b = save_blob(&mut s, &data).unwrap();
        assert_eq!(a, b, "同内容同 hash");
        let b64 = read_blob(&s, &a.hash).unwrap().unwrap();
        assert_eq!(B64.decode(b64).unwrap(), data);
        assert!(has_blob(&s, &a.hash));
        assert!(read_blob(&s, &"0".repeat(64)).unwrap().is_none());
    }

    #[test]
    fn oversized_rejected() {
        let mut s = MemoryStorage::new();
        let big = vec![0u8; BLOB_MAX_BYTES + 1];
        assert!(save_blob(&mut s, &big).is_err());
    }

    #[test]
    fn refs_extraction_recursive() {
        let value = serde_json::json!({
            "title": "x",
            "file": { "$blob": "aaa", "name": "f.png", "size": 3, "mime": "image/png" },
            "items": [{ "$blob": "bbb" }, { "no": 1 }]
        });
        let refs = blob_refs_in(&value);
        assert_eq!(refs, vec!["aaa".to_string(), "bbb".to_string()]);
    }

    #[test]
    fn chunk_serve_and_ingest_multi_block() {
        let mut server = MemoryStorage::new();
        // 2.5 块的数据
        let data: Vec<u8> = (0..(BLOB_CHUNK_BYTES * 2 + 1234))
            .map(|i| (i % 251) as u8)
            .collect();
        let info = save_blob(&mut server, &data).unwrap();

        let mut client = MemoryStorage::new();
        let mut offset = 0usize;
        loop {
            let (chunk, total) = serve_chunk(&server, &info.hash, offset).unwrap().unwrap();
            assert_eq!(total, data.len() as u64);
            let done = ingest_chunk(&mut client, &info.hash, offset, &chunk, total).unwrap();
            offset += chunk.len() / 4 * 3;
            if done {
                break;
            }
        }
        let got = B64.decode(read_blob(&client, &info.hash).unwrap().unwrap()).unwrap();
        assert_eq!(got, data, "分块拼接后哈希校验通过且内容一致");
        // 乱序块被拒绝且不破坏暂存
        let mut c2 = MemoryStorage::new();
        let (chunk0, total) = serve_chunk(&server, &info.hash, 0).unwrap().unwrap();
        let (chunk1, _) = serve_chunk(&server, &info.hash, BLOB_CHUNK_BYTES).unwrap().unwrap();
        assert!(!ingest_chunk(&mut c2, &info.hash, BLOB_CHUNK_BYTES, &chunk1, total).unwrap());
        assert!(!ingest_chunk(&mut c2, &info.hash, 0, &chunk0, total).unwrap());
        // 哈希不匹配：收齐后校验失败并清理
        let mut c3 = MemoryStorage::new();
        assert!(
            ingest_chunk(&mut c3, &"f".repeat(64), 0, &B64.encode(b"tampered"), 8).is_err(),
            "哈希校验失败"
        );
        assert!(c3.get(&blob_part_key(&"f".repeat(64))).unwrap().is_none());
    }

    #[test]
    fn missing_blobs_scan_and_want() {
        let mut s = MemoryStorage::new();
        let present = save_blob(&mut s, b"here").unwrap();
        s.put(
            "pdoc:ai-chat:c@v1:c1",
            &serde_json::json!({ "f": { "$blob": present.hash } }).to_string(),
        )
        .unwrap();
        s.put(
            "pdoc:ai-chat:c@v1:c2",
            &serde_json::json!({ "f": { "$blob": "abc123" } }).to_string(),
        )
        .unwrap();
        // 非 pdoc 键不参与扫描
        s.put("msg:item:x", &serde_json::json!({ "$blob": "zzz" }).to_string()).unwrap();
        let missing = missing_blobs(&s, true).unwrap();
        assert_eq!(missing, vec!["abc123".to_string()], "已持有的不算缺失");
        // 不扫记录时为空；want 标记并入
        assert!(missing_blobs(&s, false).unwrap().is_empty());
        mark_want(&mut s, "lazy-one").unwrap();
        let missing2 = missing_blobs(&s, false).unwrap();
        assert_eq!(missing2, vec!["lazy-one".to_string()]);
    }

    #[test]
    fn throttle_request_window() {
        let mut s = MemoryStorage::new();
        assert!(throttle_request(&mut s, "h", 1000).unwrap(), "首次放行");
        assert!(!throttle_request(&mut s, "h", 1000 + BLOB_REQ_THROTTLE_MS - 1).unwrap());
        assert!(throttle_request(&mut s, "h", 1000 + BLOB_REQ_THROTTLE_MS).unwrap());
    }

    #[test]
    fn gc_grace_period_two_phase() {
        let mut s = MemoryStorage::new();
        // 两个本体：a 被 pdoc 记录引用，b 无引用
        let a = save_blob(&mut s, b"referenced").unwrap();
        let b = save_blob(&mut s, b"orphan").unwrap();
        s.put(
            "pdoc:ai-chat:c@v1:c1",
            &serde_json::json!({ "f": { "$blob": a.hash } }).to_string(),
        )
        .unwrap();
        // ldoc（local 集合）引用同样计入引用面
        let c = save_blob(&mut s, b"local-ref").unwrap();
        s.put(
            "ldoc:ai-chat:d@v1:d1",
            &serde_json::json!({ "f": { "$blob": c.hash } }).to_string(),
        )
        .unwrap();

        // 第一轮 GC：b 置 unref 标记，不回收
        let collected = gc_blobs(&mut s, 1_000_000).unwrap();
        assert!(collected.is_empty());
        assert!(has_blob(&s, &b.hash), "宽限期内保留");
        assert!(s.get(&blob_unref_key(&b.hash)).unwrap().is_some());

        // 宽限期未满：仍不回收
        let collected = gc_blobs(&mut s, 1_000_000 + BLOB_GC_GRACE_MS - 1).unwrap();
        assert!(collected.is_empty());

        // 宽限期满：回收 b；a/c 引用在册不回收
        let collected = gc_blobs(&mut s, 1_000_000 + BLOB_GC_GRACE_MS).unwrap();
        assert_eq!(collected, vec![b.hash.clone()]);
        assert!(!has_blob(&s, &b.hash));
        assert!(has_blob(&s, &a.hash));
        assert!(has_blob(&s, &c.hash));

        // 重新被引用后 unref 标记清除（拉回来的本体不再被误回收）
        let d = save_blob(&mut s, b"back").unwrap();
        gc_blobs(&mut s, 2_000_000).unwrap(); // 置标记
        s.put(
            "pdoc:ai-chat:c@v1:c2",
            &serde_json::json!({ "f": { "$blob": d.hash } }).to_string(),
        )
        .unwrap();
        gc_blobs(&mut s, 2_000_000 + BLOB_GC_GRACE_MS).unwrap();
        assert!(has_blob(&s, &d.hash), "重新引用后清除标记");
        assert!(s.get(&blob_unref_key(&d.hash)).unwrap().is_none());
    }

    #[test]
    fn gc_cleans_stale_parts_and_wants() {
        let mut s = MemoryStorage::new();
        s.put(&blob_part_key("interrupted"), "QUJD").unwrap();
        mark_want(&mut s, "orphan-want").unwrap();
        gc_blobs(&mut s, 1000).unwrap();
        assert!(s.get(&blob_part_key("interrupted")).unwrap().is_none());
        assert!(s.get(&blob_want_key("orphan-want")).unwrap().is_none());
        // 有引用的 want 保留（正在等拉取）
        let info = save_blob(&mut s, b"x").unwrap();
        s.put(
            "pdoc:ai-chat:c@v1:c1",
            &serde_json::json!({ "f": { "$blob": info.hash } }).to_string(),
        )
        .unwrap();
        mark_want(&mut s, &info.hash).unwrap();
        gc_blobs(&mut s, 1000).unwrap();
        assert!(s.get(&blob_want_key(&info.hash)).unwrap().is_some());
    }
}
