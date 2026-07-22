//! 存储用量统计（usage.ts；core/spec/data-mgmt.md §2）。
//!
//! 全库单遍扫描按键前缀分类聚合；只测量与提示，不拒绝任何写入（软配额）。
//! 每行字节数 = key + value 的 UTF-8 字节长度（Rust `str::len` 即 UTF-8 字节数，
//! 等价 TS `Buffer.byteLength(..., 'utf8')`）。

use serde::Serialize;

use crate::storage::{KEY_RANGE_UPPER_BOUND, ScanOptions, StorageBackend};

use super::constants::{DISK_FREE_WARN_RATIO, USAGE_WARN_TOTAL_BYTES};
use super::Result;

/// 用量分类（usage.ts:12-20）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageClass {
    /// 业务文档 `doc:plugin:*` / `doc:<domain>:*`。
    Documents,
    /// 二级索引 `idx:*`。
    Indexes,
    /// 同步元数据 `meta:*`（含 tombstone）。
    SyncMeta,
    /// 存证链 `doc:evidence:*`。
    Evidence,
    /// 组织 `org:meta:*` / `org:tx:*`。
    Organization,
    /// p2p 网络状态 `p2p:*`（含 peer 记录与 org-sync-state）。
    P2p,
    /// 系统域 `doc:system:*`（策略注册表、purge 水位线、审计日志、配置）。
    System,
    /// 其余。
    Other,
}

/// 单分类统计（usage.ts:22-25）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageClassStat {
    /// 键数量。
    pub keys: u64,
    /// 字节数（key + value 的 UTF-8 长度合计）。
    pub bytes: u64,
}

/// 八类用量表（usage.ts:60-71；JSON 字段序对齐 TS `emptyClasses`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageClasses {
    /// 业务文档。
    pub documents: UsageClassStat,
    /// 二级索引。
    pub indexes: UsageClassStat,
    /// 同步元数据。
    #[serde(rename = "syncMeta")]
    pub sync_meta: UsageClassStat,
    /// 存证链。
    pub evidence: UsageClassStat,
    /// 组织。
    pub organization: UsageClassStat,
    /// p2p 网络状态。
    pub p2p: UsageClassStat,
    /// 系统域。
    pub system: UsageClassStat,
    /// 其余。
    pub other: UsageClassStat,
}

impl UsageClasses {
    fn stat_mut(&mut self, class: UsageClass) -> &mut UsageClassStat {
        match class {
            UsageClass::Documents => &mut self.documents,
            UsageClass::Indexes => &mut self.indexes,
            UsageClass::SyncMeta => &mut self.sync_meta,
            UsageClass::Evidence => &mut self.evidence,
            UsageClass::Organization => &mut self.organization,
            UsageClass::P2p => &mut self.p2p,
            UsageClass::System => &mut self.system,
            UsageClass::Other => &mut self.other,
        }
    }
}

/// 磁盘可用信息（usage.ts:27-32）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiskInfo {
    /// 测量的数据目录路径。
    pub path: String,
    /// 可用字节数（`bavail * bsize`）。
    #[serde(rename = "freeBytes")]
    pub free_bytes: u64,
    /// 总字节数（`blocks * bsize`）。
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    /// 可用比例。
    #[serde(rename = "freeRatio")]
    pub free_ratio: f64,
}

/// 警告判定（usage.ts:40-45）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageWarnings {
    /// 数据总量超过软配额阈值（严格 `>`）。
    #[serde(rename = "usageExceeded")]
    pub usage_exceeded: bool,
    /// 磁盘可用比例低于阈值（严格 `<`；`disk` 为 `None` 时恒 false）。
    #[serde(rename = "diskLow")]
    pub disk_low: bool,
}

/// 用量报告（usage.ts:34-46）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DataUsageReport {
    /// 扫描时间（ms）。
    #[serde(rename = "scannedAt")]
    pub scanned_at: i64,
    /// 八类分类统计。
    pub classes: UsageClasses,
    /// 总键数。
    #[serde(rename = "totalKeys")]
    pub total_keys: u64,
    /// 总字节数。
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    /// 磁盘信息（测量失败为 `None`，不影响用量统计）。
    pub disk: Option<DiskInfo>,
    /// 警告。
    pub warnings: UsageWarnings,
}

/// `classifyKey`：按存储键前缀归类（usage.ts:49-58）。
///
/// **顺序敏感**：更具体的前缀先判（`doc:evidence:` / `doc:system:` 先于 `doc:`）。
pub fn classify_key(key: &str) -> UsageClass {
    if key.starts_with("doc:evidence:") {
        return UsageClass::Evidence;
    }
    if key.starts_with("doc:system:") {
        return UsageClass::System;
    }
    if key.starts_with("doc:") {
        return UsageClass::Documents;
    }
    if key.starts_with("idx:") {
        return UsageClass::Indexes;
    }
    if key.starts_with("meta:") {
        return UsageClass::SyncMeta;
    }
    if key.starts_with("org:") {
        return UsageClass::Organization;
    }
    if key.starts_with("p2p:") {
        return UsageClass::P2p;
    }
    UsageClass::Other
}

/// `measureDiskInfo`（usage.ts:74-86）：读取数据目录所在磁盘的可用空间。
///
/// - `freeBytes = bavail * bsize`（**bavail**，非 bfree），`totalBytes = blocks * bsize`；
/// - 任一乘积超出可表示范围（对应 TS 的非有限数）或 `totalBytes <= 0` → `None`；
/// - `statfs` 失败静默返回 `None`（不影响用量统计）。
pub fn measure_disk_info(path: &str) -> Option<DiskInfo> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(std::path::Path::new(path).as_os_str().as_bytes()).ok()?;
    // SAFETY: `stat` 为零初始化结构体，`c_path` 以 NUL 结尾；返回值检查后字段才可读。
    let stat = unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        if libc::statfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    let free_bytes = u64::try_from((stat.f_bavail as u128).checked_mul(stat.f_bsize as u128)?).ok()?;
    let total_bytes = u64::try_from((stat.f_blocks as u128).checked_mul(stat.f_bsize as u128)?).ok()?;
    if total_bytes == 0 {
        return None;
    }
    Some(DiskInfo {
        path: path.to_string(),
        free_bytes,
        total_bytes,
        free_ratio: free_bytes as f64 / total_bytes as f64,
    })
}

/// 警告判定（usage.ts:116-119）：`usageExceeded` 严格 `>`，`diskLow` 严格 `<`。
fn build_warnings(total_bytes: u64, disk: Option<&DiskInfo>) -> UsageWarnings {
    UsageWarnings {
        usage_exceeded: total_bytes > USAGE_WARN_TOTAL_BYTES,
        disk_low: disk.is_some_and(|d| d.free_ratio < DISK_FREE_WARN_RATIO),
    }
}

/// `collectDataUsage`（usage.ts:92-121）：全库单遍扫描并分类聚合用量。
///
/// 扫描 `prefix: ''` + 排他上界 `U+10FFFF`（坑 #2：恰好等于/以 `U+10FFFF` 开头的
/// 极端 key 不被统计，实际不产生此类 key）。`disk_path` 提供时附带磁盘信息
/// （statfs 失败静默为 `None`）。
pub fn collect_data_usage<S: StorageBackend + ?Sized>(
    storage: &S,
    disk_path: Option<&str>,
    now_ms: i64,
) -> Result<DataUsageReport> {
    let mut classes = UsageClasses::default();
    let mut total_keys = 0u64;
    let mut total_bytes = 0u64;

    // end 取最大合法 UTF-8 码位，保证非 ASCII 键也被遍历到
    let rows = storage.scan(&ScanOptions {
        prefix: String::new(),
        start: None,
        end: Some(KEY_RANGE_UPPER_BOUND.to_string()),
        limit: None,
        reverse: false,
    })?;
    for (key, value) in rows {
        let bytes = (key.len() + value.len()) as u64;
        let stat = classes.stat_mut(classify_key(&key));
        stat.keys += 1;
        stat.bytes += bytes;
        total_keys += 1;
        total_bytes += bytes;
    }

    let disk = disk_path.and_then(measure_disk_info);
    let warnings = build_warnings(total_bytes, disk.as_ref());

    Ok(DataUsageReport {
        scanned_at: now_ms,
        classes,
        total_keys,
        total_bytes,
        disk,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn classify_key_all_classes_order_sensitive() {
        assert_eq!(classify_key("doc:evidence:proof:000000000001"), UsageClass::Evidence);
        assert_eq!(classify_key("doc:evidence:head"), UsageClass::Evidence);
        assert_eq!(classify_key("doc:system:purge-watermark:x"), UsageClass::System);
        assert_eq!(classify_key("doc:system:collection-schema:a%2Fb"), UsageClass::System);
        assert_eq!(classify_key("doc:system:purge-log:123"), UsageClass::System);
        assert_eq!(classify_key("doc:plugin:app:col:id"), UsageClass::Documents);
        assert_eq!(classify_key("doc:"), UsageClass::Documents);
        // 顺序敏感：doc:evidence: / doc:system: 先于 doc:
        assert_eq!(classify_key("doc:evidence"), UsageClass::Documents); // 无尾冒号
        assert_eq!(classify_key("doc:system"), UsageClass::Documents);
        assert_eq!(classify_key("idx:plugin:app:col:byX:v:id"), UsageClass::Indexes);
        assert_eq!(classify_key("meta:plugin:app:col:id"), UsageClass::SyncMeta);
        assert_eq!(classify_key("org:meta:org1"), UsageClass::Organization);
        assert_eq!(classify_key("org:tx:org1:1"), UsageClass::Organization);
        assert_eq!(classify_key("p2p:peer:record:p1"), UsageClass::P2p);
        assert_eq!(classify_key("p2p:org-sync-state:p1:o1"), UsageClass::P2p);
        assert_eq!(classify_key("hello"), UsageClass::Other);
        assert_eq!(classify_key(""), UsageClass::Other);
        // 长度足够但不带冒号边界的前缀不归类
        assert_eq!(classify_key("documentation:x"), UsageClass::Other);
        assert_eq!(classify_key("metadata:x"), UsageClass::Other);
    }

    #[test]
    fn collect_usage_classifies_and_counts_utf8_bytes() {
        let mut s = MemoryStorage::new();
        // evidence：key 23B + value 2B
        s.put("doc:evidence:proof:0001", "{}").unwrap();
        // system：key 19B + value 3B
        s.put("doc:system:config:a", "123").unwrap();
        // documents：中文 key/value 按 UTF-8 字节计（"值" = 3B）
        s.put("doc:plugin:app:c:中文id", "值").unwrap();
        s.put("doc:plugin:app:c:id2", "v2").unwrap();
        // indexes / syncMeta / organization / p2p / other
        s.put("idx:plugin:app:c:byX:v:id2", "").unwrap();
        s.put("meta:plugin:app:c:id2", "{\"ts\":1}").unwrap();
        s.put("org:meta:o1", "{}").unwrap();
        s.put("p2p:peer:record:p1", "{\"lastSeenAt\":1}").unwrap();
        s.put("other-key", "x").unwrap();

        let report = collect_data_usage(&s, None, 1000).unwrap();
        assert_eq!(report.scanned_at, 1000);
        assert_eq!(report.total_keys, 9);
        // documents：("doc:plugin:app:c:中文id" 26B + 3B) + ("doc:plugin:app:c:id2" 20B + 2B)
        let doc_bytes = ("doc:plugin:app:c:中文id".len() + "值".len()
            + "doc:plugin:app:c:id2".len()
            + "v2".len()) as u64;
        assert_eq!(report.classes.documents, UsageClassStat { keys: 2, bytes: doc_bytes });
        assert_eq!(report.classes.evidence, UsageClassStat { keys: 1, bytes: 25 });
        assert_eq!(report.classes.system, UsageClassStat { keys: 1, bytes: 22 });
        assert_eq!(report.classes.indexes.keys, 1);
        assert_eq!(report.classes.sync_meta.keys, 1);
        assert_eq!(report.classes.organization, UsageClassStat { keys: 1, bytes: 13 });
        assert_eq!(report.classes.other, UsageClassStat { keys: 1, bytes: 10 });
        let sum: u64 = [
            report.classes.documents.bytes,
            report.classes.indexes.bytes,
            report.classes.sync_meta.bytes,
            report.classes.evidence.bytes,
            report.classes.organization.bytes,
            report.classes.p2p.bytes,
            report.classes.system.bytes,
            report.classes.other.bytes,
        ]
        .iter()
        .sum();
        assert_eq!(report.total_bytes, sum);
        assert!(report.disk.is_none());
        assert!(!report.warnings.usage_exceeded);
        assert!(!report.warnings.disk_low);
    }

    #[test]
    fn collect_usage_scans_non_ascii_keys() {
        // U+10FFFF 上界覆盖首字节 > 0xC3 的 key（TS `\xFF` 上界会漏扫）
        let mut s = MemoryStorage::new();
        s.put("doc:plugin:a:c:😀", "1").unwrap();
        s.put("meta:中文:中文:中文", "2").unwrap();
        let report = collect_data_usage(&s, None, 0).unwrap();
        assert_eq!(report.total_keys, 2);
    }

    #[test]
    fn warnings_strict_boundaries() {
        // usageExceeded 严格 >：恰好 1 GiB 不告警
        assert!(!build_warnings(USAGE_WARN_TOTAL_BYTES, None).usage_exceeded);
        assert!(build_warnings(USAGE_WARN_TOTAL_BYTES + 1, None).usage_exceeded);

        let disk_at_threshold = DiskInfo {
            path: "/".to_string(),
            free_bytes: 150,
            total_bytes: 1000,
            free_ratio: DISK_FREE_WARN_RATIO,
        };
        // diskLow 严格 <：恰好 0.15 不告警
        assert!(!build_warnings(0, Some(&disk_at_threshold)).disk_low);
        let disk_below = DiskInfo { free_ratio: 0.149, ..disk_at_threshold.clone() };
        assert!(build_warnings(0, Some(&disk_below)).disk_low);
        // disk 为 None：diskLow 恒 false
        assert!(!build_warnings(u64::MAX, None).disk_low);
    }

    #[test]
    fn measure_disk_info_real_and_missing_paths() {
        let info = measure_disk_info("/").expect("rootfs statfs should succeed");
        assert_eq!(info.path, "/");
        assert!(info.total_bytes > 0);
        assert!(info.free_bytes <= info.total_bytes);
        assert!(info.free_ratio > 0.0 && info.free_ratio <= 1.0);

        assert_eq!(measure_disk_info("/definitely/not/existing-path-xyz"), None);
    }

    #[test]
    fn report_json_shape_matches_ts() {
        let mut s = MemoryStorage::new();
        s.put("a", "b").unwrap();
        let report = collect_data_usage(&s, None, 7).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        // TS 字段名逐字对齐
        assert_eq!(json["scannedAt"], 7);
        assert_eq!(json["totalKeys"], 1);
        assert_eq!(json["totalBytes"], 2);
        assert!(json["disk"].is_null());
        assert_eq!(json["warnings"]["usageExceeded"], false);
        assert_eq!(json["warnings"]["diskLow"], false);
        for name in ["documents", "indexes", "syncMeta", "evidence", "organization", "p2p", "system", "other"] {
            assert!(json["classes"][name].is_object(), "missing class {name}");
        }
    }
}
