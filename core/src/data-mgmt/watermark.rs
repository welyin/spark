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
        purged_before: existing
            .as_ref()
            .map_or(0, |r| r.purged_before)
            .max(purged_before),
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
