//! 存储后端契约测试：同一组语义断言在 `MemoryStorage` 与 `SledStorage` 上各跑一遍。
//!
//! 覆盖：get/put/delete 往返、batch 顺序与覆盖语义、scan 的前缀默认界、
//! start 含下界 / end 排他上界、limit、reverse（含 reverse+limit 取尾部 N 条）、
//! 空范围、空前缀全库扫描、非 ASCII key 覆盖。

#![cfg(test)]

use super::{BatchOperation, ScanOptions, StorageBackend};

fn keys(items: &[(String, String)]) -> Vec<&str> {
    items.iter().map(|(k, _)| k.as_str()).collect()
}

/// 在传入的**空**存储上执行全量契约断言。
pub(crate) fn check_storage_contract<S: StorageBackend>(s: &mut S) {
    // --- get/put/delete 往返 ---
    assert_eq!(s.get("k").unwrap(), None);
    s.put("k", "v").unwrap();
    assert_eq!(s.get("k").unwrap(), Some("v".to_string()));
    s.put("k", "v2").unwrap();
    assert_eq!(s.get("k").unwrap(), Some("v2".to_string()));
    s.delete("k").unwrap();
    assert_eq!(s.get("k").unwrap(), None);
    // 删除不存在的键不报错（LevelDB del 语义）
    s.delete("k").unwrap();

    // --- batch 顺序语义 ---
    s.batch(vec![
        BatchOperation::put("a", "1"),
        BatchOperation::put("b", "2"),
        BatchOperation::delete("a"),
    ])
    .unwrap();
    assert_eq!(s.get("a").unwrap(), None);
    assert_eq!(s.get("b").unwrap(), Some("2".to_string()));
    // 同键先 put 后 delete → 删除；先 delete 后 put → 写入
    s.batch(vec![
        BatchOperation::put("c", "1"),
        BatchOperation::delete("c"),
        BatchOperation::put("c", "3"),
    ])
    .unwrap();
    assert_eq!(s.get("c").unwrap(), Some("3".to_string()));
    s.batch(vec![
        BatchOperation::delete("b"),
        BatchOperation::put("b", "4"),
    ])
    .unwrap();
    assert_eq!(s.get("b").unwrap(), Some("4".to_string()));

    // --- scan 前缀默认界 ---
    for (k, v) in [
        ("doc:a:1", "x"),
        ("doc:a:2", "y"),
        ("doc:b:1", "z"),
        ("meta:a:1", "m"),
    ] {
        s.put(k, v).unwrap();
    }
    let items = s.scan(&ScanOptions::prefix("doc:a:")).unwrap();
    assert_eq!(keys(&items), vec!["doc:a:1", "doc:a:2"]);

    // --- 非 ASCII key 覆盖（U+10FFFF 默认上界）---
    s.put("doc:a:中文", "2").unwrap();
    s.put("doc:a:😀", "3").unwrap();
    let items = s.scan(&ScanOptions::prefix("doc:a:")).unwrap();
    assert_eq!(keys(&items), vec!["doc:a:1", "doc:a:2", "doc:a:中文", "doc:a:😀"]);

    // --- start 含下界 / end 排他上界 ---
    for i in 0..5 {
        s.put(&format!("k{i}"), "v").unwrap();
    }
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            start: Some("k1".to_string()),
            end: Some("k4".to_string()),
            limit: None,
            reverse: false,
        })
        .unwrap();
    assert_eq!(keys(&items), vec!["k1", "k2", "k3"]);

    // --- limit ---
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            limit: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(keys(&items), vec!["k0", "k1"]);

    // --- reverse：全量降序 ---
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            reverse: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(keys(&items), vec!["k4", "k3", "k2", "k1", "k0"]);

    // --- reverse + limit：取最后 N 条并降序返回（LevelDB iterator 语义）---
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            limit: Some(2),
            reverse: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(keys(&items), vec!["k4", "k3"]);

    // --- 显式 start/end + reverse ---
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            start: Some("k1".to_string()),
            end: Some("k4".to_string()),
            limit: None,
            reverse: true,
        })
        .unwrap();
    assert_eq!(keys(&items), vec!["k3", "k2", "k1"]);

    // --- 空范围：start == end 与 start > end 均为空 ---
    for (start, end) in [("k2", "k2"), ("k3", "k1")] {
        let items = s
            .scan(&ScanOptions {
                prefix: "k".to_string(),
                start: Some(start.to_string()),
                end: Some(end.to_string()),
                limit: None,
                reverse: false,
            })
            .unwrap();
        assert!(items.is_empty(), "range [{start}, {end}) should be empty");
    }

    // --- 空前缀全库扫描（data-mgmt usage/exporter 依赖）---
    let items = s.scan(&ScanOptions::prefix("")).unwrap();
    assert_eq!(items.len(), 13, "全库扫描应覆盖全部已写入 key");
    // 全库扫描结果按键升序
    let mut sorted = keys(&items);
    sorted.sort_unstable();
    assert_eq!(keys(&items), sorted);

    // --- limit=0：返回空 ---
    let items = s
        .scan(&ScanOptions {
            prefix: "k".to_string(),
            limit: Some(0),
            ..Default::default()
        })
        .unwrap();
    assert!(items.is_empty());
}
