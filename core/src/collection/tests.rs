//! 集合本地读写路径单测。

use super::*;
use serde_json::json;

use crate::evidence::{get_evidence_entry, get_evidence_head, verify_evidence_chain};
use crate::schema::{CollectionSchemaDeclaration, declare_collection_schema};
use crate::storage::MemoryStorage;
use crate::sync::meta::get_meta;

const NOW: i64 = 1_800_000_000_000;
const NODE: &str = "local-node";

fn lww_collection(indexed: &[&str]) -> DocumentCollection {
    DocumentCollection::new(
        "chat",
        "messages",
        CollectionConfig {
            indexed_fields: indexed.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    )
}

fn declare_lww(s: &mut MemoryStorage) {
    declare_collection_schema(
        s,
        "chat",
        "messages",
        &CollectionSchemaDeclaration::lww(),
        NOW,
    )
    .unwrap();
}

#[test]
fn put_writes_doc_meta_evidence_with_ts_byte_layout() {
    let mut s = MemoryStorage::new();
    declare_lww(&mut s);
    let c = lww_collection(&[]);
    let doc = json!({"text": "hello", "from": "alice"});
    let write = c.put(&mut s, "id1", &doc, NODE, NOW).unwrap();

    // doc 键值逐字节：紧凑 JSON、键序保持插入序
    assert_eq!(
        s.get("doc:chat:messages:id1").unwrap().as_deref(),
        Some(r#"{"text":"hello","from":"alice"}"#)
    );
    // meta 键值逐字节：{vv, ts, nodeId} 键序
    assert_eq!(
        s.get("meta:chat:messages:id1").unwrap().as_deref(),
        Some(format!(r#"{{"vv":{{"{NODE}":1}},"ts":{NOW},"nodeId":"{NODE}"}}"#).as_str())
    );
    assert_eq!(write.meta.vv.get(NODE), Some(&1));

    // lww 未声明 enableEvidence → 无存证
    assert!(get_evidence_head(&s).unwrap().is_none());
}

#[test]
fn put_with_evidence_and_index_diff() {
    let mut s = MemoryStorage::new();
    declare_collection_schema(
        &mut s,
        "chat",
        "messages",
        &CollectionSchemaDeclaration {
            sync_strategy: Some(SyncStrategy::Lww),
            governance: false,
            enable_evidence: true,
        },
        NOW,
    )
    .unwrap();
    let c = lww_collection(&["from", "meta.tag"]);
    c.put(
        &mut s,
        "id1",
        &json!({"from": "alice", "meta": {"tag": "a b"}}),
        NODE,
        NOW,
    )
    .unwrap();
    // 索引键：值经 encodeURIComponent
    assert_eq!(
        s.get("idx:chat:messages:from:alice:id1")
            .unwrap()
            .as_deref(),
        Some("")
    );
    assert_eq!(
        s.get("idx:chat:messages:meta.tag:a%20b:id1")
            .unwrap()
            .as_deref(),
        Some("")
    );
    // 存证第 1 条
    let entry = get_evidence_entry(&s, 1).unwrap().unwrap();
    assert_eq!(entry.op, EvidenceOp::Put);
    assert_eq!(entry.prev_hash, None);
    assert_eq!(entry.node_id, NODE);

    // 更新：from 变化 → 旧索引删、新索引增；meta 计数递增
    c.put(&mut s, "id1", &json!({"from": "bob"}), NODE, NOW + 1)
        .unwrap();
    assert!(s.get("idx:chat:messages:from:alice:id1").unwrap().is_none());
    assert!(s.get("idx:chat:messages:from:bob:id1").unwrap().is_some());
    // 字段消失的索引项也被清理
    assert!(
        s.get("idx:chat:messages:meta.tag:a%20b:id1")
            .unwrap()
            .is_none()
    );
    let meta = get_meta(&s, "chat", "messages", "id1").unwrap().unwrap();
    assert_eq!(meta.vv.get(NODE), Some(&2));
    assert_eq!(meta.ts, NOW + 1);
    assert!(verify_evidence_chain(&s).unwrap());
}

#[test]
fn append_only_rejects_overwrite_and_delete_with_ts_messages() {
    let mut s = MemoryStorage::new();
    // 未声明集合默认 append-only（最安全兜底）
    let c = lww_collection(&[]);
    c.put(&mut s, "id1", &json!({"v": 1}), NODE, NOW).unwrap();
    let err = c
        .put(&mut s, "id1", &json!({"v": 2}), NODE, NOW)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Collection \"messages\" is append-only: document \"id1\" already exists and cannot be overwritten"
    );
    let err = c.delete(&mut s, "id1", NODE, NOW).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Collection \"messages\" is append-only: documents cannot be deleted"
    );
    // append-only 强制存证（默认策略 enableEvidence=true）
    assert!(verify_evidence_chain(&s).unwrap());
    assert_eq!(get_evidence_head(&s).unwrap().unwrap().seq, 1);
}

#[test]
fn delete_writes_tombstone_and_evidence() {
    let mut s = MemoryStorage::new();
    declare_lww(&mut s);
    let c = lww_collection(&["from"]);
    c.put(&mut s, "id1", &json!({"from": "alice"}), NODE, NOW)
        .unwrap();
    let write = c.delete(&mut s, "id1", NODE, NOW + 5).unwrap().unwrap();
    assert!(c.get(&s, "id1").unwrap().is_none());
    assert!(s.get("idx:chat:messages:from:alice:id1").unwrap().is_none());
    // 墓碑 meta 逐字节：{vv, ts, tombstone:true}，无 nodeId
    assert_eq!(
        s.get("meta:chat:messages:id1").unwrap().as_deref(),
        Some(
            format!(
                r#"{{"vv":{{"{NODE}":2}},"ts":{},"tombstone":true}}"#,
                NOW + 5
            )
            .as_str()
        )
    );
    // 广播用 meta 是非墓碑版（含 nodeId）
    assert_eq!(write.meta.node_id.as_deref(), Some(NODE));
    // 不存在再删为空操作
    assert!(c.delete(&mut s, "id1", NODE, NOW).unwrap().is_none());
}

#[test]
fn query_primary_pagination_reverse_and_filter() {
    let mut s = MemoryStorage::new();
    declare_lww(&mut s);
    let c = lww_collection(&[]);
    for i in 0..5 {
        c.put(
            &mut s,
            &format!("id{i}"),
            &json!({"n": i, "kind": if i % 2 == 0 { "even" } else { "odd" }}),
            NODE,
            NOW + i,
        )
        .unwrap();
    }
    // 第一页（默认升序，limit 2 → next_cursor）
    let page1 = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        page1
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id0", "id1"]
    );
    assert_eq!(page1.next_cursor.as_deref(), Some("id1"));
    // 第二页
    let page2 = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(2),
                start_after_id: page1.next_cursor.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        page2
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id2", "id3"]
    );
    assert_eq!(page2.next_cursor.as_deref(), Some("id3"));
    // 最后一页（不足 limit → 无游标）
    let page3 = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(2),
                start_after_id: page2.next_cursor.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        page3
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id4"]
    );
    assert_eq!(page3.next_cursor, None);
    // 逆序
    let rev = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(2),
                reverse: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        rev.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec!["id4", "id3"]
    );
    // filter：eq 命中 + gt 过滤
    let filtered = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(10),
                filter: vec![QueryFilter {
                    field: "kind".into(),
                    value: json!("even"),
                    op: FilterOp::Eq,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        filtered
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id0", "id2", "id4"]
    );
    let filtered = c
        .query(
            &s,
            &QueryOptions {
                limit: Some(10),
                filter: vec![QueryFilter {
                    field: "n".into(),
                    value: json!(2),
                    op: FilterOp::Gt,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    // 数字按 String 归一比较："3" > "2" 且 "4" > "2"
    assert_eq!(
        filtered
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        vec!["id3", "id4"]
    );
}

#[test]
fn query_index_exact_prefix_and_stale_index() {
    let mut s = MemoryStorage::new();
    declare_lww(&mut s);
    let c = lww_collection(&["from"]);
    c.put(&mut s, "id1", &json!({"from": "alice"}), NODE, NOW)
        .unwrap();
    c.put(&mut s, "id2", &json!({"from": "alina"}), NODE, NOW)
        .unwrap();
    c.put(&mut s, "id3", &json!({"from": "bob"}), NODE, NOW)
        .unwrap();
    // 中文 id 的索引键也在扫描上界内（U+10FFFF；TS `\xFF` 会漏）
    c.put(&mut s, "中文", &json!({"from": "alice"}), NODE, NOW)
        .unwrap();

    // 精确匹配：alice 命中 id1 与中文 id
    let r = c
        .query(
            &s,
            &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("alice")),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec!["id1", "中文"]
    );
    // 前缀匹配：al* 命中 alice/alina（字节序：alice:中文 < alina:id2）
    let r = c
        .query(
            &s,
            &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("al")),
                index_prefix: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec!["id1", "中文", "id2"]
    );
    // 脏索引：手写无主文档的索引键 → 跳过
    s.put("idx:chat:messages:from:ghost:missing", "").unwrap();
    let r = c
        .query(
            &s,
            &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("ghost")),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(r.items.is_empty());
}

#[test]
fn js_string_semantics() {
    assert_eq!(js_string(&json!(true)), "true");
    assert_eq!(js_string(&json!("a")), "a");
    assert_eq!(js_string(&json!(1.5)), "1.5");
    assert_eq!(js_string(&json!(2)), "2");
    assert_eq!(js_string(&json!([1, "a", null])), "1,a,");
    assert_eq!(js_string(&json!({"x": 1})), "[object Object]");
}

#[test]
fn resolve_field_nested_and_array() {
    let doc = json!({"a": {"b": {"c": 7}}, "arr": [{"x": 1}]});
    assert_eq!(resolve_field_value(&doc, "a.b.c"), Some(&json!(7)));
    assert_eq!(resolve_field_value(&doc, "arr.0.x"), Some(&json!(1)));
    assert_eq!(resolve_field_value(&doc, "a.missing"), None);
    assert_eq!(resolve_field_value(&doc, "a.b.c.d"), None);
}
