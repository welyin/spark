//! 全库导出单测。

use spark_core::data_mgmt::exporter::{build_export_dump, write_export_dump};
use spark_core::storage::{MemoryStorage, StorageBackend};

const NOW: i64 = 1_800_000_000_000;

fn fixture() -> MemoryStorage {
    let mut s = MemoryStorage::new();
    // 含系统域（水位线）与非 ASCII key 的全库样本
    s.put("doc:plugin:app:c:id1", "{\"v\":1}").unwrap();
    s.put(
        "doc:system:purge-watermark:plugin%3Aapp%2Fc",
        "{\"purgedBefore\":100}",
    )
    .unwrap();
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
    assert_eq!(
        dump.entries[1].key,
        "doc:system:purge-watermark:plugin%3Aapp%2Fc"
    );
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
    let dir =
        std::env::temp_dir().join(format!("spark-core-export-test-{}", std::process::id()));
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
