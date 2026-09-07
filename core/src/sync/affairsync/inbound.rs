//! affairsync hello/need 入站编排（affair-sync §7）：diff 裁决 → 回 need /
//! 推 data。纯逻辑（不触碰 p2p/签名），信封投递由调用方完成。
//!
//! 入站前提：本机已关注该事务（affair-sync §7：未关注静默丢弃）；
//! 对端 from 落关注者目录（覆盖网线索，§6）。

use serde_json::Value;

use crate::storage::StorageBackend;
use crate::sync::SyncResult;
use crate::sync::meta::VersionVector;

use super::collect::{AffairDiffOutcome, collect_affair_records, collect_affair_vv, diff_affair};
use super::dir::note_follower_seen;
use super::envelope::{
    AFFAIRSYNC_BATCH_BYTES, AffairsyncOut, build_affairsync_data_batch, build_affairsync_need,
    parse_affairsync_hello, parse_affairsync_need, split_affairsync_batches,
};
use super::follow::is_following;

/// affairsync-hello 入站：摘要交换。返回应答出向（need/data），未关注或
/// 解析失败返回 None（静默丢弃，§7）。
pub fn handle_affairsync_hello<S: StorageBackend>(
    storage: &mut S,
    from_root_id: &str,
    from_peer_id: Option<&str>,
    body: &Value,
    now_ms: i64,
) -> SyncResult<Option<AffairsyncOut>> {
    let Some((affair_id, remote_vv, _heads, _device_class)) = parse_affairsync_hello(body) else {
        log::info!("[AFFAIRSYNC] hello rejected: invalid-body");
        return Ok(None);
    };
    if !is_following(storage, &affair_id)? {
        log::info!("[AFFAIRSYNC] hello ignored: not following affair={affair_id}");
        return Ok(None);
    }
    note_follower_seen(storage, &affair_id, from_root_id, from_peer_id, now_ms)?;

    let local_vv = collect_affair_vv(storage, &affair_id)?;
    let mut out = AffairsyncOut::default();
    match diff_affair(&local_vv, &remote_vv) {
        AffairDiffOutcome::LocalBehind { local_vv } => {
            out.need = Some(build_affairsync_need(&affair_id, &local_vv));
        }
        AffairDiffOutcome::LocalAhead => {
            out.data = collect_and_batch(storage, &affair_id, &remote_vv)?;
        }
        AffairDiffOutcome::Concurrent { local_vv } => {
            out.need = Some(build_affairsync_need(&affair_id, &local_vv));
            out.data = collect_and_batch(storage, &affair_id, &remote_vv)?;
        }
        AffairDiffOutcome::Equal => {}
    }
    Ok(Some(out))
}

/// affairsync-need 入站：diff 请求。采集增量分批发回；未关注或解析失败
/// 返回 None（§7）。
pub fn handle_affairsync_need<S: StorageBackend>(
    storage: &mut S,
    from_root_id: &str,
    from_peer_id: Option<&str>,
    body: &Value,
    now_ms: i64,
) -> SyncResult<Option<Vec<Value>>> {
    let Some((affair_id, known_vv)) = parse_affairsync_need(body) else {
        log::info!("[AFFAIRSYNC] need rejected: invalid-body");
        return Ok(None);
    };
    if !is_following(storage, &affair_id)? {
        log::info!("[AFFAIRSYNC] need ignored: not following affair={affair_id}");
        return Ok(None);
    }
    note_follower_seen(storage, &affair_id, from_root_id, from_peer_id, now_ms)?;
    Ok(Some(collect_and_batch(storage, &affair_id, &known_vv)?))
}

/// 采集增量并按批字节上限切分装配 data body 列表。
fn collect_and_batch<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
    known_vv: &VersionVector,
) -> SyncResult<Vec<Value>> {
    let records = collect_affair_records(storage, affair_id, known_vv)?;
    let batches = split_affairsync_batches(records, AFFAIRSYNC_BATCH_BYTES);
    let total = batches.len();
    Ok(batches
        .into_iter()
        .enumerate()
        .map(|(i, batch)| build_affairsync_data_batch(affair_id, &batch, i, total))
        .collect())
}
