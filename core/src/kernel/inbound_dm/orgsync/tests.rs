//! orgsync 入站编排的内联测试（从 `orgsync.rs` 拆出，文件长度硬线——
//! 测试天然罗列另置，与 access_tests.rs 同款先例）。零逻辑变化。

use super::*;
use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
use crate::plugindata::{Accounts, Scope, Space, declare};
use crate::storage::{MemoryStorage, ScanOptions};
use crate::sync::meta::DocMeta;
use serde_json::json;

fn member(root_id: &str) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role: OrganizationRole::Member,
        joined_at: 1000,
        added_by: "creator".to_string(),
        node_info: None,
        nickname: None,
        avatar: None,
        signature: None,
        gender: None,
        region: None,
        use_personal_identity: None,
        access_key: None,
        kind: None,
        org_binding: None,
        extra: Default::default(),
    }
}

fn ctx<'a>(
    my_root_id: &'a str,
    remote_peer_id: &'a str,
    online: &'a std::collections::HashSet<String>,
) -> InboundContext<'a> {
    InboundContext {
        my_root_id,
        my_nickname: "me",
        remote_peer_id,
        online_peers: online,
        node_id: "local-node",
        now_ms: 2000,
        kverify: None,
    }
}

fn setup_org_and_collection() -> MemoryStorage {
    let mut s = MemoryStorage::new();
    // 组织记录：member-a（发送方/接收方）与 self（本机）均为成员
    let record = OrganizationRecord {
        org_id: "org_0000000000000001".to_string(),
        name: "t".to_string(),
        description: String::new(),
        avatar: String::new(),
        base_plugin_domain: None,
        created_at: 1000,
        created_by: "self".to_string(),
        updated_at: 1000,
        members: vec![member("member-a"), member("self")],
        sync: None,
        gateways: vec![],
        data_accounts: vec![],
        org_address: None,
        is_public: false,
        domain_type: None,
        extra: Default::default(),
    };
    crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
    // 声明集合：all-members（复制组 = 全体成员）
    let decl = declare(
        &mut s,
        "ai-chat",
        crate::plugindata::DeclareInput {
            name: "ai-chat:finance".to_string(),
            version: Some("1.0.0".to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::AllMembers),
            scope: Some(Scope::Sync),
            ..Default::default()
        },
        1000,
        Some("org_0000000000000001"),
    )
    .unwrap();
    let _ = decl;
    s
}

fn meta(node: &str, counter: i64) -> DocMeta {
    DocMeta {
        vv: [(node.to_string(), counter)].into_iter().collect(),
        ts: 2000,
        node_id: Some(node.to_string()),
        ..Default::default()
    }
}

/// B3：orgsync-data 入站 key 白名单——`orgd:` 数据键放行，
/// 越界键（`p2p:` 等）整批拒收。
#[test]
fn orgsync_data_key_whitelist_rejects_out_of_collection() {
    let mut s = setup_org_and_collection();
    let online = std::collections::HashSet::new();
    let c = ctx("self", "peer-a", &online);

    // 合法：orgd: 数据键 → 应应用（返回 ok，orgsync_out 非空）
    let ok_body = crate::sync::orgsync::build_orgsync_data_batch(
        "org_0000000000000001",
        "ai-chat:finance@v1.0.0",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1".to_string(),
            value: serde_json::json!("v"),
            meta: meta("node-a", 1),
            dseq: None,
        }],
        0,
        1,
    );
    let res = handle_orgsync_data(&mut s, &c, "member-a", &ok_body).unwrap();
    assert_eq!(res.response["ok"], json!(true), "合法 orgd 键应放行");
    assert!(
        s.get("orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1")
            .unwrap()
            .is_some(),
        "合法记录已合入"
    );

    // 越界：p2p: 键 → 整批拒收（reason key-out-of-collection）
    let bad_body = crate::sync::orgsync::build_orgsync_data_batch(
        "org_0000000000000001",
        "ai-chat:finance@v1.0.0",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: "p2p:identity:privateKey".to_string(),
            value: serde_json::json!("x"),
            meta: meta("node-a", 1),
            dseq: None,
        }],
        0,
        1,
    );
    let res2 = handle_orgsync_data(&mut s, &c, "member-a", &bad_body).unwrap();
    assert_eq!(res2.response["ok"], json!(false));
    assert_eq!(res2.response["reason"], json!("key-out-of-collection"));
    assert!(
        s.get("p2p:identity:privateKey").unwrap().is_none(),
        "越界键不得落库"
    );
}

/// B4：orgsync-data 入站远端墓碑落地后补登 **org 域 dlog**（接力传播），
/// 个人域 dlog 不被 orgd: 污染。
#[test]
fn orgsync_data_tombstone_relay_appends_org_dlog() {
    let mut s = setup_org_and_collection();
    let online = std::collections::HashSet::new();
    let c = ctx("self", "peer-a", &online);
    let key = "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1";

    // 先落一条本地记录
    s.put(key, "\"v1\"").unwrap();
    s.put(
        &format!("pmeta:{key}"),
        &serde_json::to_string(&DocMeta {
            vv: [("node-a".to_string(), 1)].into_iter().collect(),
            ts: 1000,
            node_id: Some("node-a".to_string()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();

    // 入站远端墓碑（vv=2 领先）
    let body = crate::sync::orgsync::build_orgsync_data_batch(
        "org_0000000000000001",
        "ai-chat:finance@v1.0.0",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: serde_json::Value::Null,
            meta: DocMeta {
                vv: [("node-a".to_string(), 2)].into_iter().collect(),
                ts: 2000,
                node_id: Some("node-a".to_string()),
                tombstone: Some(true),
            },
            dseq: Some(3),
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();

    // 个人域 dlog 为空
    let personal_dlog: Vec<_> = s
        .scan(&ScanOptions::prefix("dlog:entry:"))
        .unwrap()
        .into_iter()
        .collect();
    assert!(personal_dlog.is_empty(), "个人域 dlog 不被 orgd 污染");
    // org 域 dlog 已补登
    let entries = crate::sync::orgsync::org_dlog_entries_after(
        &s,
        "org_0000000000000001",
        "ai-chat:finance",
        "1.0.0",
        0,
    )
    .unwrap();
    assert_eq!(entries.len(), 1, "远端墓碑接力进 org dlog");
    assert_eq!(entries[0].1, key);
}

/// O2b 双线幂等合入：存量组织键（内建 all-members 集合）经 orgsync-data
/// 到达，与既有同 vv 数据合并幂等——重复/并发双线（orgsync + pdsync）
/// 到达不重复 bump vv、值不被旧版本覆盖。
#[test]
fn orgsync_data_builtin_collection_merges_idempotently() {
    let mut s = setup_org_and_collection();
    let online = std::collections::HashSet::new();
    let c = ctx("self", "peer-a", &online);
    // 声明内建 org:contacts 集合（all-members，声明记录 + pmeta）
    let org_id = "org_0000000000000001";
    let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
    let decl = declare(
        &mut s,
        "org",
        crate::plugindata::DeclareInput {
            name: "org:contacts".to_string(),
            version: Some("1".to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::AllMembers),
            scope: Some(Scope::Sync),
            ..Default::default()
        },
        1000,
        Some(org_id),
    )
    .unwrap();
    s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
        .unwrap();
    s.put(
        &format!("pmeta:{decl_key}"),
        &serde_json::to_string(&meta("node-a", 1)).unwrap(),
    )
    .unwrap();

    // 存量键 ct:org:{orgId}:* 经 orgsync 到达（vv node-a=1）
    let key = "ct:org:org_0000000000000001:member-x";
    let remote_meta = DocMeta {
        vv: [("node-a".to_string(), 1)].into_iter().collect(),
        ts: 1500,
        node_id: Some("node-a".to_string()),
        ..Default::default()
    };
    let body = crate::sync::orgsync::build_orgsync_data_batch(
        org_id,
        "org:contacts@v1",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: json!("member-value"),
            meta: remote_meta.clone(),
            dseq: None,
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
    assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
    let stored = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
    assert_eq!(stored.vv.get("node-a"), Some(&1));

    // 双线幂等：同 vv 再次到达（pdsync/orgsync 并发重放）→ 不重复 bump、
    // 值不翻转
    let body2 = crate::sync::orgsync::build_orgsync_data_batch(
        org_id,
        "org:contacts@v1",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: json!("member-value"),
            meta: remote_meta,
            dseq: None,
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
    assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
    let stored2 = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
    assert_eq!(
        stored2.vv.get("node-a"),
        Some(&1),
        "同 vv 重复到达不重复 bump"
    );
}

/// F5：远端合入**存量组织键**（内建集合键域 ct:org:）墓碑 → org dlog 与
/// 个人域 dlog **双有**（与本地 tombstone_local 双写对称：orgsync 走 org
/// dlog、pdsync 自设备同步走个人 dlog）。
#[test]
fn orgsync_data_legacy_key_tombstone_writes_both_dlogs() {
    let mut s = setup_org_and_collection();
    let online = std::collections::HashSet::new();
    let c = ctx("self", "peer-a", &online);
    let org_id = "org_0000000000000001";
    // 声明 org:contacts 内建集合（存量键域 ct:org:{orgId}:*）
    let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
    let decl = declare(
        &mut s,
        "org",
        crate::plugindata::DeclareInput {
            name: "org:contacts".to_string(),
            version: Some("1".to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::AllMembers),
            scope: Some(Scope::Sync),
            ..Default::default()
        },
        1000,
        Some(org_id),
    )
    .unwrap();
    s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
        .unwrap();
    s.put(
        &format!("pmeta:{decl_key}"),
        &serde_json::to_string(&meta("node-a", 1)).unwrap(),
    )
    .unwrap();
    // 先落一条存量数据：声明 pmeta（node-a:1）已把 node-a 序号种子到 1，
    // 本次受管写拿到 per-node 序号 2 → vv={node-a:2}
    let key = "ct:org:org_0000000000000001:member-x";
    crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
    // 远端墓碑（vv=3 领先本地 2）经 orgsync-data 到达
    let body = crate::sync::orgsync::build_orgsync_data_batch(
        org_id,
        "org:contacts@v1",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: serde_json::Value::Null,
            meta: DocMeta {
                vv: [("node-a".to_string(), 3)].into_iter().collect(),
                ts: 2000,
                node_id: Some("node-a".to_string()),
                tombstone: Some(true),
            },
            dseq: Some(4),
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
    // org dlog 有（接力）
    let org_entries =
        crate::sync::orgsync::org_dlog_entries_after(&s, org_id, "org:contacts", "1", 0).unwrap();
    assert_eq!(org_entries.len(), 1, "存量键墓碑登 org dlog");
    assert_eq!(org_entries[0].1, key);
    // 个人域 dlog 也有（pdsync 自设备同步）
    let personal_entries: Vec<_> = s
        .scan(&ScanOptions::prefix("dlog:entry:"))
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        personal_entries.iter().any(|(_, v)| v == key),
        "存量键墓碑同时登个人域 dlog"
    );
}

/// F5 防双写幂等：同一存量键墓碑再次（同 vv）到达 → did_apply=false，
/// 个人域 dlog 不重复补登（保持单条目）。
#[test]
fn orgsync_data_legacy_tombstone_does_not_double_log_personal() {
    let mut s = setup_org_and_collection();
    let online = std::collections::HashSet::new();
    let c = ctx("self", "peer-a", &online);
    let org_id = "org_0000000000000001";
    let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
    let decl = declare(
        &mut s,
        "org",
        crate::plugindata::DeclareInput {
            name: "org:contacts".to_string(),
            version: Some("1".to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::AllMembers),
            scope: Some(Scope::Sync),
            ..Default::default()
        },
        1000,
        Some(org_id),
    )
    .unwrap();
    s.put(&decl_key, &serde_json::to_string(&decl).unwrap())
        .unwrap();
    s.put(
        &format!("pmeta:{decl_key}"),
        &serde_json::to_string(&meta("node-a", 1)).unwrap(),
    )
    .unwrap();
    let key = "ct:org:org_0000000000000001:member-y";
    // 声明 pmeta（node-a:1）把 node-a 序号种子到 1，本地存量数据拿到序号 2
    crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
    // 远端墓碑 vv=3 领先本地 2 → 首达合入（登个人 dlog），同 vv 重放不重复登
    let tomb_meta = DocMeta {
        vv: [("node-a".to_string(), 3)].into_iter().collect(),
        ts: 2000,
        node_id: Some("node-a".to_string()),
        tombstone: Some(true),
    };
    let body = crate::sync::orgsync::build_orgsync_data_batch(
        org_id,
        "org:contacts@v1",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: serde_json::Value::Null,
            meta: tomb_meta.clone(),
            dseq: Some(4),
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
    // 同 vv 墓碑重放 → 不重复补登个人 dlog
    let body2 = crate::sync::orgsync::build_orgsync_data_batch(
        org_id,
        "org:contacts@v1",
        &[crate::sync::orgsync::OrgsyncRecord {
            key: key.to_string(),
            value: serde_json::Value::Null,
            meta: tomb_meta,
            dseq: Some(4),
        }],
        0,
        1,
    );
    handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
    let personal_entries: Vec<_> = s
        .scan(&ScanOptions::prefix("dlog:entry:"))
        .unwrap()
        .into_iter()
        .filter(|(_, v)| v == key)
        .collect();
    assert_eq!(personal_entries.len(), 1, "同 vv 重放不重复登个人 dlog");
}
