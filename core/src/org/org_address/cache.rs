//! 组织地址记录本地缓存（sled `p2p:org-address:` 前缀，尊重 ttl）。

use super::super::Result;
use super::record::{OrgAddressRecord, is_newer_org_address_record};
use crate::storage::{ScanOptions, StorageBackend};

/// 本地缓存 sled 键前缀（p2p-messages.md §16）。
pub const ORG_ADDRESS_CACHE_PREFIX: &str = "p2p:org-address:";

/// 缓存键：`p2p:org-address:<orgAddress>`。
pub fn org_address_cache_key(org_address: &str) -> String {
    format!("{ORG_ADDRESS_CACHE_PREFIX}{org_address}")
}

/// 记录是否已过 ttl 有效期（边界同校验链：`now > publishedAt + ttl` 过期）。
pub fn org_address_record_expired(record: &OrgAddressRecord, now_ms: i64) -> bool {
    match record.published_at.checked_add(record.ttl) {
        Some(expiry) => now_ms > expiry,
        None => true,
    }
}

/// 读取缓存记录（不做过期判定；解析失败/缺失返回 `None`）。
pub fn read_cached_org_address_record<S: StorageBackend>(
    storage: &S,
    org_address: &str,
) -> Option<OrgAddressRecord> {
    let raw = storage.get(&org_address_cache_key(org_address)).ok()??;
    serde_json::from_str(&raw).ok()
}

/// 写入缓存：带 seq/publishedAt 冲突裁决——已有较新记录时不动（返回 `Ok(false)`）。
///
/// 调用方负责先过五步校验链；本函数只做裁决与落盘。
pub fn cache_org_address_record<S: StorageBackend>(
    storage: &mut S,
    record: &OrgAddressRecord,
) -> Result<bool> {
    if let Some(existing) = read_cached_org_address_record(storage, &record.org_address)
        && !is_newer_org_address_record(record, &existing)
    {
        return Ok(false);
    }
    storage.put(
        &org_address_cache_key(&record.org_address),
        &serde_json::to_string(record)?,
    )?;
    Ok(true)
}

/// 本地搜索（org.md §16.4）：缓存中未过期记录按 `displayName`/orgAddress 子串
/// 匹配（大小写不敏感；空关键字列出全部），按 `publishedAt` 降序。纯本地查询。
pub fn search_cached_org_address_records<S: StorageBackend>(
    storage: &S,
    keyword: &str,
    now_ms: i64,
) -> Vec<OrgAddressRecord> {
    let needle = keyword.trim().to_lowercase();
    let rows = storage
        .scan(&ScanOptions::prefix(ORG_ADDRESS_CACHE_PREFIX))
        .unwrap_or_default();
    let mut out: Vec<OrgAddressRecord> = rows
        .into_iter()
        .filter_map(|(_, value)| serde_json::from_str::<OrgAddressRecord>(&value).ok())
        .filter(|record| !org_address_record_expired(record, now_ms))
        .filter(|record| {
            needle.is_empty()
                || record
                    .display_name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&needle)
                || record.org_address.contains(&needle)
        })
        .collect();
    out.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    out
}
