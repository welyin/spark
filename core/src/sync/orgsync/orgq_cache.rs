//! orgq（O3）成员侧查询结果缓存命名空间：`orgq:cache:{orgId}:{collection}:`。
//!
//! 独立命名空间（不算副本、可淘汰、不计副本指标）；成员移除时按远程指令
//! 尽力擦除（见 `orgq_queue` 的 `orgq_wipe_org_local`）。纯逻辑（存储泛型）。
//!
//! 从 `orgq.rs` 拆出的子模块（Z5 650 行硬线），共享父模块的
//! [`OrgqRespRecord`] 类型。

use crate::storage::{ScanOptions, StorageBackend};

/// 成员侧 orgq 查询结果缓存键前缀：`orgq:cache:{orgId}:{collection}:`。
/// 独立命名空间（不算副本、可淘汰、不计副本指标；成员移除时按远程指令尽力擦除）。
pub fn orgq_cache_prefix(org_id: &str, collection: &str) -> String {
    format!("orgq:cache:{org_id}:{collection}:")
}

/// 成员侧 orgq 查询结果缓存单条键：`orgq:cache:{orgId}:{collection}:{key}`。
pub fn orgq_cache_key(org_id: &str, collection: &str, key: &str) -> String {
    format!("{}{}", orgq_cache_prefix(org_id, collection), key)
}

/// 成员侧缓存命名空间是否有该集合的数据（`has_cache` 判定辅助）。
pub fn orgq_cache_has_data<S: StorageBackend>(storage: &S, org_id: &str, collection: &str) -> bool {
    let prefix = orgq_cache_prefix(org_id, collection);
    !storage
        .scan(&ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .is_empty()
}

/// 单集合缓存条数上限（最简「条数淘汰」：超出即按 meta.ts 淘汰最旧）。
/// 规格只要求「可淘汰」，选条数上限 + 最旧淘汰为最简可验证实现（注释）。
pub const ORGQ_CACHE_MAX_KEYS_PER_COLLECTION: usize = 5000;

/// 淘汰某集合成员侧缓存中超过条数上限的最旧条目（按缓存值内 `meta.ts`）。
/// 缓存值即 `OrgqRespRecord` JSON（含 meta.ts），读取解析 ts、按旧优先删除。
/// 返回删除条数；无超限时返回 0。`best-effort`：解析失败条目视为最旧优先删。
pub fn orgq_cache_evict<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
) -> usize {
    let prefix = orgq_cache_prefix(org_id, collection);
    let mut entries: Vec<(String, i64)> = storage
        .scan(&ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .map(|(key, raw)| {
            let ts = serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&raw)
                .ok()
                .map(|r| r.meta.ts)
                .unwrap_or(i64::MIN);
            (key, ts)
        })
        .collect();
    if entries.len() <= ORGQ_CACHE_MAX_KEYS_PER_COLLECTION {
        return 0;
    }
    // 按 ts 升序（最旧在前）排序后，删超出的最旧条目
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let over = entries.len() - ORGQ_CACHE_MAX_KEYS_PER_COLLECTION;
    let mut removed = 0;
    for (key, _) in entries.into_iter().take(over) {
        let _ = storage.delete(&key);
        removed += 1;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orgq_cache_key_namespace() {
        assert_eq!(
            orgq_cache_prefix("org_01", "finance:ledger"),
            "orgq:cache:org_01:finance:ledger:"
        );
        assert_eq!(
            orgq_cache_key("org_01", "finance:ledger", "2026-08"),
            "orgq:cache:org_01:finance:ledger:2026-08"
        );
    }

    #[test]
    fn orgq_cache_has_data_detects_population() {
        let mut s = crate::storage::MemoryStorage::new();
        assert!(!orgq_cache_has_data(&s, "org_01", "c"));
        s.put(&orgq_cache_key("org_01", "c", "k1"), "{}").unwrap();
        assert!(orgq_cache_has_data(&s, "org_01", "c"));
    }
}
