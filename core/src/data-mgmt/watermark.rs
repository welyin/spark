//! purge 水位线（watermark.ts；core/spec/data-mgmt.md §5）。
//!
//! 记录"某集合在某时间点之前的数据已被本地清理"：被清理的文档经远端重推时，
//! 其 meta 仍携带原始写入时间戳（必然早于水位线），`apply_remote_update` 据此
//! 跳过落地（拦截点见 [`crate::sync::apply`]，经 [`StoragePurgeWatermark`] 注入）。
//! 水位线只升不降，永不被任何清理流程删除。
//!
//! 存储在系统域（复用 collectionSchemaKey 的 `encodeURIComponent` 技巧），
//! 插件经底层 db 接口无法篡改。

use serde::Serialize;
use serde_json::Value;

use crate::schema::encode_uri_component;
use crate::storage::StorageBackend;
use crate::sync::{SyncError, SyncResult};

use super::Result;

/// 水位线存储键前缀（watermark.ts:25）。
pub const PURGE_WATERMARK_KEY_PREFIX: &str = "doc:system:purge-watermark:";

/// purge 水位线记录（watermark.ts:14-23；JSON 字段序对齐 TS `JSON.stringify(record)`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PurgeWatermarkRecord {
    /// 数据域（读取时以入参为准，不信任存储值）。
    pub domain: String,
    /// 集合名（读取时以入参为准）。
    pub collection: String,
    /// 该时间戳之前（严格小于）的文档已被清理，远端重推一律拒绝。
    #[serde(rename = "purgedBefore")]
    pub purged_before: i64,
    /// 最近一次清理执行时间。
    #[serde(rename = "purgedAt")]
    pub purged_at: i64,
    /// 累计清理文档数。
    #[serde(rename = "removedDocs")]
    pub removed_docs: u64,
}

/// `purgeWatermarkKey`（watermark.ts:27-29）：
/// `doc:system:purge-watermark:{encodeURIComponent(domain + "/" + collection)}`。
pub fn purge_watermark_key(domain: &str, collection: &str) -> String {
    format!(
        "{PURGE_WATERMARK_KEY_PREFIX}{}",
        encode_uri_component(&format!("{domain}/{collection}"))
    )
}

/// JSON number → i64（对齐 TS `typeof x === 'number'`；浮点截断——
/// 本实现写入的均为整数 ms，浮点只会来自外部损坏数据）。
fn json_number_as_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}

/// `getPurgeWatermark`（watermark.ts:32-56）：读取集合的 purge 水位线。
///
/// key 不存在、JSON 损坏、或 `purgedBefore` 非 number → `None`；
/// `purgedAt`/`removedDocs` 缺失或非 number 时**默认 0** 容忍；
/// 返回记录的 domain/collection 以入参为准（不信任存储值）。
pub fn get_purge_watermark<S: StorageBackend + ?Sized>(
    storage: &S,
    domain: &str,
    collection: &str,
) -> Result<Option<PurgeWatermarkRecord>> {
    let Some(raw) = storage.get(&purge_watermark_key(domain, collection))? else {
        return Ok(None);
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Ok(None);
    };
    let Some(purged_before) = json_number_as_i64(parsed.get("purgedBefore")) else {
        return Ok(None);
    };
    Ok(Some(PurgeWatermarkRecord {
        domain: domain.to_string(),
        collection: collection.to_string(),
        purged_before,
        purged_at: json_number_as_i64(parsed.get("purgedAt")).unwrap_or(0),
        removed_docs: json_number_as_i64(parsed.get("removedDocs"))
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0),
    }))
}

/// `raisePurgeWatermark`（watermark.ts:62-79）：抬升集合的 purge 水位线（只升不降）。
///
/// `purgedBefore = max(existing ?? 0, 新值)`；`purgedAt = now`；`removedDocs` 累计。
/// 返回生效后的记录。
pub fn raise_purge_watermark<S: StorageBackend>(
    storage: &mut S,
    domain: &str,
    collection: &str,
    purged_before: i64,
    removed_docs: u64,
    now_ms: i64,
) -> Result<PurgeWatermarkRecord> {
    let existing = get_purge_watermark(storage, domain, collection)?;
    let next = PurgeWatermarkRecord {
        domain: domain.to_string(),
        collection: collection.to_string(),
        purged_before: existing.as_ref().map_or(0, |r| r.purged_before).max(purged_before),
        purged_at: now_ms,
        removed_docs: existing.as_ref().map_or(0, |r| r.removed_docs) + removed_docs,
    };
    storage.put(
        &purge_watermark_key(domain, collection),
        &serde_json::to_string(&next)?,
    )?;
    Ok(next)
}

/// `isPurgedByWatermark`（watermark.ts:85-96）：远端同步时间戳是否落在已清理区间。
///
/// 坑 #7 如实复刻：`remoteTs <= 0` **放行**（TS 还有"非 number 放行"，Rust 入参
/// 类型 `i64` 已在类型层面排除）——是否拦截落到后续 LWW/append-only 逻辑判定。
/// 否则 `remoteTs < purgedBefore` 拦截（**严格 `<`**；`ts == purgedBefore` 不拦截，
/// 与 purge 选中条件 `ts < beforeTs` 边界一致，无需特判）。
pub fn is_purged_by_watermark<S: StorageBackend + ?Sized>(
    storage: &S,
    domain: &str,
    collection: &str,
    remote_ts: i64,
) -> Result<bool> {
    if remote_ts <= 0 {
        return Ok(false);
    }
    let watermark = get_purge_watermark(storage, domain, collection)?;
    Ok(watermark.is_some_and(|w| remote_ts < w.purged_before))
}

/// sync 模块 [`crate::sync::PurgeWatermark`] 注入点的实现：
/// 以存储中的水位线记录判定远端更新是否应被拒绝落地。
#[derive(Clone, Copy, Debug, Default)]
pub struct StoragePurgeWatermark;

impl crate::sync::PurgeWatermark for StoragePurgeWatermark {
    fn is_purged_by_watermark(
        &self,
        storage: &mut dyn StorageBackend,
        domain: &str,
        collection: &str,
        remote_ts: i64,
    ) -> SyncResult<bool> {
        is_purged_by_watermark(&*storage, domain, collection, remote_ts)
            .map_err(|e| SyncError::Watermark(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn watermark_key_matches_ts_encoding() {
        // encodeURIComponent("plugin:app/col")：':' → %3A，'/' → %2F
        assert_eq!(
            purge_watermark_key("plugin:app", "col"),
            "doc:system:purge-watermark:plugin%3Aapp%2Fcol"
        );
        assert_eq!(
            purge_watermark_key("plugin:中", "c-1_x"),
            "doc:system:purge-watermark:plugin%3A%E4%B8%AD%2Fc-1_x"
        );
    }

    #[test]
    fn get_watermark_missing_and_corrupted() {
        let mut s = MemoryStorage::new();
        assert_eq!(get_purge_watermark(&s, "plugin:a", "c").unwrap(), None);

        let key = purge_watermark_key("plugin:a", "c");
        // 非 JSON
        s.put(&key, "not json").unwrap();
        assert_eq!(get_purge_watermark(&s, "plugin:a", "c").unwrap(), None);
        // JSON 非对象
        s.put(&key, "5").unwrap();
        assert_eq!(get_purge_watermark(&s, "plugin:a", "c").unwrap(), None);
        // purgedBefore 非 number
        s.put(&key, "{\"purgedBefore\":\"x\"}").unwrap();
        assert_eq!(get_purge_watermark(&s, "plugin:a", "c").unwrap(), None);
        // purgedBefore 缺失
        s.put(&key, "{\"purgedAt\":1}").unwrap();
        assert_eq!(get_purge_watermark(&s, "plugin:a", "c").unwrap(), None);
    }

    #[test]
    fn get_watermark_tolerates_missing_fields_and_distrusts_stored_identity() {
        let mut s = MemoryStorage::new();
        let key = purge_watermark_key("plugin:a", "c");
        // purgedAt / removedDocs 缺失 → 默认 0；存储的 domain/collection 不可信，以入参为准
        s.put(
            &key,
            "{\"domain\":\"evil\",\"collection\":\"evil\",\"purgedBefore\":100}",
        )
        .unwrap();
        let record = get_purge_watermark(&s, "plugin:a", "c").unwrap().unwrap();
        assert_eq!(
            record,
            PurgeWatermarkRecord {
                domain: "plugin:a".to_string(),
                collection: "c".to_string(),
                purged_before: 100,
                purged_at: 0,
                removed_docs: 0,
            }
        );

        // purgedAt / removedDocs 非 number → 容忍为 0
        s.put(&key, "{\"purgedBefore\":100,\"purgedAt\":\"x\",\"removedDocs\":null}")
            .unwrap();
        let record = get_purge_watermark(&s, "plugin:a", "c").unwrap().unwrap();
        assert_eq!(record.purged_at, 0);
        assert_eq!(record.removed_docs, 0);
    }

    #[test]
    fn raise_watermark_only_goes_up_and_accumulates() {
        let mut s = MemoryStorage::new();
        // 首次抬升
        let r = raise_purge_watermark(&mut s, "plugin:a", "c", 100, 3, NOW).unwrap();
        assert_eq!((r.purged_before, r.purged_at, r.removed_docs), (100, NOW, 3));

        // 更低值不降低（只升不降），removedDocs 累计
        let r = raise_purge_watermark(&mut s, "plugin:a", "c", 50, 2, NOW + 1).unwrap();
        assert_eq!((r.purged_before, r.purged_at, r.removed_docs), (100, NOW + 1, 5));

        // 更高值抬升
        let r = raise_purge_watermark(&mut s, "plugin:a", "c", 200, 1, NOW + 2).unwrap();
        assert_eq!((r.purged_before, r.removed_docs), (200, 6));

        // 持久化 JSON 字段名与顺序逐字节对齐 TS
        let raw = s.get(&purge_watermark_key("plugin:a", "c")).unwrap().unwrap();
        assert_eq!(
            raw,
            "{\"domain\":\"plugin:a\",\"collection\":\"c\",\"purgedBefore\":200,\"purgedAt\":1800000000002,\"removedDocs\":6}"
        );

        // 损坏的既有记录按 0 起算
        s.put(&purge_watermark_key("plugin:a", "broken"), "garbage").unwrap();
        let r = raise_purge_watermark(&mut s, "plugin:a", "broken", 10, 1, NOW).unwrap();
        assert_eq!((r.purged_before, r.removed_docs), (10, 1));
    }

    #[test]
    fn is_purged_strict_boundary_and_pit7_passthrough() {
        let mut s = MemoryStorage::new();
        raise_purge_watermark(&mut s, "plugin:a", "c", 100, 1, NOW).unwrap();

        // remoteTs <= 0 一律放行（坑 #7）
        assert!(!is_purged_by_watermark(&s, "plugin:a", "c", 0).unwrap());
        assert!(!is_purged_by_watermark(&s, "plugin:a", "c", -5).unwrap());
        // 严格 <：99 拦截，100（== purgedBefore）不拦截，101 不拦截
        assert!(is_purged_by_watermark(&s, "plugin:a", "c", 99).unwrap());
        assert!(!is_purged_by_watermark(&s, "plugin:a", "c", 100).unwrap());
        assert!(!is_purged_by_watermark(&s, "plugin:a", "c", 101).unwrap());
        // 无水位线记录的集合不拦截
        assert!(!is_purged_by_watermark(&s, "plugin:a", "other", 1).unwrap());
    }

    #[test]
    fn sync_trait_injection_point() {
        let mut s = MemoryStorage::new();
        raise_purge_watermark(&mut s, "plugin:a", "c", 100, 1, NOW).unwrap();
        let gate = StoragePurgeWatermark;
        // 经 trait 对象调用（apply_remote_update 的注入形态）
        let dyn_gate: &dyn crate::sync::PurgeWatermark = &gate;
        assert!(
            dyn_gate
                .is_purged_by_watermark(&mut s, "plugin:a", "c", 50)
                .unwrap()
        );
        assert!(
            !dyn_gate
                .is_purged_by_watermark(&mut s, "plugin:a", "c", 100)
                .unwrap()
        );
        assert!(
            !dyn_gate
                .is_purged_by_watermark(&mut s, "plugin:a", "c", 0)
                .unwrap()
        );
    }
}
