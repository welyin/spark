//! 按需回补：`blob-fetch` / `blob-chunk` dm 信封（§14.5）+ 拉取规划与
//! 诚实降级（§14.6）。
//!
//! 复用 dm 直连通道（自设备间，验签同 pdsync）；持有即做种——任何持有
//! `blob:chunk:{chunkCid}` / `blob:meta:{cid}` 的设备均可应答。拉取是幂等
//! 可重入的：分片续拉以 offset 对齐，乱序/重复块回 `NeedMore{当前进度}`
//! 让请求方按正确 offset 重拉（断点续传）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use super::presence::{
    PresenceRecord, holders_of_chunk, list_presence, manifest_holders, refresh_presence,
};
use super::{
    BlobError, BlobManifest, CHUNK_SIZE_BYTES, CHUNK_THRESHOLD_BYTES, SyncResult,
    TRANSFER_SLICE_BYTES, blob_asm_key, blob_chunk_key, blob_freq_key, get_manifest,
    held_chunk_indices, is_hex64, read_blob, sha256_hex,
};
use crate::storage::StorageBackend;

// ── blob-fetch 请求 ────────────────────────────────────────────────

/// 拉取目标：单个块（可带 offset 续拉）或 manifest。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchTarget {
    /// `{"cid": "..."}` —— 拉取 manifest。
    Manifest {
        /// blob cid。
        cid: String,
    },
    /// `{"chunkCid": "...", "offset": N}` —— 拉取块内容（offset 缺省 0）。
    Chunk {
        /// 块 cid。
        chunk_cid: String,
        /// 已收字节数（分片续拉偏移）。
        offset: u64,
    },
}

/// 构造 `blob-fetch` body（offset == 0 时省略，即设计钉定的最简形态）。
pub fn build_fetch_body(target: &FetchTarget) -> Value {
    match target {
        FetchTarget::Manifest { cid } => json!({ "cid": cid }),
        FetchTarget::Chunk { chunk_cid, offset } => {
            if *offset == 0 {
                json!({ "chunkCid": chunk_cid })
            } else {
                json!({ "chunkCid": chunk_cid, "offset": offset })
            }
        }
    }
}

/// 解析 `blob-fetch` body；线形非法 → None。
pub fn parse_fetch_body(body: &Value) -> Option<FetchTarget> {
    if let Some(chunk_cid) = body.get("chunkCid").and_then(Value::as_str) {
        if !is_hex64(chunk_cid) {
            return None;
        }
        let offset = body.get("offset").and_then(Value::as_u64).unwrap_or(0);
        return Some(FetchTarget::Chunk {
            chunk_cid: chunk_cid.to_string(),
            offset,
        });
    }
    let cid = body.get("cid").and_then(Value::as_str)?;
    if !is_hex64(cid) {
        return None;
    }
    Some(FetchTarget::Manifest {
        cid: cid.to_string(),
    })
}

// ── blob-chunk 响应 ────────────────────────────────────────────────

/// 解析后的 `blob-chunk` 响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchResponse {
    /// 块内容（整块或一个分片；data 已解码）。
    Chunk {
        /// 块 cid。
        chunk_cid: String,
        /// 本分片在块内的偏移。
        offset: u64,
        /// 分片内容（已解码字节）。
        data: Vec<u8>,
        /// 块总长（整块应答时 = data.len()）。
        total_bytes: u64,
    },
    /// manifest 应答（结构校验已通过）。
    Manifest {
        /// blob cid。
        cid: String,
        /// manifest。
        manifest: BlobManifest,
    },
    /// 诚实否定（对端无此块/manifest）。
    Missing,
}

/// 服务方：按目标产出 `blob-chunk` body（持有即做种；本地无 → missing:true）。
pub fn serve_fetch<S: StorageBackend>(storage: &S, target: &FetchTarget) -> SyncResult<Value> {
    match target {
        FetchTarget::Manifest { cid } => {
            let body = match get_manifest(storage, cid)? {
                Some(manifest) => json!({ "cid": cid, "manifest": serde_json::to_value(&manifest)? }),
                None => json!({ "cid": cid, "missing": true }),
            };
            Ok(body)
        }
        FetchTarget::Chunk { chunk_cid, offset } => {
            let Some(b64) = storage.get(&blob_chunk_key(chunk_cid))? else {
                return Ok(json!({ "chunkCid": chunk_cid, "missing": true }));
            };
            // 本地存盘损坏：诚实回 missing（绝不发出与 chunkCid 不符的字节，
            // 接收侧哈希校验是第二道防线）
            let Ok(data) = B64.decode(&b64) else {
                return Ok(json!({ "chunkCid": chunk_cid, "missing": true }));
            };
            let offset = *offset as usize;
            if offset > data.len() {
                return Ok(json!({ "chunkCid": chunk_cid, "missing": true }));
            }
            // 传输单元（§14.5）：余量 ≤256 KiB 单信封装完；更大（仅单块
            // blob 256 KiB < size ≤ 1 MiB）按 240 KiB 切片（3 的倍数，
            // 接收侧 base64 直接追加）
            let remaining = data.len() - offset;
            let take = if remaining > CHUNK_SIZE_BYTES {
                TRANSFER_SLICE_BYTES
            } else {
                remaining
            };
            let end = offset + take;
            let slice = B64.encode(&data[offset..end]);
            // 整块一个信封装完（offset==0 且到尾）→ 最简形态省略 offset/totalBytes
            if offset == 0 && end == data.len() {
                Ok(json!({ "chunkCid": chunk_cid, "data": slice }))
            } else {
                Ok(json!({
                    "chunkCid": chunk_cid,
                    "offset": offset,
                    "data": slice,
                    "totalBytes": data.len(),
                }))
            }
        }
    }
}

/// 解析 `blob-chunk` body；线形非法（缺字段/manifest 校验失败/base64 非法）
/// → None。
pub fn parse_chunk_body(body: &Value) -> Option<FetchResponse> {
    if body.get("missing").and_then(Value::as_bool) == Some(true) {
        return Some(FetchResponse::Missing);
    }
    if let Some(manifest_val) = body.get("manifest") {
        let cid = body.get("cid").and_then(Value::as_str)?;
        if !is_hex64(cid) {
            return None;
        }
        let manifest: BlobManifest = serde_json::from_value(manifest_val.clone()).ok()?;
        if super::validate_manifest(&manifest).is_err() || manifest.cid != cid {
            return None;
        }
        return Some(FetchResponse::Manifest {
            cid: cid.to_string(),
            manifest,
        });
    }
    let chunk_cid = body.get("chunkCid").and_then(Value::as_str)?;
    let data_b64 = body.get("data").and_then(Value::as_str)?;
    if !is_hex64(chunk_cid) {
        return None;
    }
    let data = B64.decode(data_b64).ok()?;
    let offset = body.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let total_bytes = body
        .get("totalBytes")
        .and_then(Value::as_u64)
        .unwrap_or(data.len() as u64);
    Some(FetchResponse::Chunk {
        chunk_cid: chunk_cid.to_string(),
        offset,
        data,
        total_bytes,
    })
}

// ── 拉取方合入 ─────────────────────────────────────────────────────

/// 合入结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    /// 未收齐（或乱序纠偏）：按 `next_offset` 续拉。
    NeedMore {
        /// 下一分片偏移（乱序时为当前已收进度，纠偏重拉）。
        next_offset: u64,
    },
    /// 块已收齐并通过 sha256 == chunkCid 校验落库。
    ChunkDone {
        /// 块 cid。
        chunk_cid: String,
    },
    /// manifest 已校验落库。
    ManifestDone {
        /// blob cid。
        cid: String,
    },
    /// 校验失败（哈希/尺寸/线形不符）——不落库、不计 presence。
    Rejected,
}

/// 合入一个块分片：offset 必须等于当前已收进度（顺序拼接）；收齐做尺寸 +
/// sha256 校验，通过落 `blob:chunk:` 并刷新 `cid`（所属 blob）的 presence
/// （manifest 未到时 presence 由 [`ingest_manifest`] 兜底刷新）。
///
/// `cid` 是请求方据以规划拉取的 manifest cid（chunkCid 只能来自 manifest，
/// 调用方必然已知）。
pub fn ingest_chunk<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    cid: &str,
    chunk_cid: &str,
    offset: u64,
    data: &[u8],
    total_bytes: u64,
    now_ms: i64,
) -> SyncResult<IngestOutcome> {
    // 块不可能超过单块阈值（1 MiB）；0 长块不存在（空 blob 无块）
    if total_bytes == 0 || total_bytes > CHUNK_THRESHOLD_BYTES as u64 {
        return Ok(IngestOutcome::Rejected);
    }
    let asm_key = blob_asm_key(chunk_cid);
    let received = storage
        .get(&asm_key)?
        .map(|s| (s.len() / 4 * 3) as u64)
        .unwrap_or(0);
    if offset != received {
        // 乱序/重复分片：不追加，让请求方按当前进度纠偏重拉
        return Ok(IngestOutcome::NeedMore {
            next_offset: received,
        });
    }
    if offset + data.len() as u64 > total_bytes {
        storage.delete(&asm_key)?;
        return Ok(IngestOutcome::Rejected);
    }
    if offset + (data.len() as u64) < total_bytes {
        let mut joined = storage.get(&asm_key)?.unwrap_or_default();
        joined.push_str(&B64.encode(data));
        storage.put(&asm_key, &joined)?;
        return Ok(IngestOutcome::NeedMore {
            next_offset: offset + data.len() as u64,
        });
    }
    // 收齐：尺寸 + 哈希校验后提升
    let mut assembled = storage.get(&asm_key)?.unwrap_or_default();
    assembled.push_str(&B64.encode(data));
    let bytes = B64
        .decode(&assembled)
        .map_err(|e| BlobError::Integrity(format!("assembled chunk base64: {e}")))?;
    if bytes.len() as u64 != total_bytes || sha256_hex(&bytes) != chunk_cid {
        storage.delete(&asm_key)?;
        return Ok(IngestOutcome::Rejected);
    }
    storage.put(&blob_chunk_key(chunk_cid), &assembled)?;
    storage.delete(&asm_key)?;
    refresh_presence(storage, node_id, device_uid, cid, now_ms)?;
    super::quota::touch_access(storage, cid, now_ms)?;
    // 回补落块 = 显式拉取完成，解除驱逐标记（§16.4）
    super::evict::clear_evicted(storage, cid)?;
    Ok(IngestOutcome::ChunkDone {
        chunk_cid: chunk_cid.to_string(),
    })
}

/// 合入 manifest：结构校验 + cid 匹配后落 `blob:meta:`（规范 JSON），并按
/// 本机已持块刷新 presence（块可能先于 manifest 到达）。校验失败 → false。
pub fn ingest_manifest<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    cid: &str,
    manifest: &BlobManifest,
    now_ms: i64,
) -> SyncResult<bool> {
    if manifest.cid != cid || super::validate_manifest(manifest).is_err() {
        return Ok(false);
    }
    storage.put(&super::blob_meta_key(cid), &manifest.to_json())?;
    refresh_presence(storage, node_id, device_uid, cid, now_ms)?;
    super::quota::touch_access(storage, cid, now_ms)?;
    Ok(true)
}

// ── 拉取规划与诚实降级 ─────────────────────────────────────────────

/// 单个缺失块的拉取计划（持有者按 deviceUid 字典序最小选定——任何设备
/// 凭相同账本算出同一计划，确定性）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkFetch {
    /// 块在 manifest 中的位次。
    pub index: usize,
    /// 块 cid。
    pub chunk_cid: String,
    /// 选定的持有者 deviceUid。
    pub holder: String,
}

/// 拉取计划：manifest 缺失时只有 `manifest_from`；否则逐缺失块一条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchPlan {
    /// manifest 的选定持有者（manifest 已在本地时为 None）。
    pub manifest_from: Option<String>,
    /// 缺失块的拉取列表（manifest 顺序）。
    pub chunks: Vec<ChunkFetch>,
}

/// 读取结果：齐块装配（终验通过）/ 可规划回补 / 诚实降级「暂不可用」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// 本地齐块且 sha256(拼接) == cid 终验通过。
    Data(Vec<u8>),
    /// 缺 manifest/块，但（在线）持有者可补齐。
    NeedFetch(FetchPlan),
    /// 无（在线）持有者——暂不可用（不落假数据；持有者上线后重规划）。
    Unavailable,
}

/// 拉取规划：`online` 为在线 deviceUid 列表（None = 不过滤，离线规划/
/// 测试用）。所有缺失块都有（在线）持有者才返回计划，否则 None。
pub fn plan_fetch<S: StorageBackend>(
    storage: &S,
    cid: &str,
    online: Option<&[String]>,
) -> SyncResult<Option<FetchPlan>> {
    let allowed = |uid: &str| online.is_none_or(|list| list.iter().any(|u| u == uid));
    let records = list_presence(storage, cid)?;
    let Some(manifest) = get_manifest(storage, cid)? else {
        // manifest 缺失：凡有 presence 记录即视为 manifest 持有者
        // （presence 只在 manifest 在握时写入）
        let holder = manifest_holders(&records).into_iter().find(|u| allowed(u));
        return Ok(holder.map(|h| FetchPlan {
            manifest_from: Some(h),
            chunks: Vec::new(),
        }));
    };
    let held = held_chunk_indices(storage, &manifest)?;
    let mut chunks = Vec::new();
    for (index, h) in held.iter().enumerate() {
        if *h {
            continue;
        }
        let Some(holder) = holders_of_chunk(&records, index)
            .into_iter()
            .find(|u| allowed(u))
        else {
            return Ok(None); // 任一块无（在线）持有者 → 整体暂不可用
        };
        chunks.push(ChunkFetch {
            index,
            chunk_cid: manifest.chunk_cids[index].clone(),
            holder,
        });
    }
    Ok(Some(FetchPlan {
        manifest_from: None,
        chunks,
    }))
}

/// 读 blob 或给出回补计划/降级结论（读路径唯一入口）。
/// 读出成功会刷新 `blob:access:`（经 [`read_blob`]）。
pub fn read_or_plan<S: StorageBackend>(
    storage: &mut S,
    cid: &str,
    online: Option<&[String]>,
    now_ms: i64,
) -> SyncResult<ReadOutcome> {
    if let Some(data) = read_blob(storage, cid, now_ms)? {
        return Ok(ReadOutcome::Data(data));
    }
    match plan_fetch(storage, cid, online)? {
        Some(plan) => Ok(ReadOutcome::NeedFetch(plan)),
        None => Ok(ReadOutcome::Unavailable),
    }
}

/// 节流判定：距上次请求不足 [`super::FETCH_THROTTLE_MS`] → false（调用方
/// 应跳过）。通过则记录本次时间。
pub fn throttle_fetch<S: StorageBackend>(
    storage: &mut S,
    chunk_cid: &str,
    now_ms: i64,
) -> SyncResult<bool> {
    if let Some(raw) = storage.get(&blob_freq_key(chunk_cid))?
        && let Ok(last) = raw.parse::<i64>()
        && now_ms - last < super::FETCH_THROTTLE_MS
    {
        return Ok(false);
    }
    storage.put(&blob_freq_key(chunk_cid), &now_ms.to_string())?;
    Ok(true)
}

/// 便于入站编排的持有者判定：`records` 中 `holder` 是否持有第 `index` 块。
pub fn record_holds_chunk(record: &PresenceRecord, index: usize) -> bool {
    super::presence::bitmap_decode(&record.chunks)
        .is_some_and(|d| super::presence::bitmap_holds(&d, index))
}

#[cfg(test)]
mod tests {
    use super::super::has_chunk;
    use super::*;
    use crate::storage::MemoryStorage;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    const NODE: &str = "node-a";
    const UID: &str = "uid-a";

    #[test]
    fn fetch_body_shapes_byte_exact() {
        assert_eq!(
            build_fetch_body(&FetchTarget::Chunk {
                chunk_cid: "ab".to_string(),
                offset: 0
            }),
            json!({ "chunkCid": "ab" })
        );
        assert_eq!(
            build_fetch_body(&FetchTarget::Chunk {
                chunk_cid: "ab".to_string(),
                offset: 245760
            }),
            json!({ "chunkCid": "ab", "offset": 245760 })
        );
        assert_eq!(
            build_fetch_body(&FetchTarget::Manifest {
                cid: "cd".to_string()
            }),
            json!({ "cid": "cd" })
        );
        // 解析往返 + 非法线形
        let t = parse_fetch_body(&json!({ "chunkCid": "a".repeat(64) })).unwrap();
        assert_eq!(
            t,
            FetchTarget::Chunk {
                chunk_cid: "a".repeat(64),
                offset: 0
            }
        );
        assert!(parse_fetch_body(&json!({ "chunkCid": "short" })).is_none());
        assert!(parse_fetch_body(&json!({})).is_none());
    }

    #[test]
    fn serve_whole_slice_and_missing() {
        let mut s = MemoryStorage::new();
        // 多分块 blob：每块 256KiB，单信封整块应答
        let big = pattern(CHUNK_THRESHOLD_BYTES + 10);
        let m = super::super::save_blob(&mut s, NODE, UID, &big, 1000).unwrap();
        let c0 = &m.chunk_cids[0];
        let body = serve_fetch(
            &s,
            &FetchTarget::Chunk {
                chunk_cid: c0.clone(),
                offset: 0,
            },
        )
        .unwrap();
        assert!(body.get("offset").is_none(), "整块省略 offset");
        let resp = parse_chunk_body(&body).unwrap();
        let FetchResponse::Chunk {
            data, total_bytes, ..
        } = resp
        else {
            panic!("应为 Chunk");
        };
        assert_eq!(data.len(), 256 * 1024);
        assert_eq!(total_bytes, 256 * 1024);
        // offset 越界与不存在 → missing
        for target in [
            FetchTarget::Chunk {
                chunk_cid: c0.clone(),
                offset: u64::MAX,
            },
            FetchTarget::Chunk {
                chunk_cid: "f".repeat(64),
                offset: 0,
            },
        ] {
            let body = serve_fetch(&s, &target).unwrap();
            assert_eq!(parse_chunk_body(&body), Some(FetchResponse::Missing));
        }
        // manifest 应答与 missing
        let body = serve_fetch(&s, &FetchTarget::Manifest { cid: m.cid.clone() }).unwrap();
        let Some(FetchResponse::Manifest { manifest, .. }) = parse_chunk_body(&body) else {
            panic!("应为 Manifest");
        };
        assert_eq!(manifest, m);
        let body = serve_fetch(
            &s,
            &FetchTarget::Manifest {
                cid: "e".repeat(64),
            },
        )
        .unwrap();
        assert_eq!(parse_chunk_body(&body), Some(FetchResponse::Missing));
    }

    #[test]
    fn sliced_single_chunk_roundtrip_and_tamper_rejected() {
        let mut server = MemoryStorage::new();
        // 600KB 单块 blob（>240KiB 传输切片 → offset 续拉）
        let data = pattern(600 * 1024);
        let m = super::super::save_blob(&mut server, NODE, UID, &data, 1000).unwrap();
        assert_eq!(m.chunk_cids.len(), 1);
        let chunk_cid = m.chunk_cids[0].clone();

        let mut client = MemoryStorage::new();
        // manifest 先行（presence 刷新依赖）
        let mbody = serve_fetch(&server, &FetchTarget::Manifest { cid: m.cid.clone() }).unwrap();
        let Some(FetchResponse::Manifest { manifest, .. }) = parse_chunk_body(&mbody) else {
            panic!()
        };
        assert!(ingest_manifest(&mut client, "node-b", "uid-b", &m.cid, &manifest, 1000).unwrap());

        let mut offset = 0u64;
        loop {
            let body = serve_fetch(
                &server,
                &FetchTarget::Chunk {
                    chunk_cid: chunk_cid.clone(),
                    offset,
                },
            )
            .unwrap();
            let Some(FetchResponse::Chunk {
                offset: o,
                data: slice,
                total_bytes,
                ..
            }) = parse_chunk_body(&body)
            else {
                panic!()
            };
            assert_eq!(o, offset);
            match ingest_chunk(
                &mut client, "node-b", "uid-b", &m.cid, &chunk_cid, o, &slice, total_bytes,
                1000,
            )
            .unwrap()
            {
                IngestOutcome::NeedMore { next_offset } => offset = next_offset,
                IngestOutcome::ChunkDone { .. } => break,
                other => panic!("意外结果 {other:?}"),
            }
        }
        assert_eq!(
            super::read_blob(&mut client, &m.cid, 2000).unwrap().as_deref(),
            Some(data.as_slice()),
            "分片装配后与源一致"
        );
        // 回补成功 → presence 已写（持有即做种）
        let records = list_presence(&client, &m.cid).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].device_uid, "uid-b");

        // 篡改分片：收齐后哈希不符 → Rejected 且不落库
        let mut c2 = MemoryStorage::new();
        let fake_cid = "f".repeat(64);
        let outcome = ingest_chunk(
            &mut c2,
            "node-b",
            "uid-b",
            &fake_cid,
            &fake_cid,
            0,
            b"tampered",
            8,
            1000,
        )
        .unwrap();
        assert_eq!(outcome, IngestOutcome::Rejected);
        assert!(!has_chunk(&c2, &fake_cid));
        // 乱序分片：纠偏回当前进度
        let outcome = ingest_chunk(
            &mut c2, "node-b", "uid-b", &fake_cid, &fake_cid, 100, b"x", 200, 1000,
        )
        .unwrap();
        assert_eq!(outcome, IngestOutcome::NeedMore { next_offset: 0 });
    }

    #[test]
    fn plan_deterministic_and_online_filtered() {
        let mut s = MemoryStorage::new();
        // manifest 在本地、块全缺；A/B 完整持有、C 部分持有
        let big = pattern(CHUNK_THRESHOLD_BYTES + 1);
        let m = super::super::build_manifest(&big);
        s.put(&super::super::blob_meta_key(&m.cid), &m.to_json()).unwrap();
        let full = super::super::presence::bitmap_encode(&[true; 5]);
        for uid in ["uid-b", "uid-a"] {
            s.put(
                &super::super::presence::presence_key(&m.cid, uid),
                &PresenceRecord {
                    v: 1,
                    cid: m.cid.clone(),
                    device_uid: uid.to_string(),
                    chunks: full.clone(),
                }
                .to_json(),
            )
            .unwrap();
        }
        // 确定性：字典序最小持有者 uid-a（与记录写入顺序无关）
        let plan = plan_fetch(&s, &m.cid, None).unwrap().unwrap();
        assert_eq!(plan.chunks.len(), 5);
        assert!(plan.chunks.iter().all(|c| c.holder == "uid-a"));
        // 在线过滤：A 离线 → 选 uid-b；都离线 → None（暂不可用）
        let online = vec!["uid-b".to_string()];
        let plan = plan_fetch(&s, &m.cid, Some(&online)).unwrap().unwrap();
        assert!(plan.chunks.iter().all(|c| c.holder == "uid-b"));
        assert!(plan_fetch(&s, &m.cid, Some(&[])).unwrap().is_none());
        // manifest 缺失时的持有者规划
        let cid2 = "c".repeat(64);
        assert!(plan_fetch(&s, &cid2, None).unwrap().is_none());
        s.put(
            &super::super::presence::presence_key(&cid2, "uid-z"),
            &PresenceRecord {
                v: 1,
                cid: cid2.clone(),
                device_uid: "uid-z".to_string(),
                chunks: "AQ==".to_string(),
            }
            .to_json(),
        )
        .unwrap();
        let plan = plan_fetch(&s, &cid2, None).unwrap().unwrap();
        assert_eq!(plan.manifest_from.as_deref(), Some("uid-z"));
        assert!(plan.chunks.is_empty());
    }

    #[test]
    fn read_or_plan_three_outcomes() {
        let mut s = MemoryStorage::new();
        let data = pattern(500);
        let m = super::super::save_blob(&mut s, NODE, UID, &data, 1000).unwrap();
        assert!(matches!(
            read_or_plan(&mut s, &m.cid, None, 1500).unwrap(),
            ReadOutcome::Data(_)
        ));
        // 弃块 + 无其他持有者 → Unavailable（自己的 presence 已墓碑）
        super::super::drop_local_chunks(&mut s, NODE, UID, &m.cid, 2000).unwrap();
        assert_eq!(
            read_or_plan(&mut s, &m.cid, None, 2500).unwrap(),
            ReadOutcome::Unavailable
        );
        // 其他持有者出现 → NeedFetch
        s.put(
            &super::super::presence::presence_key(&m.cid, "uid-c"),
            &PresenceRecord {
                v: 1,
                cid: m.cid.clone(),
                device_uid: "uid-c".to_string(),
                chunks: "AQ==".to_string(),
            }
            .to_json(),
        )
        .unwrap();
        assert!(matches!(
            read_or_plan(&mut s, &m.cid, None, 3000).unwrap(),
            ReadOutcome::NeedFetch(_)
        ));
        // 无 manifest 无持有者 → Unavailable
        assert_eq!(
            read_or_plan(&mut s, &"d".repeat(64), None, 3500).unwrap(),
            ReadOutcome::Unavailable
        );
    }

    #[test]
    fn throttle_window() {
        let mut s = MemoryStorage::new();
        assert!(throttle_fetch(&mut s, "h", 1000).unwrap());
        assert!(!throttle_fetch(&mut s, "h", 1000 + super::super::FETCH_THROTTLE_MS - 1).unwrap());
        assert!(throttle_fetch(&mut s, "h", 1000 + super::super::FETCH_THROTTLE_MS).unwrap());
    }
}
