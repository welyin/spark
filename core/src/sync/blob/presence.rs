//! presence 副本账本：`blob:presence:{cid}:{deviceUid}` 记录（含 chunk 位图），
//! 纳入 pdsync 同步（核心数据通道）——每台设备凭相同记录集合**确定性**复算
//! 每个 blob 的域内副本数（健康度与 A2 驱逐判定的共同输入）。
//!
//! 线形见 `wiki/protocol/p2p/personal-data-sync.md` §14.4。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use super::{SyncResult, get_manifest, held_chunk_indices};
use crate::storage::{ScanOptions, StorageBackend};

/// presence 记录键前缀（pdsync category `blob:presence` 的唯一前缀）。
pub const PRESENCE_KEY_PREFIX: &str = "blob:presence:";

/// presence 记录键（`blob:presence:{cid}:{deviceUid}`）。
pub fn presence_key(cid: &str, device_uid: &str) -> String {
    format!("{PRESENCE_KEY_PREFIX}{cid}:{device_uid}")
}

/// 某 blob 全部 presence 记录的扫描前缀。
pub fn presence_prefix_for(cid: &str) -> String {
    format!("{PRESENCE_KEY_PREFIX}{cid}:")
}

/// presence 记录（serde 字段序即线形字节序：v, cid, deviceUid, chunks）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRecord {
    pub v: u32,
    pub cid: String,
    pub device_uid: String,
    /// 持有位图（base64 最短长度；bit i = 持有第 i 块；空位图 = ""）。
    pub chunks: String,
}

impl PresenceRecord {
    /// 规范 JSON（紧凑、字段序固定）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 解析（v != 1 或字段缺失 → None）。
    pub fn from_json(raw: &str) -> Option<Self> {
        let r: Self = serde_json::from_str(raw).ok()?;
        if r.v != 1 {
            return None;
        }
        Some(r)
    }
}

// ── 位图编解码 ─────────────────────────────────────────────────────

/// 位图编码：byte[i/8] 的 bit (i%8)（LSB-first）置位 = 持有第 i 块；
/// **最短长度**（尾部零字节裁剪），base64 标准表；全不持有 → 空字符串。
pub fn bitmap_encode(held: &[bool]) -> String {
    let mut bytes = vec![0u8; held.len().div_ceil(8)];
    for (i, h) in held.iter().enumerate() {
        if *h {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    B64.encode(bytes)
}

/// 位图解码（base64 非法 → None）。解码长度 = base64 字节数 × 8 位
/// （调用方按 manifest 块数截断解读，短缺位视为未持有）。
pub fn bitmap_decode(s: &str) -> Option<Vec<bool>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    let bytes = B64.decode(s).ok()?;
    let mut held = Vec::with_capacity(bytes.len() * 8);
    for byte in &bytes {
        for bit in 0..8 {
            held.push(byte & (1 << bit) != 0);
        }
    }
    Some(held)
}

/// 位图是否覆盖全部 `chunk_count` 块（完整副本判定）。
fn bitmap_holds_all(decoded: &[bool], chunk_count: usize) -> bool {
    chunk_count == decoded.iter().take(chunk_count).filter(|h| **h).count()
}

/// 位图中第 `index` 块是否持有（短缺位 = 未持有）。
pub fn bitmap_holds(decoded: &[bool], index: usize) -> bool {
    decoded.get(index).copied().unwrap_or(false)
}

// ── 记录读写 ───────────────────────────────────────────────────────

/// 持有变更即刷新：按 manifest 重算本机位图——
/// - 有持有 → `put_personal` 受管写入（纳入 pdsync 反熵扩散）；
/// - 全零 → `delete_personal` 墓碑化（随 dlog 传播，即「presence 同步标记移除」）；
/// - 值未变 → 不 bump（避免无谓的版本向量推进触发回声 hello）。
///
/// manifest 缺失时不动作（无法把 chunkCid 映射到位图位次；记录保持原状）。
pub fn refresh_presence<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    cid: &str,
    now_ms: i64,
) -> SyncResult<()> {
    let Some(manifest) = get_manifest(storage, cid)? else {
        return Ok(());
    };
    let held = held_chunk_indices(storage, &manifest)?;
    let key = presence_key(cid, device_uid);
    if held.iter().all(|h| !h) {
        if storage.get(&key)?.is_some() {
            crate::sync::personal::delete_personal(storage, node_id, &key, now_ms)?;
        }
        return Ok(());
    }
    let record = PresenceRecord {
        v: 1,
        cid: cid.to_string(),
        device_uid: device_uid.to_string(),
        chunks: bitmap_encode(&held),
    };
    let raw = record.to_json();
    if storage.get(&key)?.as_deref() == Some(raw.as_str()) {
        return Ok(());
    }
    crate::sync::personal::put_personal(storage, node_id, &key, &raw, now_ms)?;
    Ok(())
}

/// 列出某 blob 的全部 presence 记录（损坏记录跳过——不以假数据参与计数；
/// 记录体含 deviceUid，重复/伪造 deviceUid 的记录按 deviceUid 去重，
/// 同 deviceUid 多条时取键序最后一条——确定性规则，防一设备多 key 虚增副本）。
pub fn list_presence<S: StorageBackend>(
    storage: &S,
    cid: &str,
) -> SyncResult<Vec<PresenceRecord>> {
    let mut by_uid: std::collections::BTreeMap<String, PresenceRecord> =
        std::collections::BTreeMap::new();
    for (_key, raw) in storage.scan(&ScanOptions::prefix(&presence_prefix_for(cid)))? {
        let Some(record) = PresenceRecord::from_json(&raw) else {
            continue;
        };
        if record.cid != cid || record.device_uid.is_empty() {
            continue;
        }
        by_uid.insert(record.device_uid.clone(), record);
    }
    Ok(by_uid.into_values().collect())
}

// ── 确定性副本计数（A2 驱逐选择器与 A3 健康度共用输入）─────────────

/// 完整副本数：位图覆盖全部 `chunk_count` 块的设备数（记录已按 deviceUid
/// 去重，见 [`list_presence`]）。`chunk_count == 0`（空 blob）：持有 manifest
/// 即完整——凡有 presence 记录皆计（空 blob 的 presence 位图恒为 ""，
/// 由 [`refresh_presence`] 特判写入；见模块测试）。
pub fn full_replica_count(records: &[PresenceRecord], chunk_count: usize) -> usize {
    if chunk_count == 0 {
        return records.len();
    }
    records
        .iter()
        .filter_map(|r| bitmap_decode(&r.chunks))
        .filter(|decoded| bitmap_holds_all(decoded, chunk_count))
        .count()
}

/// 逐块副本数：各记录位图按位求和（块 i 的副本数 = 持有块 i 的设备数）。
pub fn chunk_replica_counts(records: &[PresenceRecord], chunk_count: usize) -> Vec<u32> {
    let mut counts = vec![0u32; chunk_count];
    for record in records {
        let Some(decoded) = bitmap_decode(&record.chunks) else {
            continue;
        };
        for (i, count) in counts.iter_mut().enumerate() {
            if bitmap_holds(&decoded, i) {
                *count += 1;
            }
        }
    }
    counts
}

/// 存储级摘要：`(完整副本数, 逐块副本数)`；本机无 manifest（块数未知）
/// 时返回 None（不可计数——计数的前提是块数 n 的唯一来源 manifest）。
pub fn replica_summary<S: StorageBackend>(
    storage: &S,
    cid: &str,
) -> SyncResult<Option<(usize, Vec<u32>)>> {
    let Some(manifest) = get_manifest(storage, cid)? else {
        return Ok(None);
    };
    let records = list_presence(storage, cid)?;
    let n = manifest.chunk_count();
    Ok(Some((
        full_replica_count(&records, n),
        chunk_replica_counts(&records, n),
    )))
}

/// 供 [`super::fetch`] 的持有者查询：持有第 `index` 块的 deviceUid 列表
/// （确定性升序——与记录传入顺序无关，拉取规划在任何设备上算出同一持有者）。
pub fn holders_of_chunk(records: &[PresenceRecord], index: usize) -> Vec<String> {
    let mut holders: Vec<String> = records
        .iter()
        .filter(|r| bitmap_decode(&r.chunks).is_some_and(|d| bitmap_holds(&d, index)))
        .map(|r| r.device_uid.clone())
        .collect();
    holders.sort();
    holders
}

/// 供 [`super::fetch`] 的 manifest 持有者查询：有 presence 记录即视为持有
/// manifest（presence 只在 manifest 在握时写入，见 [`refresh_presence`]）。
/// 确定性升序（与记录传入顺序无关）。
pub fn manifest_holders(records: &[PresenceRecord]) -> Vec<String> {
    let mut holders: Vec<String> = records.iter().map(|r| r.device_uid.clone()).collect();
    holders.sort();
    holders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::personal::{get_personal_meta, is_tombstone};

    #[test]
    fn bitmap_roundtrip_and_minimal_length() {
        // 9 块持有 {0,3,8}：byte0=0b00001001, byte1=0b00000001
        let held = [true, false, false, true, false, false, false, false, true];
        let enc = bitmap_encode(&held);
        assert_eq!(enc, "CQE=");
        let dec = bitmap_decode(&enc).unwrap();
        assert_eq!(&dec[..9], &held[..]);
        // 最短长度：尾部零字节裁剪
        assert_eq!(bitmap_encode(&[true, false, false]), "AQ==");
        assert_eq!(bitmap_encode(&[false, false, false]), "");
        assert_eq!(bitmap_encode(&[]), "");
        assert_eq!(bitmap_decode(""), Some(vec![]));
        assert!(bitmap_decode("!!not-base64!!").is_none());
    }

    fn record(cid: &str, uid: &str, held: &[bool]) -> PresenceRecord {
        PresenceRecord {
            v: 1,
            cid: cid.to_string(),
            device_uid: uid.to_string(),
            chunks: bitmap_encode(held),
        }
    }

    #[test]
    fn deterministic_replica_counting() {
        // 5 块 blob：A/B 完整，C 部分（{0,2,4}）
        let full = &[true; 5];
        let a = record("cid-x", "uid-a", full);
        let b = record("cid-x", "uid-b", full);
        let c = record("cid-x", "uid-c", &[true, false, true, false, true]);
        // 两种插入顺序结果一致（确定性）
        for records in [
            vec![a.clone(), b.clone(), c.clone()],
            vec![c.clone(), a.clone(), b.clone()],
        ] {
            assert_eq!(full_replica_count(&records, 5), 2);
            assert_eq!(chunk_replica_counts(&records, 5), vec![3, 2, 3, 2, 3]);
        }
        // 空 blob：有记录即完整
        let e = record("cid-empty", "uid-a", &[]);
        assert_eq!(full_replica_count(&[e], 0), 1);
        // 持有者查询确定性升序
        assert_eq!(
            holders_of_chunk(&[c.clone(), a.clone(), b.clone()], 0),
            vec!["uid-a", "uid-b", "uid-c"]
        );
        assert_eq!(holders_of_chunk(&[a, b, c], 1), vec!["uid-a", "uid-b"]);
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn refresh_writes_and_tombstones() {
        let mut s = MemoryStorage::new();
        let data = pattern(1000);
        let m = super::super::save_blob(&mut s, "node-a", "uid-a", &data, 1000).unwrap();
        let key = presence_key(&m.cid, "uid-a");
        let raw = s.get(&key).unwrap().expect("presence 已写");
        let r = PresenceRecord::from_json(&raw).unwrap();
        assert_eq!(r.chunks, "AQ==", "单块持有位图");
        // 幂等：重复刷新不 bump vv
        let meta1 = get_personal_meta(&s, &key).unwrap().unwrap();
        refresh_presence(&mut s, "node-a", "uid-a", &m.cid, 2000).unwrap();
        let meta2 = get_personal_meta(&s, &key).unwrap().unwrap();
        assert_eq!(meta1.vv, meta2.vv, "值未变不 bump");
        // 弃块 → 全零 → 墓碑化
        super::super::drop_local_chunks(&mut s, "node-a", "uid-a", &m.cid, 3000).unwrap();
        assert!(s.get(&key).unwrap().is_none());
        let meta = get_personal_meta(&s, &key).unwrap().unwrap();
        assert!(is_tombstone(&meta), "全零位图墓碑化移除");
    }

    #[test]
    fn list_presence_dedups_device_uid_and_skips_garbage() {
        let mut s = MemoryStorage::new();
        let cid = "a".repeat(64);
        // 同 deviceUid 两条记录（伪造/异常）：按键序取最后一条，计数不虚增
        s.put(
            &presence_key(&cid, "dup"),
            &record(&cid, "dup", &[true]).to_json(),
        )
        .unwrap();
        s.put(
            &format!("{}zz", presence_prefix_for(&cid)),
            &record(&cid, "dup", &[true]).to_json(),
        )
        .unwrap();
        // 损坏记录与异 cid 记录跳过
        s.put(&format!("{}bad", presence_prefix_for(&cid)), "{broken").unwrap();
        s.put(
            &presence_key(&cid, "other"),
            &record(&"b".repeat(64), "other", &[true]).to_json(),
        )
        .unwrap();
        let records = list_presence(&s, &cid).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].device_uid, "dup");
    }
}
