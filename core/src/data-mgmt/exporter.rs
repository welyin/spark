//! 手动导出迁移（exporter.ts；core/spec/data-mgmt.md §7）。
//!
//! 全库逻辑 dump 为单个 JSON 文件（非 LevelDB 目录拷贝）：应用层一致、跨机器可读。
//! 容灾由 K 副本网络承担，本功能不做周期自动备份。**不含 RootID 身份**
//! （identities/ 目录单独存放且密码加密，身份备份走助记词）。
//!
//! 扫描 `prefix: ''` + 排他上界 `U+10FFFF`，含系统域（水位线、审计日志、策略
//! 注册表）在内的全部 key（坑 #2：恰好等于/以 `U+10FFFF` 开头的极端 key 不导出，
//! 实际不产生此类 key）。

use serde::Serialize;

use crate::storage::{KEY_RANGE_UPPER_BOUND, ScanOptions, StorageBackend};

use super::Result;

/// dump 格式版本（exporter.ts:18）。
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// dump 应用标识（exporter.ts:19）。
pub const EXPORT_APP: &str = "spark-desktop";

/// 单条导出条目（原始字符串键值）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportEntry {
    /// 原始 key。
    pub key: String,
    /// 原始 value。
    pub value: String,
}

/// `ExportDump`（exporter.ts:17-22；JSON 字段序对齐 TS 对象字面量）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportDump {
    /// 格式版本，恒为 1。
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    /// 应用标识，恒为 `spark-desktop`。
    pub app: String,
    /// 导出时间（ms）。
    #[serde(rename = "exportedAt")]
    pub exported_at: i64,
    /// 全库键值条目（键升序）。
    pub entries: Vec<ExportEntry>,
}

/// `writeExportDump` 的返回统计（exporter.ts:37）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportWriteResult {
    /// 写入路径。
    pub path: String,
    /// 条目数。
    pub entries: u64,
    /// JSON 文本的 UTF-8 字节数。
    pub bytes: u64,
}

/// `buildExportDump`（exporter.ts:25-34）：全库扫描生成导出对象（内存驻留）。
pub fn build_export_dump<S: StorageBackend + ?Sized>(
    storage: &S,
    now_ms: i64,
) -> Result<ExportDump> {
    // end 取最大合法 UTF-8 码位，保证非 ASCII 键也被导出
    let rows = storage.scan(&ScanOptions {
        prefix: String::new(),
        start: None,
        end: Some(KEY_RANGE_UPPER_BOUND.to_string()),
        limit: None,
        reverse: false,
    })?;
    Ok(ExportDump {
        format_version: EXPORT_FORMAT_VERSION,
        app: EXPORT_APP.to_string(),
        exported_at: now_ms,
        entries: rows.into_iter().map(|(key, value)| ExportEntry { key, value }).collect(),
    })
}

/// `writeExportDump`（exporter.ts:37-42）：导出到指定文件路径，返回写入统计。
///
/// `JSON.stringify(dump)`（无缩进）以 utf8 写文件；`bytes` 为 JSON 文本的
/// UTF-8 字节数。
pub fn write_export_dump<S: StorageBackend + ?Sized>(
    storage: &S,
    file_path: impl AsRef<std::path::Path>,
    now_ms: i64,
) -> Result<ExportWriteResult> {
    let dump = build_export_dump(storage, now_ms)?;
    let text = serde_json::to_string(&dump)?;
    std::fs::write(file_path.as_ref(), &text)?;
    Ok(ExportWriteResult {
        path: file_path.as_ref().to_string_lossy().into_owned(),
        entries: dump.entries.len() as u64,
        bytes: text.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_800_000_000_000;

    fn fixture() -> MemoryStorage {
        let mut s = MemoryStorage::new();
        // 含系统域（水位线）与非 ASCII key 的全库样本
        s.put("doc:plugin:app:c:id1", "{\"v\":1}").unwrap();
        s.put("doc:system:purge-watermark:plugin%3Aapp%2Fc", "{\"purgedBefore\":100}").unwrap();
        s.put("meta:plugin:app:c:中文", "{\"ts\":1}").unwrap();
        s
    }

    #[test]
    fn dump_structure_byte_aligned_with_ts() {
        let s = fixture();
        let dump = build_export_dump(&s, NOW).unwrap();
        assert_eq!(dump.format_version, 1);
        assert_eq!(dump.app, "spark-desktop");
        assert_eq!(dump.exported_at, NOW);
        assert_eq!(dump.entries.len(), 3);
        // 键升序（UTF-8 字节序）：doc:plugin:... < doc:system:... < meta:...
        assert_eq!(dump.entries[0].key, "doc:plugin:app:c:id1");
        assert_eq!(dump.entries[1].key, "doc:system:purge-watermark:plugin%3Aapp%2Fc");
        assert_eq!(dump.entries[2].key, "meta:plugin:app:c:中文");

        // 无缩进 JSON 逐字节对齐 TS JSON.stringify：非 ASCII 原样输出不转义
        let text = serde_json::to_string(&dump).unwrap();
        let expected = concat!(
            "{\"formatVersion\":1,\"app\":\"spark-desktop\",\"exportedAt\":1800000000000,\"entries\":[",
            "{\"key\":\"doc:plugin:app:c:id1\",\"value\":\"{\\\"v\\\":1}\"},",
            "{\"key\":\"doc:system:purge-watermark:plugin%3Aapp%2Fc\",\"value\":\"{\\\"purgedBefore\\\":100}\"},",
            "{\"key\":\"meta:plugin:app:c:中文\",\"value\":\"{\\\"ts\\\":1}\"}",
            "]}"
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn empty_db_exports_empty_entries() {
        let s = MemoryStorage::new();
        let text = serde_json::to_string(&build_export_dump(&s, 7).unwrap()).unwrap();
        assert_eq!(
            text,
            "{\"formatVersion\":1,\"app\":\"spark-desktop\",\"exportedAt\":7,\"entries\":[]}"
        );
    }

    #[test]
    fn write_dump_to_file_and_stats() {
        let s = fixture();
        let dir = std::env::temp_dir().join(format!("spark-core-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dump.json");

        let result = write_export_dump(&s, &path, NOW).unwrap();
        assert_eq!(result.entries, 3);
        assert_eq!(result.path, path.to_string_lossy());

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result.bytes, text.len() as u64);
        assert!(text.starts_with("{\"formatVersion\":1,"));
        // 写出的内容与 build + 序列化一致
        let dump = build_export_dump(&s, NOW).unwrap();
        assert_eq!(text, serde_json::to_string(&dump).unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
