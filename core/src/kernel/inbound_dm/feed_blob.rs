//! dm 入站编排（feed-blob 系）：跨联系人 blob 分块拉取。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §7] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.6]。从 `inbound_dm` 拆出的子模块，
//! 共享父模块的 [`InboundContext`]/应答助手。
//!
//! - `feed-blob-req`：服务方（feed 原作者 rootId）从本地本体切一块回
//!   `feed-blob-resp {hash, offset, data, totalBytes}`；缺本体回 `missing:true`；
//! - `feed-blob-resp`：请求方按序合块，收齐做尺寸 + SHA-256 校验提升，未齐
//!   续拉下一块（`feed-blob-req`）。
//!
//! 复用 `plugindata::blob` 的分块语义（`serve_chunk`/`ingest_chunk`，
//! 与 pdsync-attachment 同构）。鉴权：hash 即能力（256 bit 内容寻址），
//! 请求方必须是本机朋友（§7.3）；入站限流豁免（§4.4）。

use serde_json::{Value, json};

use super::{
    FeedBlobOut, InboundContext, InboundDmResult, Result, done, fail_response, ok_response,
};
use crate::contact::ContactService;
use crate::plugindata::blob;
use crate::storage::StorageBackend;

/// 结果模板：无事件、仅 feed-blob 出站指令。
fn with_out(out: FeedBlobOut) -> Result<InboundDmResult> {
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
        feed_blob_out: Some(out),
    })
}

/// 请求方是否本机朋友（feed-blob 鉴权：hash 即能力，但请求方必须是朋友）。
fn is_friend<S: StorageBackend>(storage: &S, from: &str) -> bool {
    ContactService::get_friend(storage, from).ok().flatten().is_some()
}

/// `feed-blob-req`：服务本地本体分块（对齐 attachment.rs 的 `serve_chunk`）。
pub(super) fn handle_feed_blob_req<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    // 鉴权：请求方必须是本机朋友（§7.3），否则静默拒（不回数据）
    if !is_friend(storage, from) {
        return done(ok_response(), Vec::new());
    }
    let Some(hash) = body.get("hash").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let offset = body
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    // 逐 hash 节流（§19.6 服务方 `BLOB_REQ_THROTTLE_MS` 口径）：仅对**首块**
    // （offset 0）限流——续拉块（offset > 0）是一次活跃拉取的连续往返，不得
    // 被打断；首块限流防止对端在窗口内反复发起同 hash 的拉取（防洪）。
    if offset == 0 && !blob::throttle_request(storage, hash, ctx.now_ms)? {
        return done(ok_response(), Vec::new());
    }
    let resp_body = match blob::serve_chunk(storage, hash, offset)? {
        Some((data, total_bytes)) => json!({
            "hash": hash,
            "offset": offset,
            "data": data,
            "totalBytes": total_bytes,
        }),
        // 本地暂无本体：如实回 missing，请求方节流后重试
        None => json!({ "hash": hash, "offset": offset, "missing": true }),
    };
    with_out(FeedBlobOut::BlobResp { body: resp_body })
}

/// `feed-blob-resp`：按序合块，收齐校验提升，未齐续拉下一块。
pub(super) fn handle_feed_blob_resp<S: StorageBackend>(
    storage: &mut S,
    _ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    // 应答必须来自本机朋友（且该 hash 有 feed 来源登记——未登记则来源不可信，
    // 静默丢弃；请求方只向登记来源拉取）
    if !is_friend(storage, from) {
        return done(ok_response(), Vec::new());
    }
    let Some(hash) = body.get("hash").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // 服务方缺本体：放弃本次（下轮调和再试）
    if body.get("missing").and_then(Value::as_bool) == Some(true) {
        return done(ok_response(), Vec::new());
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
        log::info!("[FEED-BLOB] assembled | hash={}", &hash[..16.min(hash.len())]);
        return done(ok_response(), Vec::new());
    }
    // 未收齐且本块被接受（offset 对齐）→ 续拉下一块
    let next_offset = offset as usize + base64_chunk_bytes(data);
    let part_bytes = storage
        .get(&blob::blob_part_key(hash))
        .map_err(crate::sync::SyncError::from)?
        .map(|s| s.len() / 4 * 3)
        .unwrap_or(0);
    if next_offset == part_bytes {
        return with_out(FeedBlobOut::BlobReq {
            body: json!({ "hash": hash, "offset": next_offset }),
        });
    }
    done(ok_response(), Vec::new())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use base64::Engine as _;
    use crate::contact::{ContactService, FriendRecord};
    use crate::storage::MemoryStorage;

    fn ctx<'a>(online: &'a HashSet<String>) -> InboundContext<'a> {
        InboundContext {
            my_root_id: "me",
            my_nickname: "我",
            remote_peer_id: "peer-x",
            online_peers: online,
            node_id: "node-me",
            now_ms: 1000,
        }
    }

    fn empty_online() -> HashSet<String> {
        HashSet::new()
    }

    fn friend(root_id: &str) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            nickname: "朋友".to_string(),
            permission: "open".to_string(),
            ..Default::default()
        }
    }

    /// feed-blob 分块往返：服务方切块、请求方合块，收齐后本体可读。
    #[test]
    fn feed_blob_req_resp_roundtrip() {
        let mut server = MemoryStorage::new();
        let mut client = MemoryStorage::new();
        ContactService::upsert_friend(&mut server, &friend("clientRoot")).unwrap();
        ContactService::upsert_friend(&mut client, &friend("serverRoot")).unwrap();

        // 服务方保存本体（2.5 块）
        let data: Vec<u8> = (0..(blob::BLOB_CHUNK_BYTES * 2 + 1234)).map(|i| (i % 251) as u8).collect();
        let info = blob::save_blob(&mut server, &data).unwrap();

        // 客户端请求 offset 0 → 服务方回第一块
        let req0 = json!({ "hash": info.hash, "offset": 0 });
        let r = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "clientRoot", &req0).unwrap();
        let resp0 = r.feed_blob_out.expect("服务方应回 feed-blob-resp");
        assert_eq!(resp0.kind(), crate::kernel::dm_envelope::KIND_FEED_BLOB_RESP);
        assert_eq!(resp0.body()["totalBytes"], json!(data.len() as u64));

        // 请求方收块（offset 对齐）→ 未齐续拉下一块
        let r = handle_feed_blob_resp(&mut client, &ctx(&empty_online()), "serverRoot", resp0.body()).unwrap();
        let next = r.feed_blob_out.expect("未收齐应续拉下一块");
        assert_eq!(next.kind(), crate::kernel::dm_envelope::KIND_FEED_BLOB_REQ);

        // 继续拉取直到收齐。首块（offset 0）已由 req0 取到，服务方对同 hash
        // 首块逐 hash 节流（I5 §19.6），故续拉从下一块（BLOB_CHUNK_BYTES）开始，
        // 不再重复请求 offset 0。
        let mut offset = blob::BLOB_CHUNK_BYTES;
        let mut done = false;
        while !done {
            let req = json!({ "hash": info.hash, "offset": offset });
            let r = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "clientRoot", &req).unwrap();
            let resp = r.feed_blob_out.expect("服务方应回块");
            offset += crate::plugindata::blob::BLOB_CHUNK_BYTES.min(data.len().saturating_sub(offset));
            let r = handle_feed_blob_resp(&mut client, &ctx(&empty_online()), "serverRoot", resp.body()).unwrap();
            if r.feed_blob_out.is_none() && blob::has_blob(&client, &info.hash) {
                done = true;
            }
        }
        assert!(blob::has_blob(&client, &info.hash), "收齐后本体可读");
        let got = base64::engine::general_purpose::STANDARD
            .decode(blob::read_blob(&client, &info.hash).unwrap().unwrap())
            .unwrap();
        assert_eq!(got, data, "分块拼接后内容一致");
    }

    /// 服务方首块逐 hash 节流：窗口内同 hash 首块请求被跳过，续拉块不受限。
    #[test]
    fn feed_blob_req_first_chunk_throttled() {
        let mut server = MemoryStorage::new();
        ContactService::upsert_friend(&mut server, &friend("clientRoot")).unwrap();
        let data: Vec<u8> = (0..(blob::BLOB_CHUNK_BYTES + 10)).map(|i| (i % 251) as u8).collect();
        let info = blob::save_blob(&mut server, &data).unwrap();

        // 首块（offset 0）：首次放行（登记节流时间戳）
        let r0 = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "clientRoot", &json!({ "hash": info.hash, "offset": 0 })).unwrap();
        assert!(r0.feed_blob_out.is_some(), "首次首块应放行");

        // 窗口内再次首块 → 节流（不回数据）
        let r1 = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "clientRoot", &json!({ "hash": info.hash, "offset": 0 })).unwrap();
        assert!(r1.feed_blob_out.is_none(), "窗口内重复首块应被节流跳过");

        // 续拉块（offset > 0）不受节流影响（活跃拉取连续往返）
        let r2 = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "clientRoot", &json!({ "hash": info.hash, "offset": blob::BLOB_CHUNK_BYTES })).unwrap();
        assert!(r2.feed_blob_out.is_some(), "续拉块不应被节流");

        // 跨过节流窗口后首块重新放行
        let later_online = empty_online();
        let mut later_ctx = ctx(&later_online);
        later_ctx.now_ms = 1000 + blob::BLOB_REQ_THROTTLE_MS;
        let r3 = handle_feed_blob_req(&mut server, &later_ctx, "clientRoot", &json!({ "hash": info.hash, "offset": 0 })).unwrap();
        assert!(r3.feed_blob_out.is_some(), "跨窗口后首块重新放行");
    }

    /// 服务方缺本体 → missing:true；非朋友请求 → 静默拒（不回数据）。
    #[test]
    fn feed_blob_missing_and_nonfriend() {
        let mut server = MemoryStorage::new();
        ContactService::upsert_friend(&mut server, &friend("friendRoot")).unwrap();
        // 非朋友请求：不回数据（ok 但无 feed_blob_out）
        let r = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "stranger", &json!({ "hash": "h", "offset": 0 })).unwrap();
        assert!(r.feed_blob_out.is_none(), "非朋友请求不回数据");
        // 朋友但 hash 缺失 → missing:true
        let r = handle_feed_blob_req(&mut server, &ctx(&empty_online()), "friendRoot", &json!({ "hash": "nope", "offset": 0 })).unwrap();
        let resp = r.feed_blob_out.expect("朋友请求应回应答");
        assert_eq!(resp.body()["missing"], json!(true));
    }
}
