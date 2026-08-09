//! 删除投递日志（delete journal）：删除操作的可靠传播队列。
//!
//! 背景：折叠 vv 只保留 `nodeId → 最大序号` 一个维度，丢失了 key 维度——
//! 对端折叠摘要声明"见过本 nodeId 第 N 次写"推不出"知道 key K 被删"，
//! 导致墓碑在增量采集的 vv 比大小中被误判"已覆盖"而不推送（删除不同步
//! 的根因）。本模块把删除作为独立事实显式传播：
//!
//! - 每条删除（本地 [`crate::sync::delete_personal`] / 远端墓碑合入）追加
//!   一条日志：`dlog:entry:{seq:016}` → recordKey，`dlog:seq` 持当前最大
//!   序号（单调递增，与删除同 batch 提交）；
//! - 推送按对端 ACK 游标（hello/need 的 `dlogAck`）选取 `seq > ack` 的
//!   条目，不再参与折叠 vv 判定；未确认的下轮自动重推（落库侧 vv 幂等）；
//! - 接收方在 data 中按条目携带的 `dseq` 记录 `dlog:seen:{peerId}`，下轮
//!   hello/need 作为 `dlogAck` 回执；
//! - 发送方把对端回执落为 `dlog:wm:{peerId}`（GC 水位）。
//!
//! GC 严格规则：条目 seq ≤ min(设备清单中除本机外各设备的水位) 才可清理
//! （全员确认）；设备被解绑/移除即离开等待集合（清单以 deviceUid 绑定
//! 保持准确）；无其他设备时等待集合为空，条目直接可清（新设备从未拥有
//! 被删记录，不需要历史墓碑）。墓碑 pmeta 本身永久保留（防复活），GC 只
//! 清投递队列条目。

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::SyncResult;

/// 当前最大日志序号（u64 十进制字符串）。
const DLOG_SEQ_KEY: &str = "dlog:seq";
/// 日志条目前缀（`dlog:entry:{seq:016}` → recordKey）。
const DLOG_ENTRY_PREFIX: &str = "dlog:entry:";
/// 对端设备已确认水位前缀（`dlog:wm:{peerId}` → u64；我方日志的 GC 依据）。
const DLOG_WM_PREFIX: &str = "dlog:wm:";
/// 我已收到的对端日志序号前缀（`dlog:seen:{peerId}` → u64；hello/need
/// 的 `dlogAck` 回执来源）。
const DLOG_SEEN_PREFIX: &str = "dlog:seen:";
/// 历史墓碑回填完成标记（升级迁移：journal 引入前的既有墓碑补登一次）。
const DLOG_MIGRATED_KEY: &str = "dlog:migrated";

fn read_u64(raw: Option<String>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn entry_key(seq: u64) -> String {
    format!("{DLOG_ENTRY_PREFIX}{seq:016}")
}

fn wm_key(peer_id: &str) -> String {
    format!("{DLOG_WM_PREFIX}{peer_id}")
}

fn seen_key(peer_id: &str) -> String {
    format!("{DLOG_SEEN_PREFIX}{peer_id}")
}

/// 追加一条删除日志（seq = 当前最大 + 1），返回新序号。
///
/// 与删除本体/墓碑 pmeta 同一 batch 提交由调用方保证——本函数只构造
/// 两个 [`crate::storage::BatchOperation`]，由调用方并入其 batch。
pub(crate) fn append_ops<S: StorageBackend>(
    storage: &S,
    record_key: &str,
) -> SyncResult<(u64, Vec<crate::storage::BatchOperation>)> {
    let seq = read_u64(storage.get(DLOG_SEQ_KEY)?) + 1;
    Ok((
        seq,
        vec![
            crate::storage::BatchOperation::put(entry_key(seq), record_key),
            crate::storage::BatchOperation::put(DLOG_SEQ_KEY, seq.to_string()),
        ],
    ))
}

/// 读取 `seq > after` 的日志条目（升序）。
pub fn entries_after<S: StorageBackend>(
    storage: &S,
    after: u64,
) -> SyncResult<Vec<(u64, String)>> {
    let mut out = Vec::new();
    for (key, value) in storage.scan(&ScanOptions::prefix(DLOG_ENTRY_PREFIX))? {
        let Some(seq_str) = key.strip_prefix(DLOG_ENTRY_PREFIX) else {
            continue;
        };
        let Ok(seq) = seq_str.parse::<u64>() else {
            continue;
        };
        if seq > after {
            out.push((seq, value));
        }
    }
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}

/// 我方已收到的对端日志最大序号（作为发给它 hello/need 的 `dlogAck`）。
pub fn get_seen<S: StorageBackend>(storage: &S, peer_id: &str) -> SyncResult<u64> {
    Ok(read_u64(storage.get(&seen_key(peer_id))?))
}

/// 推进已收序号（只增不减）。
pub fn set_seen<S: StorageBackend>(
    storage: &mut S,
    peer_id: &str,
    seq: u64,
) -> SyncResult<()> {
    let current = get_seen(storage, peer_id)?;
    if seq > current {
        storage.put(&seen_key(peer_id), &seq.to_string())?;
    }
    Ok(())
}

/// 读取对端设备对我方日志的已确认水位（GC 依据）。
pub fn get_watermark<S: StorageBackend>(storage: &S, peer_id: &str) -> SyncResult<u64> {
    Ok(read_u64(storage.get(&wm_key(peer_id))?))
}

/// 推进对端确认水位（只增不减），返回推进后的值。
pub fn set_watermark<S: StorageBackend>(
    storage: &mut S,
    peer_id: &str,
    ack: u64,
) -> SyncResult<u64> {
    let current = get_watermark(storage, peer_id)?;
    let ack = ack.max(current);
    if ack > current {
        storage.put(&wm_key(peer_id), &ack.to_string())?;
    }
    Ok(ack)
}

/// 严格 GC：清理 `seq <= threshold` 的日志条目，返回清理条数。
///
/// threshold 由调用方按"全员确认"规则计算（见 [`gc_threshold`]）。
/// 只清投递队列条目；墓碑 pmeta 永久保留（防旧版本数据复活）。
pub fn gc<S: StorageBackend>(storage: &mut S, threshold: u64) -> SyncResult<usize> {
    if threshold == 0 {
        return Ok(0);
    }
    let mut removed = 0usize;
    let mut ops = Vec::new();
    for (key, _) in storage.scan(&ScanOptions::prefix(DLOG_ENTRY_PREFIX))? {
        let Some(seq_str) = key.strip_prefix(DLOG_ENTRY_PREFIX) else {
            continue;
        };
        let Ok(seq) = seq_str.parse::<u64>() else {
            continue;
        };
        if seq <= threshold {
            ops.push(crate::storage::BatchOperation::delete(key));
            removed += 1;
        }
    }
    if !ops.is_empty() {
        storage.batch(ops)?;
    }
    Ok(removed)
}

/// 计算 GC 阈值：设备清单中除本机外各设备确认水位的最小值。
///
/// - 等待集合为空（无其他设备）→ 当前最大序号（全部可清：新设备从未
///   拥有被删记录，不需要历史墓碑）；
/// - 某设备无水位记录（从未回执）→ 0（严格等待，阻塞清理）。
pub fn gc_threshold<S: StorageBackend>(
    storage: &S,
    device_peer_ids: &[String],
    self_peer_id: &str,
) -> SyncResult<u64> {
    let wait_set: Vec<&String> = device_peer_ids
        .iter()
        .filter(|p| p.as_str() != self_peer_id)
        .collect();
    if wait_set.is_empty() {
        return Ok(read_u64(storage.get(DLOG_SEQ_KEY)?));
    }
    let mut threshold = u64::MAX;
    for peer_id in wait_set {
        threshold = threshold.min(get_watermark(storage, peer_id)?);
    }
    Ok(if threshold == u64::MAX { 0 } else { threshold })
}

/// 升级迁移：journal 引入前已存在的墓碑 pmeta 一次性补登日志
/// （否则这些历史删除永远无法按新机制传播）。幂等（标记键门控）。
pub fn backfill_from_tombstones<S: StorageBackend>(storage: &mut S) -> SyncResult<usize> {
    if storage.get(DLOG_MIGRATED_KEY)?.is_some() {
        return Ok(0);
    }
    let mut seq = read_u64(storage.get(DLOG_SEQ_KEY)?);
    let mut count = 0usize;
    let mut ops = Vec::new();
    for (meta_key, raw) in storage.scan(&ScanOptions::prefix(crate::sync::personal::PMETA_PREFIX))? {
        let Ok(meta) = serde_json::from_str::<crate::sync::meta::DocMeta>(&raw) else {
            continue;
        };
        if !crate::sync::personal::is_tombstone(&meta) {
            continue;
        }
        let Some(record_key) = meta_key.strip_prefix(crate::sync::personal::PMETA_PREFIX) else {
            continue;
        };
        seq += 1;
        ops.push(crate::storage::BatchOperation::put(
            entry_key(seq),
            record_key,
        ));
        count += 1;
    }
    if count > 0 {
        ops.push(crate::storage::BatchOperation::put(
            DLOG_SEQ_KEY,
            seq.to_string(),
        ));
        storage.batch(ops)?;
    }
    storage.put(DLOG_MIGRATED_KEY, "1")?;
    Ok(count)
}

/// hello/need body 的 `dlogAck` 字段解析（缺省/非法 → 0，即"什么都还没
/// 收到"，触发全量日志推送——对旧版本对端自然兼容）。
pub fn parse_dlog_ack(body: &Value) -> u64 {
    body.get("dlogAck").and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{BatchOperation, MemoryStorage};
    use crate::sync::{
        apply_personal_remote, delete_personal, get_personal_meta, put_personal, set_personal_meta,
    };

    #[test]
    fn delete_appends_journal_entry() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, "node-a", "ct:friend:x", r#""v""#, 1000).unwrap();
        delete_personal(&mut s, "node-a", "ct:friend:x", 2000).unwrap();

        let entries = entries_after(&s, 0).unwrap();
        assert_eq!(entries, vec![(1, "ct:friend:x".to_string())]);
        assert!(entries_after(&s, 1).unwrap().is_empty());

        // 再删一条：序号单调递增
        put_personal(&mut s, "node-a", "ct:friend:y", r#""v""#, 3000).unwrap();
        delete_personal(&mut s, "node-a", "ct:friend:y", 4000).unwrap();
        let entries = entries_after(&s, 1).unwrap();
        assert_eq!(entries, vec![(2, "ct:friend:y".to_string())]);
    }

    /// 远端墓碑合入补登日志（接力传播：本机之后推给其他自设备）。
    #[test]
    fn remote_tombstone_apply_journals_relay() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, "node-a", "ct:friend:x", r#""v""#, 1000).unwrap();
        let tomb = crate::sync::meta::DocMeta {
            vv: [("node-a".to_string(), 2)].into_iter().collect(),
            ts: 2000,
            node_id: Some("node-a".to_string()),
            tombstone: Some(true),
        };
        let r = apply_personal_remote(&mut s, "ct:friend:x", "null", &tomb).unwrap();
        assert!(r.did_apply());
        let entries = entries_after(&s, 0).unwrap();
        assert_eq!(entries, vec![(1, "ct:friend:x".to_string())]);
    }

    #[test]
    fn seen_and_watermark_are_monotonic() {
        let mut s = MemoryStorage::new();
        set_seen(&mut s, "peer-b", 5).unwrap();
        set_seen(&mut s, "peer-b", 3).unwrap();
        assert_eq!(get_seen(&s, "peer-b").unwrap(), 5, "seen 只增不减");

        assert_eq!(set_watermark(&mut s, "peer-b", 4).unwrap(), 4);
        assert_eq!(set_watermark(&mut s, "peer-b", 2).unwrap(), 4, "水位只增不减");
        assert_eq!(get_watermark(&s, "peer-b").unwrap(), 4);
    }

    /// 严格 GC：全员确认才可清理——任何一台设备未确认即阻塞；设备离开
    /// 清单（解绑）即退出等待集合。
    #[test]
    fn gc_waits_for_all_devices() {
        let mut s = MemoryStorage::new();
        for i in 0..5 {
            let key = format!("ct:friend:k{i}");
            put_personal(&mut s, "node-a", &key, r#""v""#, 1000).unwrap();
            delete_personal(&mut s, "node-a", &key, 2000).unwrap();
        }
        let devices = vec!["peer-b".to_string(), "peer-c".to_string()];

        // 两台都未回执 → 阈值 0，清不掉任何条目
        let t = gc_threshold(&s, &devices, "peer-self").unwrap();
        assert_eq!(t, 0);
        assert_eq!(gc(&mut s, t).unwrap(), 0);

        // 只有 peer-b 回执到 5 → peer-c 缺席（水位 0）仍阻塞
        set_watermark(&mut s, "peer-b", 5).unwrap();
        let t = gc_threshold(&s, &devices, "peer-self").unwrap();
        assert_eq!(t, 0);

        // peer-c 回执到 3 → 阈值 3，清 seq≤3 的三条
        set_watermark(&mut s, "peer-c", 3).unwrap();
        let t = gc_threshold(&s, &devices, "peer-self").unwrap();
        assert_eq!(t, 3);
        assert_eq!(gc(&mut s, t).unwrap(), 3);
        assert_eq!(entries_after(&s, 0).unwrap().len(), 2);

        // peer-c 被解绑（离开清单）→ 只看 peer-b（水位 5）→ 全部可清
        let devices = vec!["peer-b".to_string()];
        let t = gc_threshold(&s, &devices, "peer-self").unwrap();
        assert_eq!(t, 5);
        assert_eq!(gc(&mut s, t).unwrap(), 2);
        assert!(entries_after(&s, 0).unwrap().is_empty());
    }

    /// 无其他设备（等待集合为空）→ 直接全部可清：新设备从未拥有被删
    /// 记录，不需要历史墓碑。
    #[test]
    fn gc_empty_wait_set_clears_all() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, "node-a", "ct:friend:x", r#""v""#, 1000).unwrap();
        delete_personal(&mut s, "node-a", "ct:friend:x", 2000).unwrap();

        let t = gc_threshold(&s, &[], "peer-self").unwrap();
        assert_eq!(t, 1);
        assert_eq!(gc(&mut s, t).unwrap(), 1);
        assert!(entries_after(&s, 0).unwrap().is_empty());
        // 墓碑 pmeta 不受 GC 影响（防旧版本数据复活）
        let meta = get_personal_meta(&s, "ct:friend:x").unwrap().unwrap();
        assert!(crate::sync::is_tombstone(&meta));
    }

    /// 升级迁移：journal 引入前的既有墓碑一次性补登；幂等。
    #[test]
    fn backfill_migrates_legacy_tombstones_once() {
        let mut s = MemoryStorage::new();
        // 手工造一条"无日志的历史墓碑"（绕过 delete_personal 的日志追加）
        put_personal(&mut s, "node-a", "ct:friend:old", r#""v""#, 1000).unwrap();
        let mut meta = get_personal_meta(&s, "ct:friend:old").unwrap().unwrap();
        meta.tombstone = Some(true);
        s.batch(vec![
            BatchOperation::delete("ct:friend:old"),
            BatchOperation::put(
                crate::sync::personal_meta_key("ct:friend:old"),
                serde_json::to_string(&meta).unwrap(),
            ),
        ])
        .unwrap();
        set_personal_meta(&mut s, "ct:friend:old", &meta).unwrap();
        assert!(entries_after(&s, 0).unwrap().is_empty(), "迁移前无日志");

        let n = backfill_from_tombstones(&mut s).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            entries_after(&s, 0).unwrap(),
            vec![(1, "ct:friend:old".to_string())]
        );
        // 幂等：二次迁移不再补登
        assert_eq!(backfill_from_tombstones(&mut s).unwrap(), 0);
        assert_eq!(entries_after(&s, 0).unwrap().len(), 1);
    }

    /// 删除后重建的记录：历史日志条目不得再推墓碑（pmeta 已不是墓碑）。
    #[test]
    fn recreated_record_stale_entry_not_pushed() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, "node-a", "ct:friend:x", r#""v1""#, 1000).unwrap();
        delete_personal(&mut s, "node-a", "ct:friend:x", 2000).unwrap();
        // 重建同名记录（墓碑清除）
        put_personal(&mut s, "node-a", "ct:friend:x", r#""v2""#, 3000).unwrap();

        let category = crate::sync::pdsync::category_by_name("ct:friend").unwrap();
        let tombs = crate::sync::pdsync::collect_tombstones_after(&s, category, None, 0).unwrap();
        assert!(tombs.is_empty(), "重建后历史删除条目不得误推");
    }
}
