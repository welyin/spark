//! 配额、水位、K 语义与访问跟踪（A2，协议 §15.1–15.3）。
//!
//! - 配额：本地键 `blob:quota`（用户可配，字节），未配置按设备类默认
//!   （PC 10 GiB / 移动 1 GiB）；hello 经 `blobQuota` 字段声明；
//! - 水位：本机 `blob:chunk:` 全部值的原始字节数之和（base64 折算）；
//! - K 语义：`K = min(3, 未撤销设备数)`——设备 ≤3 台退化为全量；
//! - 访问：`blob:access:{cid}` 最近访问 ms（写/回补/读出即刷新），
//!   驱逐选择器「最久未访问优先」的输入（[`super::evict`]）。

use super::SyncResult;
use crate::storage::{ScanOptions, StorageBackend};

/// PC 默认配额：10 GiB。
pub const DEFAULT_QUOTA_PC_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// 移动设备默认配额：1 GiB。
pub const DEFAULT_QUOTA_MOBILE_BYTES: u64 = 1024 * 1024 * 1024;
/// 副本目标上限（设备数 ≥3 时 K=3）。
pub const K_REPLICAS: usize = 3;

/// 用户配额配置键（本地键，十进制 ASCII 字节数）。
pub const BLOB_QUOTA_KEY: &str = "blob:quota";
/// 对端 hello 声明配额的本地备查键前缀（`pdsync:blobquota:{peer}`，
/// `pdsync:devclass:` 先例）。
pub const REMOTE_BLOB_QUOTA_PREFIX: &str = "pdsync:blobquota:";

/// 访问键（`blob:access:{cid}`；本地键，十进制 ASCII ms）。
pub fn blob_access_key(cid: &str) -> String {
    format!("blob:access:{cid}")
}

/// 对端配额备查键。
pub fn remote_blob_quota_key(peer: &str) -> String {
    format!("{REMOTE_BLOB_QUOTA_PREFIX}{peer}")
}

// ── 配额 ───────────────────────────────────────────────────────────

/// 本机生效配额（字节）：用户配置优先，未配置按设备类默认。
pub fn get_blob_quota<S: StorageBackend>(storage: &S) -> SyncResult<u64> {
    if let Some(raw) = storage.get(BLOB_QUOTA_KEY)?
        && let Ok(v) = raw.parse::<u64>()
    {
        return Ok(v);
    }
    Ok(if crate::sync::pdsync::local_device_class() == "mobile" {
        DEFAULT_QUOTA_MOBILE_BYTES
    } else {
        DEFAULT_QUOTA_PC_BYTES
    })
}

/// 设置用户配额（字节）。`None` 清除配置回落设备类默认。
pub fn set_blob_quota<S: StorageBackend>(storage: &mut S, bytes: Option<u64>) -> SyncResult<()> {
    match bytes {
        Some(v) => storage.put(BLOB_QUOTA_KEY, &v.to_string())?,
        None => storage.delete(BLOB_QUOTA_KEY)?,
    }
    Ok(())
}

// ── K 语义 ─────────────────────────────────────────────────────────

/// 副本目标 K：`min(3, device_count)`，至少 1。
pub fn k_target(device_count: usize) -> usize {
    device_count.clamp(1, K_REPLICAS)
}

/// 域内未撤销设备数（设备清单 `revokedAt` 为空者，含本机；记录解析失败
/// 的不计——宁可 K 偏小保守驱逐受限，也不放大驱逐面）。
pub fn active_device_count<S: StorageBackend>(storage: &S) -> usize {
    crate::device::DeviceService::list(storage)
        .map(|records| {
            records
                .iter()
                .filter(|r| r.revoked_at.is_none())
                .count()
                .max(1)
        })
        .unwrap_or(1)
}

// ── 访问跟踪 ───────────────────────────────────────────────────────

/// 刷新访问时间（写/回补/读出即调用；LRU 龄期语义的数据源）。
pub fn touch_access<S: StorageBackend>(storage: &mut S, cid: &str, now_ms: i64) -> SyncResult<()> {
    storage.put(&blob_access_key(cid), &now_ms.to_string())?;
    Ok(())
}

/// 读访问时间（无记录 → None，选择器按 0=最老处理）。
pub fn get_access<S: StorageBackend>(storage: &S, cid: &str) -> SyncResult<Option<i64>> {
    Ok(storage
        .get(&blob_access_key(cid))?
        .and_then(|raw| raw.parse::<i64>().ok()))
}

// ── 水位 ───────────────────────────────────────────────────────────

/// base64 折算原始字节数（`len/4*3` 减 padding）。
fn b64_decoded_len(s: &str) -> u64 {
    let len = s.len() as u64;
    if len == 0 {
        return 0;
    }
    let padding = if s.ends_with("==") {
        2
    } else if s.ends_with('=') {
        1
    } else {
        0
    };
    len / 4 * 3 - padding
}

/// 配额水位：本机 `blob:chunk:` 全部值的原始字节数之和。
pub fn blob_usage<S: StorageBackend>(storage: &S) -> SyncResult<u64> {
    let mut used = 0u64;
    for (_key, value) in storage.scan(&ScanOptions::prefix("blob:chunk:"))? {
        used += b64_decoded_len(&value);
    }
    Ok(used)
}

/// 配额状态快照（健康度/驱逐判定的共同输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaStatus {
    /// 生效配额（字节）。
    pub quota_bytes: u64,
    /// 当前水位（字节）。
    pub used_bytes: u64,
    /// 超出量（未超为 0）。
    pub over_bytes: u64,
}

/// 计算配额状态。
pub fn quota_status<S: StorageBackend>(storage: &S) -> SyncResult<QuotaStatus> {
    let quota_bytes = get_blob_quota(storage)?;
    let used_bytes = blob_usage(storage)?;
    Ok(QuotaStatus {
        quota_bytes,
        used_bytes,
        over_bytes: used_bytes.saturating_sub(quota_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn k_target_semantics() {
        assert_eq!(k_target(0), 1, "无设备记录兜底 1");
        assert_eq!(k_target(1), 1, "单设备单副本");
        assert_eq!(k_target(2), 2);
        assert_eq!(k_target(3), 3, "≤3 台退化全量");
        assert_eq!(k_target(5), 3, "上限 3");
    }

    #[test]
    fn quota_config_and_defaults() {
        let mut s = MemoryStorage::new();
        // 未配置：按设备类默认（测试宿主非 mobile → PC 10GiB）
        assert_eq!(get_blob_quota(&s).unwrap(), DEFAULT_QUOTA_PC_BYTES);
        set_blob_quota(&mut s, Some(123_456)).unwrap();
        assert_eq!(get_blob_quota(&s).unwrap(), 123_456);
        set_blob_quota(&mut s, None).unwrap();
        assert_eq!(get_blob_quota(&s).unwrap(), DEFAULT_QUOTA_PC_BYTES);
    }

    #[test]
    fn usage_water_level_and_over() {
        let mut s = MemoryStorage::new();
        // 两个 blob：1000B 单块 + 空 blob（无 chunk 键）
        let d1 = pattern(1000);
        let m1 = super::super::save_blob(&mut s, "node-a", "uid-a", &d1, 1000).unwrap();
        super::super::save_blob(&mut s, "node-a", "uid-a", &[], 1001).unwrap();
        assert_eq!(blob_usage(&s).unwrap(), 1000);
        // 配额 999 → 超 1；配额 1000 → 恰好不超
        set_blob_quota(&mut s, Some(999)).unwrap();
        let st = quota_status(&s).unwrap();
        assert_eq!(st.over_bytes, 1);
        set_blob_quota(&mut s, Some(1000)).unwrap();
        assert_eq!(quota_status(&s).unwrap().over_bytes, 0);
        // 弃块后水位回落
        super::super::drop_local_chunks(&mut s, "node-a", "uid-a", &m1.cid, 2000).unwrap();
        assert_eq!(blob_usage(&s).unwrap(), 0);
    }

    #[test]
    fn access_touch_and_read() {
        let mut s = MemoryStorage::new();
        assert_eq!(get_access(&s, "cid-x").unwrap(), None);
        touch_access(&mut s, "cid-x", 12345).unwrap();
        assert_eq!(get_access(&s, "cid-x").unwrap(), Some(12345));
    }

    #[test]
    fn b64_len_conversion() {
        assert_eq!(b64_decoded_len(""), 0);
        assert_eq!(b64_decoded_len("AQ=="), 1);
        assert_eq!(b64_decoded_len("AQI="), 2);
        assert_eq!(b64_decoded_len("AQID"), 3);
        assert_eq!(b64_decoded_len("CQE="), 2);
    }
}
