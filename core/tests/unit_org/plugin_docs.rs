use serde_json::{Value, json};

use spark_core::org::plugin_docs::*;
use spark_core::plugindata::{Accounts, CollectionDeclaration, Space, get_declaration_org};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::versioned::{VersionedStorage, shared_node_id};
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

// ---------------------------------------------------------------------------
// doc:plugin: 旧通道迁移（O2 工作项 5）：declareCollection + orgd: 键域
// ---------------------------------------------------------------------------

fn new_versioned() -> VersionedStorage<MemoryStorage> {
    VersionedStorage::new(MemoryStorage::new(), shared_node_id("node-migrate"))
}

/// 迁移：doc:plugin: 键 → org scope 声明（全缺省 data-accounts）+ orgd: 数据键，
/// 幂等（重复调用无副作用）。
#[test]
fn migrate_moves_doc_to_orgd_and_declares_collection() {
    let mut storage = new_versioned();
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_x", "text": "hi"}),
        false,
    );
    // 迁移：全缺省 accounts（data-accounts）。
    let migrated =
        migrate_plugin_docs(&mut storage, "org_x", Accounts::DataAccounts, "admin", 1000).unwrap();
    assert_eq!(migrated, 1, "迁入 1 条 orgd: 记录");

    // 集合已声明为 org scope、accounts=data-accounts。
    let decl: CollectionDeclaration =
        get_declaration_org(storage.raw(), "org_x", "chat:messages", "1")
            .unwrap()
            .expect("org 声明存在");
    assert_eq!(decl.space, Some(Space::Org));
    assert_eq!(decl.accounts, Accounts::DataAccounts);
    assert_eq!(decl.scope, spark_core::plugindata::Scope::Sync);

    // 数据已迁入 orgd: 键域（值 = payload 原样 JSON）。
    let data_key = "orgd:org_x:chat:messages@v1:m1";
    let stored = storage.get(data_key).unwrap().expect("orgd 记录存在");
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["text"],
        "hi"
    );

    // 幂等：重复迁移 0 条新增（集合已声明、orgd 键已存在）。
    let again =
        migrate_plugin_docs(&mut storage, "org_x", Accounts::DataAccounts, "admin", 2000).unwrap();
    assert_eq!(again, 0, "重复迁移无新增");
}

/// 迁移：`accounts` 入参映射——需全员驻留时显式 all-members。
#[test]
fn migrate_maps_all_members_when_requested() {
    let mut storage = new_versioned();
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_x"}),
        false,
    );
    migrate_plugin_docs(&mut storage, "org_x", Accounts::AllMembers, "admin", 1000).unwrap();
    let decl: CollectionDeclaration =
        get_declaration_org(storage.raw(), "org_x", "chat:messages", "1")
            .unwrap()
            .expect("org 声明存在");
    assert_eq!(decl.accounts, Accounts::AllMembers);
}

/// 迁移：scope:local（sync 禁用）文档不迁移。
#[test]
fn migrate_skips_sync_disabled_docs() {
    let mut storage = new_versioned();
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "messages",
        "m1",
        &json!({"orgId": "org_x", "__sync": {"mode": "local"}}),
        false,
    );
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "messages",
        "m2",
        &json!({"orgId": "org_x"}),
        false,
    );
    let migrated =
        migrate_plugin_docs(&mut storage, "org_x", Accounts::DataAccounts, "admin", 1000).unwrap();
    // m1 不迁移（local），只迁 m2。
    assert_eq!(migrated, 1);
    assert!(
        storage
            .get("orgd:org_x:chat:messages@v1:m1")
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get("orgd:org_x:chat:messages@v1:m2")
            .unwrap()
            .is_some()
    );
}

/// F7：迁移遇非法集合名（declare 失败）逐条跳过 + warn，一颗耗子屎不堵
/// 后续迁移（不再整体中断）。
#[test]
fn migrate_skips_invalid_name_and_continues() {
    let mut storage = new_versioned();
    // 非法：collection 段含 @ → 集合名 `chat:a@b` 触发 InvalidName。
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "a@b",
        "bad",
        &json!({"orgId": "org_x"}),
        false,
    );
    // 正常：collection 合法。
    put_doc(
        storage.raw_mut(),
        "plugin:chat",
        "messages",
        "good",
        &json!({"orgId": "org_x"}),
        false,
    );
    let migrated =
        migrate_plugin_docs(&mut storage, "org_x", Accounts::DataAccounts, "admin", 1000).unwrap();
    // 非法名被跳过，正常那条仍被迁移（整体不中断）。
    assert_eq!(migrated, 1, "非法名跳过，正常那条迁入");
    assert!(
        storage.get("orgd:org_x:chat:a@b@v1:bad").unwrap().is_none(),
        "非法集合未迁入"
    );
    assert!(
        storage
            .get("orgd:org_x:chat:messages@v1:good")
            .unwrap()
            .is_some(),
        "正常集合已迁入"
    );
}
