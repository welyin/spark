//! `blob-fetch` / `blob-chunk` 入站处理：A1 blob 层按需回补（自设备间，
//! 验签与 `from == 自己 rootId` 口径同 pdsync；线形 personal-data-sync §14.5）。
//!
//! - 请求方：`{chunkCid, offset?}` 拉块 / `{cid}` 拉 manifest；
//! - 服务方：持有即做种——本地有 `blob:chunk:`/`blob:meta:` 即应答，
//!   无则回 `missing: true`（诚实否定，请求方下轮重规划）；
//! - 接收方：分片按序拼接，收齐 SHA-256 校验落库 + 刷新 presence；
//!   manifest 校验落库后按已持块刷新 presence；
//! - 热切续拉（对齐 P6 热切先例，仅 PC eager）：落库后本机仍缺块且
//!   **本帧发送方**持有下一块 → 立即向其续拉（pdsync_out 只回投连接层
//!   对端，目标只能是发送方）；手机 lazy 不主动持 blob（设计 §4.2）。

use serde_json::Value;

use super::{InboundContext, InboundDmResult, PdsyncOut, Result, done, fail_response, ok_response};
use crate::storage::StorageBackend;
use crate::sync::blob::{self, FetchResponse, IngestOutcome};

/// 结果模板：无事件、无自记录动作，仅 pdsync_out。
fn with_out(out: Vec<PdsyncOut>) -> Result<InboundDmResult> {
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: out,
        orgsync_out: Vec::new(),
        affairsync_out: Vec::new(),
        profile_applied: false,
        feed_blob_out: None,
    })
}

/// `blob-fetch`：服务本地持有的块/manifest。
pub(super) fn handle_blob_fetch<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some(target) = blob::parse_fetch_body(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let resp_body = blob::serve_fetch(storage, &target)?;
    with_out(vec![PdsyncOut::BlobChunk { body: resp_body }])
}

/// `blob-chunk`：合入 manifest/块分片，必要时续拉。
pub(super) fn handle_blob_chunk<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some(resp) = blob::parse_chunk_body(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // 本机 deviceUid（presence 记账主体；种子缺失时按 device 模块惯例创建）
    let device_uid = crate::device::get_or_create_device_uid(storage)?;
    match resp {
        // 诚实否定：不落任何状态，下轮由读路径/调和重规划（可能彼时对端
        // 已就绪或换其他持有设备）
        FetchResponse::Missing => with_out(Vec::new()),
        FetchResponse::Manifest { cid, manifest } => {
            if !blob::ingest_manifest(storage, ctx.node_id, &device_uid, &cid, &manifest, ctx.now_ms)?
            {
                return done(fail_response("invalid-manifest"), Vec::new());
            }
            let next = next_fetch_from_sender(storage, ctx, &cid)?;
            with_out(next.into_iter().collect())
        }
        FetchResponse::Chunk {
            chunk_cid,
            offset,
            data,
            total_bytes,
        } => {
            // 所属 blob 的 cid： chunkCid 只能来自 manifest，本机必已持有
            // （manifest 未到不可能发出该块的拉取）；经 manifest 反查
            let Some(cid) = find_cid_for_chunk(storage, &chunk_cid)? else {
                return done(fail_response("unknown-chunk"), Vec::new());
            };
            match blob::ingest_chunk(
                storage,
                ctx.node_id,
                &device_uid,
                &cid,
                &chunk_cid,
                offset,
                &data,
                total_bytes,
                ctx.now_ms,
            )? {
                IngestOutcome::NeedMore { next_offset } => with_out(vec![PdsyncOut::BlobFetch {
                    body: serde_json::json!({ "chunkCid": chunk_cid, "offset": next_offset }),
                }]),
                IngestOutcome::ChunkDone { .. } => {
                    let next = next_fetch_from_sender(storage, ctx, &cid)?;
                    with_out(next.into_iter().collect())
                }
                // 校验失败：不落库不计 presence，等下轮重规划
                IngestOutcome::Rejected | IngestOutcome::ManifestDone { .. } => {
                    with_out(Vec::new())
                }
            }
        }
    }
}

/// 由 chunkCid 反查所属 blob 的 cid（扫本机 manifest；拉取方必已持有
/// manifest——chunkCid 只能从中读出，扫不到即线形异常）。
fn find_cid_for_chunk<S: StorageBackend>(
    storage: &S,
    chunk_cid: &str,
) -> Result<Option<String>> {
    for (key, raw) in storage.scan(&crate::storage::ScanOptions::prefix("blob:meta:"))? {
        let Some(manifest) = blob::BlobManifest::from_json(&raw) else {
            continue;
        };
        if manifest.chunk_cids.iter().any(|c| c == chunk_cid) {
            return Ok(Some(key.trim_start_matches("blob:meta:").to_string()));
        }
    }
    Ok(None)
}

/// 热切续拉（仅 PC eager）：本机仍缺块且本帧发送方持有下一块 → 续拉指令。
///
/// 发送方持有判定：设备清单反查其 deviceUid → 读本机 presence 账本中该
/// 设备的位图。账本未覆盖（记录未到/位图不含）则不续拉——下轮反熵刷新
/// 账本后由读路径/调和重规划兜底。
fn next_fetch_from_sender<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    cid: &str,
) -> Result<Option<PdsyncOut>> {
    if crate::sync::pdsync::local_device_class() != "pc" {
        return Ok(None);
    }
    let Some(manifest) = blob::get_manifest(storage, cid)? else {
        return Ok(None);
    };
    let held = blob::held_chunk_indices(storage, &manifest)?;
    let Some(index) = held.iter().position(|h| !h) else {
        return Ok(None); // 已齐块
    };
    let sender_uid = crate::device::DeviceService::get(storage, ctx.remote_peer_id)
        .ok()
        .flatten()
        .and_then(|d| d.device_uid);
    let Some(sender_uid) = sender_uid else {
        return Ok(None);
    };
    let Ok(Some(raw)) = storage.get(&blob::presence_key(cid, &sender_uid)) else {
        return Ok(None);
    };
    let Some(record) = blob::PresenceRecord::from_json(&raw) else {
        return Ok(None);
    };
    if !blob::record_holds_chunk(&record, index) {
        return Ok(None);
    }
    let chunk_cid = &manifest.chunk_cids[index];
    if !blob::throttle_fetch(storage, chunk_cid, ctx.now_ms)? {
        return Ok(None);
    }
    Ok(Some(PdsyncOut::BlobFetch {
        body: serde_json::json!({ "chunkCid": chunk_cid }),
    }))
}
