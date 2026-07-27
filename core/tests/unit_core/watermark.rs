//! purge 水位线单测。

use spark_core::data_mgmt::watermark::{
    PurgeWatermarkRecord, StoragePurgeWatermark, get_purge_watermark, is_purged_by_watermark,
    purge_watermark_key, raise_purge_watermark,
};
use spark_core::storage::{MemoryStorage, StorageBackend};

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
    s.put(
        &key,
        "{\"purgedBefore\":100,\"purgedAt\":\"x\",\"removedDocs\":null}",
    )
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
    assert_eq!(
        (r.purged_before, r.purged_at, r.removed_docs),
        (100, NOW, 3)
    );

    // 更低值不降低（只升不降），removedDocs 累计
    let r = raise_purge_watermark(&mut s, "plugin:a", "c", 50, 2, NOW + 1).unwrap();
    assert_eq!(
        (r.purged_before, r.purged_at, r.removed_docs),
        (100, NOW + 1, 5)
    );

    // 更高值抬升
    let r = raise_purge_watermark(&mut s, "plugin:a", "c", 200, 1, NOW + 2).unwrap();
    assert_eq!((r.purged_before, r.removed_docs), (200, 6));

    // 持久化 JSON 字段名与顺序逐字节对齐 TS
    let raw = s
        .get(&purge_watermark_key("plugin:a", "c"))
        .unwrap()
        .unwrap();
    assert_eq!(
        raw,
        "{\"domain\":\"plugin:a\",\"collection\":\"c\",\"purgedBefore\":200,\"purgedAt\":1800000000002,\"removedDocs\":6}"
    );

    // 损坏的既有记录按 0 起算
    s.put(&purge_watermark_key("plugin:a", "broken"), "garbage")
        .unwrap();
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
    let dyn_gate: &dyn spark_core::sync::PurgeWatermark = &gate;
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
