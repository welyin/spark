//! 手动清理执行器 L2 级单测。

use super::*;
use crate::data_mgmt::watermark::{get_purge_watermark, is_purged_by_watermark};
use crate::storage::MemoryStorage;

const NOW: i64 = 1_800_000_000_000;

fn opts(domain: &str, before_ts: i64) -> PurgeOptions {
    PurgeOptions {
        domain: domain.to_string(),
        before_ts,
        collection: None,
    }
}

fn meta_json(ts: i64) -> String {
    format!("{{\"vv\":{{\"n1\":1}},\"ts\":{ts}}}")
}

/// 标准 fixture：col1 下 id1（ts=100，旧）与 id2（ts=300，新），含 doc/meta/idx 三件套。
fn standard_fixture() -> MemoryStorage {
    let mut s = MemoryStorage::new();
    s.put("doc:plugin:app:col1:id1", "{\"v\":1}").unwrap();
    s.put("meta:plugin:app:col1:id1", &meta_json(100)).unwrap();
    s.put("idx:plugin:app:col1:byX:enc1:id1", "").unwrap();
    s.put("doc:plugin:app:col1:id2", "{\"v\":2}").unwrap();
    s.put("meta:plugin:app:col1:id2", &meta_json(300)).unwrap();
    s.put("idx:plugin:app:col1:byX:enc2:id2", "").unwrap();
    s
}

#[test]
fn select_validates_domain_before_before_ts() {
    let s = MemoryStorage::new();
    // 非 plugin 域
    let err = purge_domain_docs(&mut s.clone(), &opts("chat", 100), NOW).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Refused to purge non-plugin domain \"chat\": only plugin domains can be purged"
    );
    // 恰好 "plugin:"（长度 7）也拒绝
    let err = purge_domain_docs(&mut s.clone(), &opts("plugin:", 100), NOW).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Refused to purge non-plugin domain \"plugin:\": only plugin domains can be purged"
    );
    // domain 校验优先于 beforeTs 校验
    let err = purge_domain_docs(&mut s.clone(), &opts("chat", 0), NOW).unwrap_err();
    assert!(matches!(err, DataMgmtError::NonPluginDomain(_)));
    // beforeTs <= 0
    let err = purge_domain_docs(&mut s.clone(), &opts("plugin:app", 0), NOW).unwrap_err();
    assert_eq!(err.to_string(), "beforeTs must be a positive timestamp");
    let err = purge_domain_docs(&mut s.clone(), &opts("plugin:app", -1), NOW).unwrap_err();
    assert!(matches!(err, DataMgmtError::InvalidBeforeTs));
}

#[test]
fn select_ts_strict_less_than_and_parse_failures() {
    let mut s = MemoryStorage::new();
    s.put("meta:plugin:app:c:old", &meta_json(199)).unwrap(); // 选中
    s.put("meta:plugin:app:c:equal", &meta_json(200)).unwrap(); // ts == beforeTs 不选（严格 <）
    s.put("meta:plugin:app:c:new", &meta_json(201)).unwrap(); // 不选
    s.put("meta:plugin:app:c:broken", "not json").unwrap(); // 损坏 → 保守跳过
    s.put("meta:plugin:app:c:strts", "{\"ts\":\"100\"}")
        .unwrap(); // ts 非 number → 跳过
    s.put("meta:plugin:app:c:nots", "{\"vv\":{}}").unwrap(); // ts 缺失 → 跳过
    // tombstone meta 同样按 ts 判定（同时代 tombstone 一并清理）
    s.put("meta:plugin:app:c:tomb", "{\"ts\":50,\"tombstone\":true}")
        .unwrap();

    let selected = select_expired_metas(&s, &opts("plugin:app", 200)).unwrap();
    let ids: Vec<&str> = selected.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["old", "tomb"]); // 扫描按键升序
}

#[test]
fn select_key_parsing_edge_cases() {
    let mut s = MemoryStorage::new();
    // 空 collection（separator == 0）→ 跳过
    s.put("meta:plugin:app::id", &meta_json(1)).unwrap();
    // 空 id（separator 在末尾）→ 跳过
    s.put("meta:plugin:app:col:", &meta_json(1)).unwrap();
    // 无冒号 → 跳过
    s.put("meta:plugin:app:colonly", &meta_json(1)).unwrap();
    // id 含冒号：第一个冒号后全部内容为 id（精确）
    s.put("meta:plugin:app:col:a:b", &meta_json(1)).unwrap();

    let selected = select_expired_metas(&s, &opts("plugin:app", 100)).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].collection, "col");
    assert_eq!(selected[0].id, "a:b");
}

#[test]
fn select_with_collection_option_scans_only_that_collection() {
    let mut s = MemoryStorage::new();
    s.put("meta:plugin:app:c1:a", &meta_json(1)).unwrap();
    s.put("meta:plugin:app:c2:b", &meta_json(1)).unwrap();
    let options = PurgeOptions {
        domain: "plugin:app".to_string(),
        before_ts: 100,
        collection: Some("c1".to_string()),
    };
    let selected = select_expired_metas(&s, &options).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].collection, "c1");
}

#[test]
fn purge_full_flow_deletes_three_key_kinds_and_writes_watermark_and_audit() {
    let mut s = standard_fixture();
    let result = purge_domain_docs(&mut s, &opts("plugin:app", 200), NOW).unwrap();

    assert_eq!(result.domain, "plugin:app");
    assert_eq!(result.before_ts, 200);
    assert_eq!(result.collections, ["col1"]);
    assert_eq!(result.removed_docs, 1);
    assert_eq!(result.purged_at, NOW);
    // freedBytes = doc/meta/idx 三件套 key+value 合计
    let expected_freed = ("doc:plugin:app:col1:id1".len()
        + "{\"v\":1}".len()
        + "meta:plugin:app:col1:id1".len()
        + meta_json(100).len()
        + "idx:plugin:app:col1:byX:enc1:id1".len()) as u64;
    assert_eq!(result.freed_bytes, expected_freed);

    // id1 三件套全删；id2（ts=300 >= 200）完整保留
    assert!(s.get("doc:plugin:app:col1:id1").unwrap().is_none());
    assert!(s.get("meta:plugin:app:col1:id1").unwrap().is_none());
    assert!(s.get("idx:plugin:app:col1:byX:enc1:id1").unwrap().is_none());
    assert!(s.get("doc:plugin:app:col1:id2").unwrap().is_some());
    assert!(s.get("meta:plugin:app:col1:id2").unwrap().is_some());
    assert!(s.get("idx:plugin:app:col1:byX:enc2:id2").unwrap().is_some());

    // 水位线抬升到 beforeTs，removedDocs=1
    let w = get_purge_watermark(&s, "plugin:app", "col1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (w.purged_before, w.removed_docs, w.purged_at),
        (200, 1, NOW)
    );
    // 同时代远端重推被拦截，边界 ts == 200 不拦
    assert!(is_purged_by_watermark(&s, "plugin:app", "col1", 199).unwrap());
    assert!(!is_purged_by_watermark(&s, "plugin:app", "col1", 200).unwrap());

    // 审计日志：key = doc:system:purge-log:{purgedAt}，value 逐字节对齐 TS
    let log = s
        .get(&format!("doc:system:purge-log:{NOW}"))
        .unwrap()
        .unwrap();
    let expected_log = format!(
        "{{\"domain\":\"plugin:app\",\"collection\":null,\"beforeTs\":200,\"collections\":[\"col1\"],\"removedDocs\":1,\"freedBytes\":{expected_freed},\"purgedAt\":{NOW}}}"
    );
    assert_eq!(log, expected_log);
}

#[test]
fn purge_multi_collection_watermark_per_collection() {
    let mut s = MemoryStorage::new();
    // col1：两条旧；col2：一条旧
    s.put("meta:plugin:app:col1:a", &meta_json(10)).unwrap();
    s.put("doc:plugin:app:col1:a", "1").unwrap();
    s.put("meta:plugin:app:col1:b", &meta_json(20)).unwrap();
    s.put("meta:plugin:app:col2:c", &meta_json(30)).unwrap();
    s.put("idx:plugin:app:col2:byY:v:c", "").unwrap();

    let result = purge_domain_docs(&mut s, &opts("plugin:app", 100), NOW).unwrap();
    assert_eq!(result.collections, ["col1", "col2"]); // 首次出现顺序（键升序）
    assert_eq!(result.removed_docs, 3);

    let w1 = get_purge_watermark(&s, "plugin:app", "col1")
        .unwrap()
        .unwrap();
    let w2 = get_purge_watermark(&s, "plugin:app", "col2")
        .unwrap()
        .unwrap();
    assert_eq!((w1.purged_before, w1.removed_docs), (100, 2));
    assert_eq!((w2.purged_before, w2.removed_docs), (100, 1));
}

#[test]
fn empty_purge_leaves_no_trace() {
    let mut s = standard_fixture();
    let before = s.len();
    // beforeTs=50：无 meta 早于 50
    let result = purge_domain_docs(&mut s, &opts("plugin:app", 50), NOW).unwrap();
    assert_eq!(result.removed_docs, 0);
    assert_eq!(result.freed_bytes, 0);
    assert!(result.collections.is_empty());
    assert_eq!(result.purged_at, NOW);
    // 坑 #4：不抬水位线、不写审计日志、库内容零变化
    assert_eq!(s.len(), before);
    assert!(
        get_purge_watermark(&s, "plugin:app", "col1")
            .unwrap()
            .is_none()
    );
    assert!(
        s.get(&format!("doc:system:purge-log:{NOW}"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn preview_matches_execute_without_writes() {
    let mut s = standard_fixture();
    let preview = preview_purge_domain_docs(&s, &opts("plugin:app", 200)).unwrap();
    assert_eq!(preview.collections, ["col1"]);
    assert_eq!(preview.affected_docs, 1);
    let before = s.len();

    let result = purge_domain_docs(&mut s, &opts("plugin:app", 200), NOW).unwrap();
    assert_eq!(preview.affected_bytes, result.freed_bytes);
    assert_eq!(preview.affected_docs, result.removed_docs);
    // preview 本身不写（上方 execute 前的 len 与 fixture 相同）
    assert_eq!(before, 6);
}

#[test]
fn idx_suffix_match_pit10_replicated() {
    // 坑 #10 如实复刻并固定行为：id "a:b" 的索引行尾部以 ":b" 结尾，
    // 清理 id "b" 时被误匹配删除；doc 按精确 id 匹配不受影响。
    let mut s = MemoryStorage::new();
    s.put("meta:plugin:app:c:b", &meta_json(10)).unwrap(); // 选中 id "b"
    s.put("doc:plugin:app:c:b", "v").unwrap();
    s.put("meta:plugin:app:c:a:b", &meta_json(9999)).unwrap(); // id "a:b" 不在选中集（ts 新）
    s.put("doc:plugin:app:c:a:b", "v2").unwrap();
    s.put("idx:plugin:app:c:byX:v:a:b", "").unwrap(); // id "a:b" 的索引行

    let result = purge_domain_docs(&mut s, &opts("plugin:app", 100), NOW).unwrap();
    assert_eq!(result.removed_docs, 1);
    // id "b" 的 doc/meta 删除
    assert!(s.get("doc:plugin:app:c:b").unwrap().is_none());
    assert!(s.get("meta:plugin:app:c:b").unwrap().is_none());
    // 缺陷：id "a:b" 的索引行被 ":b" 尾部匹配误删
    assert!(s.get("idx:plugin:app:c:byX:v:a:b").unwrap().is_none());
    // 但 id "a:b" 的 doc/meta 按精确匹配保留
    assert!(s.get("doc:plugin:app:c:a:b").unwrap().is_some());
    assert!(s.get("meta:plugin:app:c:a:b").unwrap().is_some());
}

#[test]
fn purge_with_collection_option_writes_collection_in_audit() {
    let mut s = MemoryStorage::new();
    s.put("meta:plugin:app:c1:a", &meta_json(10)).unwrap();
    s.put("meta:plugin:app:c2:b", &meta_json(10)).unwrap();
    let options = PurgeOptions {
        domain: "plugin:app".to_string(),
        before_ts: 100,
        collection: Some("c1".to_string()),
    };
    let result = purge_domain_docs(&mut s, &options, NOW).unwrap();
    assert_eq!(result.collections, ["c1"]);
    assert!(s.get("meta:plugin:app:c2:b").unwrap().is_some());
    let log = s
        .get(&format!("doc:system:purge-log:{NOW}"))
        .unwrap()
        .unwrap();
    assert!(log.contains("\"collection\":\"c1\""));
    assert!(
        get_purge_watermark(&s, "plugin:app", "c2")
            .unwrap()
            .is_none()
    );
}
