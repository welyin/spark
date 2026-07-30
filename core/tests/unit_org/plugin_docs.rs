use serde_json::{Value, json};

use spark_core::org::plugin_docs::*;
use spark_core::schema::CollectionSchemaDeclaration;
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::{DocMeta, meta_key};

#[test]
fn parse_key_shapes() {
    assert_eq!(
        parse_plugin_doc_key("doc:plugin:chat:messages:m1"),
        Some((
            "plugin:chat".to_string(),
            "messages".to_string(),
            "m1".to_string()
        ))
    );
    // id 可含冒号（`.+`）
    assert_eq!(
        parse_plugin_doc_key("doc:plugin:chat:messages:a:b:c"),
        Some((
            "plugin:chat".to_string(),
            "messages".to_string(),
            "a:b:c".to_string()
        ))
    );
    assert_eq!(parse_plugin_doc_key("doc:plugin::messages:m1"), None);
    assert_eq!(parse_plugin_doc_key("doc:plugin:chat::m1"), None);
    assert_eq!(parse_plugin_doc_key("doc:plugin:chat:messages:"), None);
    assert_eq!(parse_plugin_doc_key("doc:core:messages:m1"), None);
    assert_eq!(parse_plugin_doc_key("plugin:chat:messages:m1"), None);
    assert_eq!(parse_plugin_doc_key(""), None);
}

#[test]
fn sync_disabled_rules() {
    assert!(is_sync_disabled(&json!({"__sync": false})));
    assert!(!is_sync_disabled(&json!({"__sync": true})));
    assert!(is_sync_disabled(&json!({"__sync": {"disabled": true}})));
    assert!(!is_sync_disabled(&json!({"__sync": {"disabled": false}})));
    for mode in ["local", "none", "disabled", " LOCAL ", "None"] {
        assert!(
            is_sync_disabled(&json!({"__sync": {"mode": mode}})),
            "mode={mode}"
        );
        assert!(
            is_sync_disabled(&json!({"__sync": {"strategy": mode}})),
            "strategy={mode}"
        );
    }
    assert!(!is_sync_disabled(&json!({"__sync": {"mode": "lww"}})));
    assert!(!is_sync_disabled(&json!({"__sync": {"mode": 0}})));
    assert!(!is_sync_disabled(&json!({"__sync": "nonsense"})));
    assert!(!is_sync_disabled(&json!({})));
    // mode 优先于 strategy（JS ?? 语义）
    assert!(!is_sync_disabled(
        &json!({"__sync": {"mode": "lww", "strategy": "local"}})
    ));
}

#[test]
fn resolve_org_id_trims_and_defaults() {
    assert_eq!(resolve_org_id(&json!({"orgId": " org_x "})), "org_x");
    assert_eq!(resolve_org_id(&json!({"orgId": 42})), "");
    assert_eq!(resolve_org_id(&json!({})), "");
}

fn put_doc(
    storage: &mut MemoryStorage,
    domain: &str,
    collection: &str,
    id: &str,
    payload: &Value,
    with_meta: bool,
) {
    storage
        .put(
            &format!("doc:{domain}:{collection}:{id}"),
            &serde_json::to_string(payload).unwrap(),
        )
        .unwrap();
    if with_meta {
        storage
            .put(
                &meta_key(domain, collection, id),
                &serde_json::to_string(&DocMeta {
                    vv: [("node1".to_string(), 3)].into_iter().collect(),
                    ts: 1234,
                    node_id: Some("node1".to_string()),
                    tombstone: None,
                })
                .unwrap(),
            )
            .unwrap();
    }
}

#[test]
fn collect_filters_by_org_and_sync_marker() {
    let mut storage = MemoryStorage::new();
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_x", "text": "hi"}),
        true,
    );
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m2",
        &json!({"orgId": "org_other"}),
        true,
    );
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m3",
        &json!({"orgId": "org_x", "__sync": false}),
        true,
    );
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m4",
        &json!({"orgId": "org_x"}),
        false,
    ); // 无 meta
    storage
        .put("doc:plugin:chat:messages:m5", "{broken")
        .unwrap(); // 损坏
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m6",
        &json!({"orgId": " org_x "}),
        true,
    ); // trim 命中

    let items = collect_syncable_plugin_docs(&storage, "org_x").unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["m1", "m6"]);
    assert_eq!(items[0].meta.ts, 1234);
    assert_eq!(items[0].meta.node_id.as_deref(), Some("node1"));

    // 空 orgId → 空集
    assert!(
        collect_syncable_plugin_docs(&storage, "  ")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn collect_carries_schema_when_declared() {
    let mut storage = MemoryStorage::new();
    spark_core::schema::declare_collection_schema(
        &mut storage,
        "plugin:chat",
        "messages",
        &CollectionSchemaDeclaration::lww(),
        1000,
    )
    .unwrap();
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_x"}),
        true,
    );
    let items = collect_syncable_plugin_docs(&storage, "org_x").unwrap();
    assert_eq!(items.len(), 1);
    let schema = items[0].schema.as_ref().unwrap();
    assert_eq!(schema.sync_strategy, Some(spark_core::schema::SyncStrategy::Lww));
}

#[test]
fn collect_org_plugin_domains_counts() {
    let mut storage = MemoryStorage::new();
    // 0 域：无任何插件文档
    assert!(
        collect_org_plugin_domains(&storage, "org_x")
            .unwrap()
            .is_empty()
    );
    // 0 域：只有别的 orgId 的文档
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_other"}),
        false,
    );
    assert!(
        collect_org_plugin_domains(&storage, "org_x")
            .unwrap()
            .is_empty()
    );
    // 1 域（同域多篇去重）
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m2",
        &json!({"orgId": "org_x"}),
        false,
    );
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "m3",
        &json!({"orgId": "org_x"}),
        false,
    );
    assert_eq!(
        collect_org_plugin_domains(&storage, "org_x").unwrap(),
        vec!["plugin:chat"]
    );
    // 2 域：扫描键升序 → 返回顺序确定；多域时由调用方取第一个（保持单 domain 语义）
    put_doc(
        &mut storage,
        "plugin:aaa",
        "notes",
        "n1",
        &json!({"orgId": "org_x"}),
        false,
    );
    let domains = collect_org_plugin_domains(&storage, "org_x").unwrap();
    assert_eq!(domains, vec!["plugin:aaa", "plugin:chat"]);
    assert_eq!(domains.into_iter().next().unwrap(), "plugin:aaa");
}

#[test]
fn collect_org_plugin_domains_skips_bad_rows() {
    let mut storage = MemoryStorage::new();
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "ok",
        &json!({"orgId": "org_x"}),
        false,
    );
    // payload 非 JSON → 跳过
    storage
        .put("doc:plugin:chat:messages:broken", "{broken")
        .unwrap();
    // payload 无 orgId → 跳过
    put_doc(
        &mut storage,
        "plugin:chat",
        "messages",
        "noorg",
        &json!({"v": 1}),
        false,
    );
    // 键形不符（doc:plugin: 前缀内但非 doc:plugin:{domain}:{collection}:{id} 三段式）→ 跳过
    storage
        .put(
            "doc:plugin::messages:x",
            &json!({"orgId": "org_x"}).to_string(),
        )
        .unwrap();
    storage
        .put("doc:plugin:chat::x", &json!({"orgId": "org_x"}).to_string())
        .unwrap();

    assert_eq!(
        collect_org_plugin_domains(&storage, "org_x").unwrap(),
        vec!["plugin:chat"]
    );
    // 空 orgId → 空集
    assert!(
        collect_org_plugin_domains(&storage, "  ")
            .unwrap()
            .is_empty()
    );
}
