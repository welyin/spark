//! `pdsync-attachment-req/resp` 入站处理：P6 blob 按需拉取（自设备间，
//! 验签与 from==self 口径同 pdsync）。
//!
//! - 请求方：`{hash, offset}` —— 顺序拉取，offset = 已收字节数；
//! - 服务方：从本地本体切 [`BLOB_CHUNK_BYTES`] 一块回 `{hash, offset, data,
//!   totalBytes}`；缺本体回 `missing: true`（请求方下轮调和重试，可能彼时
//!   服务方已就绪或换其他持有设备）；
//! - 接收方：按序追加（base64 块长 3 的倍数可直接拼接），收齐做尺寸 +
//!   SHA-256 校验，通过提升为本体；后续块经 [`PdsyncOut::AttachReq`] 续拉。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, PdsyncOut, Result, done, fail_response, ok_response,
};
use crate::plugindata::blob;
use crate::storage::StorageBackend;

/// 结果模板：无事件、无自记录动作，仅 pdsync_out。
fn with_out(out: Vec<PdsyncOut>) -> Result<InboundDmResult> {
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: out,
        profile_applied: false,
    })
}

/// `pdsync-attachment-req`：服务本地本体分块。
pub(super) fn handle_attachment_req<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some(hash) = body.get("hash").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let offset = body
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let resp_body = match blob::serve_chunk(storage, hash, offset)? {
        Some((data, total_bytes)) => json!({
            "hash": hash,
            "offset": offset,
            "data": data,
            "totalBytes": total_bytes,
        }),
        // 本地暂无本体（可能记录先到、blob 尚未拉到）：如实回 missing，
        // 请求方节流后下轮调和重试
        None => json!({ "hash": hash, "offset": offset, "missing": true }),
    };
    with_out(vec![PdsyncOut::AttachResp { body: resp_body }])
}

/// `pdsync-attachment-resp`：按序合块，收齐校验提升，未齐续拉下一块。
pub(super) fn handle_attachment_resp<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some(hash) = body.get("hash").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // 服务方缺本体：放弃本次（下轮调和再试，或对端彼时已就绪）
    if body.get("missing").and_then(Value::as_bool) == Some(true) {
        return with_out(Vec::new());
    }
    let (Some(offset), Some(data), Some(total)) = (
        body.get("offset").and_then(Value::as_u64),
        body.get("data").and_then(Value::as_str),
        body.get("totalBytes").and_then(Value::as_u64),
    ) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let completed = blob::ingest_chunk(storage, hash, offset as usize, data, total)?;
    if completed {
        log::info!("[BLOB] assembled | hash={} bytes={}", &hash[..16.min(hash.len())], total);
        return with_out(Vec::new());
    }
    // 未收齐且本块被接受（offset 对齐）→ 续拉下一块；未对齐（乱序）则等调和
    let next_offset = offset as usize + base64_chunk_bytes(data);
    let part_bytes = storage
        .get(&blob::blob_part_key(hash))
        .map_err(crate::sync::SyncError::from)?
        .map(|s| s.len() / 4 * 3)
        .unwrap_or(0);
    if next_offset == part_bytes {
        return with_out(vec![PdsyncOut::AttachReq {
            body: json!({ "hash": hash, "offset": next_offset }),
        }]);
    }
    with_out(Vec::new())
}

/// base64 串对应的原始字节数（末块含 padding 时减 1/2）。
fn base64_chunk_bytes(b64: &str) -> usize {
    let padding = if b64.ends_with("==") {
        2
    } else if b64.ends_with('=') {
        1
    } else {
        0
    };
    b64.len() / 4 * 3 - padding
}

/// hello 调和：把缺失 blob 的拉取请求并入 hello 响应的出站队列。
/// PC 扫全部 pdoc 引用（eager）；其余设备类只取 want 标记（lazy）。
/// 逐 hash 节流（[`blob::BLOB_REQ_THROTTLE_MS`]）。
pub(super) fn reconcile_blob_pulls<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    out: &mut Vec<PdsyncOut>,
) -> Result<()> {
    let scan_records = crate::sync::pdsync::local_device_class() == "pc";
    for hash in blob::missing_blobs(storage, scan_records)? {
        if blob::throttle_request(storage, &hash, ctx.now_ms)? {
            out.push(PdsyncOut::AttachReq {
                body: json!({ "hash": hash, "offset": 0 }),
            });
        }
    }
    // 顺带 GC：PC（引用面扫描口径与 eager 拉取同频）回收宽限期届满的无引用
    // 本体；记录墓碑永存，误收可由远端重拉恢复
    if scan_records {
        let collected = blob::gc_blobs(storage, ctx.now_ms)?;
        if !collected.is_empty() {
            log::info!("[BLOB] gc collected {}", collected.len());
        }
    }
    Ok(())
}
