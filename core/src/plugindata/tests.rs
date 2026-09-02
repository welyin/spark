//! plugindata 模块单测：声明注册表（代际/不可变更/校验）+ 数据读写路径
//! （merge 三档 / 驻留轴 / drop_version）+ 与 VersionedStorage 的记账联动。

use super::*;
use crate::storage::MemoryStorage;

fn input(name: &str) -> DeclareInput {
    DeclareInput {
        name: name.to_string(),
        ..Default::default()
    }
}

fn declare_sync<S: StorageBackend>(s: &mut S, name: &str) -> CollectionDeclaration {
    declare(s, "ai-chat", input(name), 1000, None).unwrap()
}

#[test]
fn declare_persists_and_is_idempotent() {
    let mut s = MemoryStorage::new();
    let d1 = declare(
        &mut s,
        "ai-chat",
        input("ai-chat:conversations"),
        1000,
        None,
    )
    .unwrap();
    assert_eq!(d1.version, "1", "version 缺省 1");
    assert_eq!(d1.scope, Scope::Sync);
    assert_eq!(d1.devices, Devices::All);
    assert_eq!(d1.merge, MergeRule::LwwRecord);
    // 声明记录落在 pdecl: 命名空间
    assert!(s.get("pdecl:ai-chat:conversations@v1").unwrap().is_some());
    // 同策略重复声明幂等（保留首次 declaredAt）
    let d2 = declare(
        &mut s,
        "ai-chat",
        input("ai-chat:conversations"),
        2000,
        None,
    )
    .unwrap();
    assert_eq!(d2.declared_at, 1000);
}

#[test]
fn declare_conflicting_strategy_rejected() {
    let mut s = MemoryStorage::new();
    declare_sync(&mut s, "ai-chat:conversations");
    let conflict = DeclareInput {
        name: "ai-chat:conversations".to_string(),
        devices: Some(Devices::PcOnly),
        ..Default::default()
    };
    let err = declare(&mut s, "ai-chat", conflict, 2000, None).unwrap_err();
    assert!(matches!(
        err,
        PlugindataError::ConflictingDeclaration { .. }
    ));
}

#[test]
fn declare_new_version_creates_independent_namespace() {
    let mut s = MemoryStorage::new();
    declare_sync(&mut s, "ai-chat:conversations");
    let v2 = DeclareInput {
        name: "ai-chat:conversations".to_string(),
        version: Some("2.0.0".to_string()),
        devices: Some(Devices::PcOnly),
        ..Default::default()
    };
    let d2 = declare(&mut s, "ai-chat", v2, 2000, None).unwrap();
    assert!(
        s.get("pdecl:ai-chat:conversations@v2.0.0")
            .unwrap()
            .is_some()
    );
    assert_ne!(
        d2.data_prefix(),
        declare_sync(&mut s, "ai-chat:conversations").data_prefix()
    );
}

#[test]
fn declare_validation() {
    let mut s = MemoryStorage::new();
    // 缺 collection 段
    assert!(matches!(
        declare(&mut s, "ai-chat", input("ai-chat"), 1, None).unwrap_err(),
        PlugindataError::InvalidName(_)
    ));
    // collection 段字符集
    assert!(matches!(
        declare(&mut s, "ai-chat", input("ai-chat:has space"), 1, None).unwrap_err(),
        PlugindataError::InvalidName(_)
    ));
    // @ 保留
    assert!(matches!(
        declare(&mut s, "ai-chat", input("ai-chat:a@b"), 1, None).unwrap_err(),
        PlugindataError::InvalidName(_)
    ));
    // 插件前缀归属
    assert!(matches!(
        declare(&mut s, "ai-chat", input("other-plugin:x"), 1, None).unwrap_err(),
        PlugindataError::NamePrefixMismatch { .. }
    ));
    // version 字符集
    let bad_version = DeclareInput {
        name: "ai-chat:x".to_string(),
        version: Some("1:0".to_string()),
        ..Default::default()
    };
    assert!(matches!(
        declare(&mut s, "ai-chat", bad_version, 1, None).unwrap_err(),
        PlugindataError::InvalidVersion(_)
    ));
}

#[test]
fn resolve_defaults_to_latest_generation() {
    let mut s = MemoryStorage::new();
    declare_sync(&mut s, "ai-chat:conversations");
    let v2 = DeclareInput {
        name: "ai-chat:conversations".to_string(),
        version: Some("2".to_string()),
        ..Default::default()
    };
    declare(&mut s, "ai-chat", v2, 2000, None).unwrap();
    // 缺省 = 最新代际（按声明时间，不解析版本号语义）
    let latest = resolve(&s, "ai-chat:conversations", None).unwrap();
    assert_eq!(latest.version, "2");
    // 显式旧代际仍可解析
    let v1 = resolve(&s, "ai-chat:conversations", Some("1")).unwrap();
    assert_eq!(v1.version, "1");
    // 未声明
    assert!(matches!(
        resolve(&s, "ai-chat:ghost", None).unwrap_err(),
        PlugindataError::NotDeclared(_)
    ));
}

#[test]
fn sync_scope_keys_managed_local_scope_not() {
    let mut s = crate::sync::versioned::VersionedStorage::new(
        MemoryStorage::new(),
        crate::sync::versioned::shared_node_id("node-a"),
    );
    let sync_decl = declare_sync(&mut s, "ai-chat:conversations");
    save(&mut s, &sync_decl, "c1", r#"{"title":"一"}"#).unwrap();
    // sync 集合：pdoc 键 + 自动 pmeta（写库即同步）。
    // 声明记录本身也走受管路径（消耗 per-node 序号 1），数据写入拿到序号 2。
    let data_key = sync_decl.data_key("c1");
    assert!(data_key.starts_with("pdoc:ai-chat:conversations@v1:"));
    assert!(s.get(&data_key).unwrap().is_some());
    let meta = crate::sync::personal::get_personal_meta(s.raw(), &data_key)
        .unwrap()
        .expect("sync 集合写入自动版本化");
    assert_eq!(meta.vv.get("node-a"), Some(&2));

    let local_decl = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            name: "ai-chat:drafts".to_string(),
            scope: Some(Scope::Local),
            ..Default::default()
        },
        1000,
        None,
    )
    .unwrap();
    save(&mut s, &local_decl, "d1", r#"{"text":"x"}"#).unwrap();
    let local_key = local_decl.data_key("d1");
    assert!(local_key.starts_with("ldoc:"));
    assert!(
        crate::sync::personal::get_personal_meta(s.raw(), &local_key)
            .unwrap()
            .is_none(),
        "local 集合不产生 pmeta（永不离开本机）"
    );
}

#[test]
fn save_get_query_del_roundtrip() {
    let mut s = MemoryStorage::new();
    let decl = declare_sync(&mut s, "ai-chat:conversations");
    save(&mut s, &decl, "c1", "\"v1\"").unwrap();
    save(&mut s, &decl, "c2", "\"v2\"").unwrap();
    save(&mut s, &decl, "other", "\"v3\"").unwrap();
    assert_eq!(get(&s, &decl, "c1").unwrap().as_deref(), Some("\"v1\""));
    // 覆盖（lww-record 允许）
    save(&mut s, &decl, "c1", "\"v1b\"").unwrap();
    assert_eq!(get(&s, &decl, "c1").unwrap().as_deref(), Some("\"v1b\""));
    // 前缀分页
    let page = query(&s, &decl, Some("c"), Some(1), None).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].0, "c1", "返回集合内相对键");
    assert_eq!(page.next_cursor.as_deref(), Some("c1"));
    let page2 = query(&s, &decl, Some("c"), None, Some("c1")).unwrap();
    assert_eq!(page2.items.len(), 1);
    assert_eq!(page2.items[0].0, "c2");
    // 删除
    del(&mut s, &decl, "c1").unwrap();
    assert!(get(&s, &decl, "c1").unwrap().is_none());
}

#[test]
fn append_only_rejects_overwrite_and_delete() {
    let mut s = MemoryStorage::new();
    let decl = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            name: "ai-chat:log".to_string(),
            merge: Some(MergeRule::AppendOnly),
            ..Default::default()
        },
        1000,
        None,
    )
    .unwrap();
    save(&mut s, &decl, "e1", "\"a\"").unwrap();
    assert!(matches!(
        save(&mut s, &decl, "e1", "\"b\"").unwrap_err(),
        PlugindataError::AppendOnlyViolation(_)
    ));
    assert!(matches!(
        del(&mut s, &decl, "e1").unwrap_err(),
        PlugindataError::AppendOnlyViolation(_)
    ));
    // 不同 key 追加正常
    save(&mut s, &decl, "e2", "\"c\"").unwrap();
}

#[test]
fn whole_merge_forces_single_key() {
    let mut s = MemoryStorage::new();
    let decl = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            name: "ai-chat:settings".to_string(),
            merge: Some(MergeRule::Whole),
            ..Default::default()
        },
        1000,
        None,
    )
    .unwrap();
    save(&mut s, &decl, "ignored-a", "\"x\"").unwrap();
    save(&mut s, &decl, "ignored-b", "\"y\"").unwrap();
    // 任意 key 读写都落到同一条记录
    assert_eq!(
        get(&s, &decl, "whatever").unwrap().as_deref(),
        Some("\"y\"")
    );
    let page = query(&s, &decl, None, None, None).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].0, WHOLE_KEY);
}

#[test]
fn devices_residency_matrix() {
    let mut s = MemoryStorage::new();
    let base = |devices: Devices| DeclareInput {
        name: "ai-chat:x".to_string(),
        devices: Some(devices),
        ..Default::default()
    };
    let all = declare(&mut s, "ai-chat", base(Devices::All), 1, None).unwrap();
    let backup = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            version: Some("2".into()),
            ..base(Devices::PcBackup)
        },
        2,
        None,
    )
    .unwrap();
    let pc = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            version: Some("3".into()),
            ..base(Devices::PcOnly)
        },
        3,
        None,
    )
    .unwrap();
    let mobile = declare(
        &mut s,
        "ai-chat",
        DeclareInput {
            version: Some("4".into()),
            ..base(Devices::MobileOnly)
        },
        4,
        None,
    )
    .unwrap();
    assert!(all.allows_device("pc") && all.allows_device("mobile"));
    assert!(backup.allows_device("pc") && backup.allows_device("mobile"));
    assert!(pc.allows_device("pc") && !pc.allows_device("mobile"));
    assert!(!mobile.allows_device("pc") && mobile.allows_device("mobile"));
}

#[test]
fn drop_version_clears_generation() {
    let mut s = MemoryStorage::new();
    let decl = declare_sync(&mut s, "ai-chat:conversations");
    save(&mut s, &decl, "c1", "\"v\"").unwrap();
    save(&mut s, &decl, "c2", "\"v\"").unwrap();
    drop_version(&mut s, &decl).unwrap();
    assert!(resolve(&s, "ai-chat:conversations", None).is_err());
    assert!(query(&s, &decl, None, None, None).unwrap().items.is_empty());
}

#[test]
fn decl_key_parsing_from_data_key() {
    assert_eq!(
        decl_key_for_data_key("pdoc:ai-chat:conversations@v1:c1").as_deref(),
        Some("pdecl:ai-chat:conversations@v1")
    );
    assert_eq!(
        decl_key_for_data_key("pdoc:github.com/o/r:col@v2.0.0:k").as_deref(),
        Some("pdecl:github.com/o/r:col@v2.0.0")
    );
    assert!(decl_key_for_data_key("ldoc:ai-chat:x@v1:k").is_none());
    assert!(decl_key_for_data_key("pdoc:no-version").is_none());
}

// ── B5：org scope 声明与读写路由 ──────────────────────────────────────

/// B5：org 空间声明落 `org:coll:{orgId}:` 键 + 幂等/冲突检查生效；
/// save/get/del/query 按 decl.space==Org 路由到 `orgd:` 键。
#[test]
fn org_declare_and_read_write_route_to_org_keys() {
    let mut s = MemoryStorage::new();
    // org 声明：space=Org、accounts/confidentiality 生效、org_id 必填
    let input = DeclareInput {
        name: "ai-chat:finance".to_string(),
        version: Some("1.0.0".to_string()),
        space: Some(Space::Org),
        accounts: Some(Accounts::DataAccounts),
        confidentiality: Some(Confidentiality::Encrypted),
        ..Default::default()
    };
    let decl = declare(&mut s, "ai-chat", input, 1000, Some("org_01")).unwrap();
    assert_eq!(decl.space, Some(Space::Org));
    assert_eq!(decl.org_id.as_deref(), Some("org_01"));
    assert_eq!(decl.accounts, Accounts::DataAccounts);
    assert_eq!(decl.confidentiality, Confidentiality::Encrypted);
    // 声明记录落在 org:coll: 键域
    assert!(
        s.get("org:coll:org_01:ai-chat:finance@v1.0.0")
            .unwrap()
            .is_some(),
        "org 声明落 org:coll: 键"
    );
    assert!(
        s.get("pdecl:ai-chat:finance@v1.0.0").unwrap().is_none(),
        "org 声明不得落 personal pdecl 键"
    );

    // 幂等：同策略重复声明返回既有
    let again = DeclareInput {
        name: "ai-chat:finance".to_string(),
        version: Some("1.0.0".to_string()),
        space: Some(Space::Org),
        accounts: Some(Accounts::DataAccounts),
        confidentiality: Some(Confidentiality::Encrypted),
        ..Default::default()
    };
    let decl2 = declare(&mut s, "ai-chat", again, 2000, Some("org_01")).unwrap();
    assert_eq!(decl2.declared_at, 1000, "幂等保留首次声明");

    // 冲突：不同策略拒绝
    let conflict = DeclareInput {
        name: "ai-chat:finance".to_string(),
        version: Some("1.0.0".to_string()),
        space: Some(Space::Org),
        accounts: Some(Accounts::AllMembers),
        ..Default::default()
    };
    assert!(matches!(
        declare(&mut s, "ai-chat", conflict, 3000, Some("org_01")).unwrap_err(),
        PlugindataError::ConflictingDeclaration { .. }
    ));

    // 读写路由：save 落 orgd: 键，get/query 按 org 域读
    save(&mut s, &decl, "k1", "\"v1\"").unwrap();
    assert!(
        s.get("orgd:org_01:ai-chat:finance@v1.0.0:k1")
            .unwrap()
            .is_some()
    );
    assert!(
        s.get("pdoc:ai-chat:finance@v1.0.0:k1").unwrap().is_none(),
        "不落 personal pdoc"
    );
    assert_eq!(get(&s, &decl, "k1").unwrap().as_deref(), Some("\"v1\""));
    let page = query(&s, &decl, Some("k"), None, None).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].0, "k1");
    del(&mut s, &decl, "k1").unwrap();
    assert!(get(&s, &decl, "k1").unwrap().is_none());
}

/// B5：org 声明 org_id 必填；encrypted 不得搭配 all-members。
#[test]
fn org_declare_validation() {
    let mut s = MemoryStorage::new();
    // 缺 org_id
    let no_oid = DeclareInput {
        name: "ai-chat:x".to_string(),
        space: Some(Space::Org),
        ..Default::default()
    };
    assert!(matches!(
        declare(&mut s, "ai-chat", no_oid, 1, None).unwrap_err(),
        PlugindataError::DeclarationConflict(_)
    ));
    // encrypted + all-members 冲突
    let bad = DeclareInput {
        name: "ai-chat:y".to_string(),
        space: Some(Space::Org),
        accounts: Some(Accounts::AllMembers),
        confidentiality: Some(Confidentiality::Encrypted),
        ..Default::default()
    };
    assert!(matches!(
        declare(&mut s, "ai-chat", bad, 1, Some("org_01")).unwrap_err(),
        PlugindataError::DeclarationConflict(_)
    ));
    // org 解析变体
    let ok = DeclareInput {
        name: "ai-chat:z".to_string(),
        space: Some(Space::Org),
        ..Default::default()
    };
    let decl = declare(&mut s, "ai-chat", ok, 1, Some("org_01")).unwrap();
    let got = resolve_org(&s, "org_01", "ai-chat:z", None).unwrap();
    assert_eq!(got.name, decl.name);
    assert_eq!(got.org_id.as_deref(), Some("org_01"));
    // personal resolve 查不到 org 声明
    assert!(matches!(
        resolve(&s, "ai-chat:z", None).unwrap_err(),
        PlugindataError::NotDeclared(_)
    ));
}

/// O2b：`declare_builtin_org_collections` 为组织注册全部内建 all-members
/// 集合（org:structure/org:contacts；F7 起 org:invites 退出 orgsync）——
/// 声明记录 + pmeta，幂等，键域与
/// [`crate::sync::orgsync::BuiltinOrgCollection`] 对齐。
#[test]
fn declare_builtin_org_collections_registers_all_builtin() {
    let mut s = MemoryStorage::new();
    declare_builtin_org_collections(&mut s, "org_01", "creator", 1000, "node-a").unwrap();
    for builtin in crate::sync::orgsync::BuiltinOrgCollection::all() {
        let key = org_decl_key("org_01", builtin.name(), builtin.version());
        let raw = s.get(&key).unwrap().expect("内建集合声明落 org:coll: 键");
        let decl: CollectionDeclaration = serde_json::from_str(&raw).unwrap();
        assert_eq!(decl.accounts, Accounts::AllMembers);
        assert_eq!(decl.space, Some(Space::Org));
        assert_eq!(decl.merge, builtin.merge());
        assert_eq!(decl.declared_by.as_deref(), Some("creator"));
        // 声明记录有 pmeta（orgsync 声明先行 / vv 折叠需要）
        assert!(
            crate::sync::get_personal_meta(&s, &key).unwrap().is_some(),
            "内建集合声明记录带 pmeta"
        );
    }
    // 幂等：重复注册不产生冲突、保留首次声明
    declare_builtin_org_collections(&mut s, "org_01", "creator", 2000, "node-a").unwrap();
    let decl = s
        .get(&org_decl_key("org_01", "org:structure", "1"))
        .unwrap()
        .map(|raw| serde_json::from_str::<CollectionDeclaration>(&raw).unwrap())
        .unwrap();
    assert_eq!(decl.declared_at, 1000, "幂等保留首次声明时间");
}
