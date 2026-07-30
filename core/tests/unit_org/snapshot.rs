//! 组织同步快照构建/合并/stale 判定/入站归一化单测。

use serde_json::Value;

use spark_core::org::snapshot::*;
use spark_core::org::tx::{OrganizationTransactionRecord, OrganizationTransactionType};
use spark_core::org::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
    OrganizationSyncSection, OrganizationSyncState, OrganizationSyncVersions,
};

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn member(root: char, role: OrganizationRole, joined: i64) -> OrganizationMember {
    OrganizationMember {
        root_id: rid(root),
        role,
        joined_at: joined,
        added_by: rid('z'),
        node_info: None,
        ..Default::default()
    }
}

fn sample_record() -> OrganizationRecord {
    let mut record = OrganizationRecord {
        org_id: "org_0123456789abcdef".to_string(),
        name: "星火".to_string(),
        description: "desc".to_string(),
        avatar: String::new(),
        base_plugin_domain: Some("plugin:chat".to_string()),
        created_at: 1000,
        created_by: rid('a'),
        updated_at: 2000,
        members: vec![
            member('a', OrganizationRole::Admin, 1000),
            member('b', OrganizationRole::Member, 1500),
        ],
        sync: None,
        gateways: Vec::new(),
        org_address: None,
        is_public: false,
        extra: Default::default(),
    };
    record.set_recovery_secret("cd".repeat(32));
    record
}

fn versions(v: i64) -> OrganizationSyncVersions {
    OrganizationSyncVersions {
        summary_version: v,
        members_version: v,
        member_details_version: v,
        transactions_version: v,
    }
}

#[test]
fn versions_all_equal_updated_at() {
    let record = sample_record();
    let v = build_organization_sync_versions(&record, 1234);
    assert_eq!(v.summary_version, 2000);
    assert_eq!(v.members_version, 2000);
    assert_eq!(v.member_details_version, 2000);
    assert_eq!(v.transactions_version, 1234);
    let d = build_organization_sync_versions_default(&record);
    assert_eq!(d, versions(2000));
}

#[test]
fn build_snapshot_metadata_carries_recovery_secret() {
    let record = sample_record();
    let snapshot = build_organization_sync_snapshot(&record, &[]);
    assert_eq!(snapshot.summary.member_count, 2);
    assert_eq!(snapshot.summary.admin_count, 1);
    let metadata = snapshot.summary.metadata.as_ref().unwrap();
    assert_eq!(
        metadata.get("recoverySecret").and_then(Value::as_str),
        Some("cd".repeat(32).as_str())
    );
    // 保留键不进 metadata
    for key in ORGANIZATION_SYNC_RESERVED_KEYS {
        assert!(!metadata.contains_key(key), "reserved key {key} leaked");
    }
    // 版本塌缩：空事务 → transactionsVersion = updatedAt
    assert_eq!(snapshot.sync, versions(2000));
}

#[test]
fn gateways_ride_snapshot_summary_and_merge_fallback() {
    // 构建：gateways 作为 summary 显式字段传播，不进 metadata
    let mut record = sample_record();
    record.gateways = vec![rid('a'), rid('b')];
    let snapshot = build_organization_sync_snapshot(&record, &[]);
    assert_eq!(
        snapshot.summary.gateways.as_deref(),
        Some([rid('a'), rid('b')].as_slice())
    );
    let metadata = snapshot.summary.metadata.as_ref().unwrap();
    assert!(!metadata.contains_key("gateways"), "保留键不进 metadata");

    // 合并：incoming 显式携带 → 以 incoming 为准
    let merged = merge_organization_sync_snapshot(None, &snapshot, 1);
    assert_eq!(merged.gateways, vec![rid('a'), rid('b')]);

    // incoming 缺省 → 保留 existing
    let mut existing = sample_record();
    existing.gateways = vec![rid('c')];
    let bare_snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    assert!(bare_snapshot.summary.gateways.is_none());
    let merged = merge_organization_sync_snapshot(Some(&existing), &bare_snapshot, 1);
    assert_eq!(merged.gateways, vec![rid('c')]);

    // 恶意 metadata 携带保留键 gateways → 合并后被剔除
    let mut poisoned = bare_snapshot.clone();
    let mut metadata = serde_json::Map::new();
    metadata.insert("gateways".to_string(), Value::from(vec![rid('z')]));
    poisoned.summary.metadata = Some(metadata);
    let merged = merge_organization_sync_snapshot(Some(&existing), &poisoned, 1);
    assert_eq!(merged.gateways, vec![rid('c')], "metadata 不得注入保留键");
    assert!(!merged.extra.contains_key("gateways"));
}

#[test]
fn avatar_rides_snapshot_summary_and_merge_fallback() {
    let logo = "data:image/png;base64,iVBORw0KGgo=";

    // 构建：avatar 作为 summary 显式字段传播（恒 Some，空串也显式携带），不进 metadata
    let mut record = sample_record();
    record.avatar = logo.to_string();
    let snapshot = build_organization_sync_snapshot(&record, &[]);
    assert_eq!(snapshot.summary.avatar.as_deref(), Some(logo));
    let metadata = snapshot.summary.metadata.as_ref().unwrap();
    assert!(!metadata.contains_key("avatar"), "保留键不进 metadata");

    // 合并：incoming 显式携带 → 以 incoming 为准
    let merged = merge_organization_sync_snapshot(None, &snapshot, 1);
    assert_eq!(merged.avatar, logo);

    // incoming 缺省（旧版发送方不携带该字段）→ 保留 existing，不得抹掉本地 logo
    let mut existing = sample_record();
    existing.avatar = logo.to_string();
    let mut bare_snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    bare_snapshot.summary.avatar = None; // 模拟旧线形缺省
    let merged = merge_organization_sync_snapshot(Some(&existing), &bare_snapshot, 1);
    assert_eq!(merged.avatar, logo);

    // incoming 显式空串 → 清除 logo（"清除"必须能跨节点传播）
    let cleared_snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    assert_eq!(cleared_snapshot.summary.avatar.as_deref(), Some(""));
    let merged = merge_organization_sync_snapshot(Some(&existing), &cleared_snapshot, 1);
    assert_eq!(merged.avatar, "");

    // 恶意 metadata 携带保留键 avatar → 合并后被剔除
    let mut poisoned = cleared_snapshot.clone();
    let mut metadata = serde_json::Map::new();
    metadata.insert("avatar".to_string(), Value::from(logo));
    poisoned.summary.metadata = Some(metadata);
    let merged = merge_organization_sync_snapshot(Some(&existing), &poisoned, 1);
    assert_eq!(merged.avatar, "", "metadata 不得注入保留键");
    assert!(!merged.extra.contains_key("avatar"));
}

#[test]
fn build_snapshot_transactions_version_from_first_tx() {
    let record = sample_record();
    let txs = vec![OrganizationTransactionRecord {
        tx_id: "t1".to_string(),
        org_id: record.org_id.clone(),
        type_: OrganizationTransactionType::MemberAdd,
        created_at: 7777,
        actor_root_id: rid('a'),
        target_root_id: None,
        summary: "s".to_string(),
        payload: None,
    }];
    let snapshot = build_organization_sync_snapshot(&record, &txs);
    assert_eq!(snapshot.sync.transactions_version, 7777);
    assert_eq!(snapshot.transactions.len(), 1);
}

#[test]
fn stale_rules() {
    let local = versions(100);
    // local 缺失 → stale
    assert!(is_organization_sync_stale(None, &local));
    // 完全等价 → 不 stale
    assert!(!is_organization_sync_stale(Some(&local), &versions(100)));
    // 任一字段严格更大 → stale
    let mut incoming = versions(100);
    incoming.transactions_version = 101;
    assert!(is_organization_sync_stale(Some(&local), &incoming));
    // 双向可同时为 true（分叉）
    let mut fork_a = versions(100);
    fork_a.summary_version = 200;
    let mut fork_b = versions(100);
    fork_b.members_version = 200;
    assert!(is_organization_sync_stale(Some(&fork_a), &fork_b));
    assert!(is_organization_sync_stale(Some(&fork_b), &fork_a));
    // 全字段落后 → 不 stale
    assert!(!is_organization_sync_stale(
        Some(&versions(200)),
        &versions(100)
    ));
}

#[test]
fn merge_into_empty() {
    let record = sample_record();
    let snapshot = build_organization_sync_snapshot(&record, &[]);
    let merged = merge_organization_sync_snapshot(None, &snapshot, 5555);
    assert_eq!(merged.org_id, record.org_id);
    assert_eq!(merged.name, "星火");
    assert_eq!(merged.members.len(), 2);
    assert_eq!(merged.updated_at, 2000);
    let sync = merged.sync.as_ref().unwrap();
    assert_eq!(sync.versions, versions(2000));
    assert_eq!(sync.last_synced_at, 5555);
    assert_eq!(
        sync.sections,
        vec![
            OrganizationSyncSection::Summary,
            OrganizationSyncSection::Members,
            OrganizationSyncSection::MemberDetails,
            OrganizationSyncSection::Transactions,
        ]
    );
    // recoverySecret 经 metadata 落到 merged.extra
    assert_eq!(merged.recovery_secret(), Some("cd".repeat(32).as_str()));
}

#[test]
fn merge_member_nodeinfo_fallback_and_order() {
    let mut existing = sample_record();
    existing.members[1].node_info = Some(OrganizationNodeInfo {
        peer_id: Some("peer-b-123".to_string()),
        addresses: vec!["/ip4/9.9.9.9/tcp/1".to_string()],
    });
    existing.updated_at = 3000; // 本地 updatedAt 更大 → max 保留

    // incoming：b 不带 nodeInfo（应保留 existing），新成员 c，且 a 角色被覆盖
    let mut incoming_record = sample_record();
    incoming_record.members = vec![
        {
            let mut m = member('a', OrganizationRole::Member, 1000);
            m.added_by = rid('y');
            m
        },
        member('b', OrganizationRole::Member, 1500),
        member('c', OrganizationRole::Member, 2500),
    ];
    let snapshot = build_organization_sync_snapshot(&incoming_record, &[]);

    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 9999);
    // 成员顺序：existing 原位（a, b），新成员 c 追加
    let order: Vec<char> = merged
        .members
        .iter()
        .map(|m| m.root_id.chars().next().unwrap())
        .collect();
    assert_eq!(order, vec!['a', 'b', 'c']);
    // a 的字段被 incoming 覆盖
    assert_eq!(merged.members[0].role, OrganizationRole::Member);
    assert_eq!(merged.members[0].added_by, rid('y'));
    // b 的 nodeInfo 保留 existing 值
    assert_eq!(
        merged.members[1]
            .node_info
            .as_ref()
            .unwrap()
            .peer_id
            .as_deref(),
        Some("peer-b-123")
    );
    // updatedAt = max(3000, 2000)
    assert_eq!(merged.updated_at, 3000);
}

#[test]
fn merge_base_plugin_domain_fallback() {
    let mut existing = sample_record();
    existing.base_plugin_domain = Some("plugin:keep".to_string());
    let mut snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    snapshot.summary.base_plugin_domain = None;
    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 1);
    assert_eq!(merged.base_plugin_domain.as_deref(), Some("plugin:keep"));
    // existing 缺失时则为 None
    let merged = merge_organization_sync_snapshot(None, &snapshot, 1);
    assert_eq!(merged.base_plugin_domain, None);
}

#[test]
fn merge_dynamic_metadata_overrides_and_strips_reserved() {
    let mut existing = sample_record();
    existing
        .extra
        .insert("customKey".to_string(), Value::from("old"));
    let mut snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    let mut metadata = serde_json::Map::new();
    metadata.insert("customKey".to_string(), Value::from("new"));
    metadata.insert("anotherKey".to_string(), Value::from(42));
    // 恶意/异常 metadata 携带保留键 → 合并后必须被剔除
    metadata.insert("name".to_string(), Value::from("hijack"));
    snapshot.summary.metadata = Some(metadata);

    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 1);
    assert_eq!(
        merged.extra.get("customKey").and_then(Value::as_str),
        Some("new")
    );
    assert_eq!(
        merged.extra.get("anotherKey").cloned(),
        Some(Value::from(42))
    );
    assert_eq!(
        merged.name, snapshot.summary.name,
        "保留键不得经 metadata 注入"
    );
    assert!(!merged.extra.contains_key("name"));
}

#[test]
fn normalize_snapshot_shape_passthrough() {
    let snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    let value = serde_json::to_value(&snapshot).unwrap();
    let normalized = normalize_incoming_snapshot(&value).unwrap();
    assert_eq!(normalized, snapshot);
}

#[test]
fn normalize_raw_record_collapses_versions() {
    // 原始记录线形（org-share 推送路径）：sync 带 sections/lastSyncedAt
    let mut record = sample_record();
    record.sync = Some(OrganizationSyncState {
        versions: OrganizationSyncVersions {
            summary_version: 2000,
            members_version: 2000,
            member_details_version: 2000,
            transactions_version: 8888, // 独立事务版本——重建后丢失
        },
        sections: pick_sync_sections_by_priority(),
        last_synced_at: 4321,
    });
    let value = serde_json::to_value(&record).unwrap();
    let normalized = normalize_incoming_snapshot(&value).unwrap();
    // 版本塌缩：四字段全部 = updatedAt（spec §4.4 线形兼容行为）
    assert_eq!(normalized.sync, versions(2000));
    assert_eq!(normalized.summary.name, "星火");
    assert_eq!(normalized.members.len(), 2);
    // recoverySecret 经 record extra → metadata 保留
    assert_eq!(
        normalized
            .summary
            .metadata
            .as_ref()
            .unwrap()
            .get("recoverySecret")
            .and_then(Value::as_str),
        Some("cd".repeat(32).as_str())
    );
}

#[test]
fn merge_inbound_snapshot_cannot_inject_org_root_secret() {
    // 本机已持有根私钥密文（组织创建时生成，org.md §15 不出本机）
    let mut existing = sample_record();
    existing.set_org_root_secret("local-sealed-root-secret");

    // 恶意对端构造带 orgRootSecret 的快照 metadata，企图覆盖本机根私钥
    // （orgRootSecret 不在保留键表内，retain 剔不掉，必须靠插入处显式跳过）
    let mut snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
    let mut metadata = snapshot.summary.metadata.take().unwrap_or_default();
    metadata.insert("orgRootSecret".to_string(), Value::from("evil-injected"));
    metadata.insert("orgSecret".to_string(), Value::from("ab".repeat(32)));
    snapshot.summary.metadata = Some(metadata);

    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 1);
    assert_eq!(
        merged.org_root_secret(),
        Some("local-sealed-root-secret"),
        "入站快照不得覆盖本机根私钥密文"
    );
    // orgSecret 是**有意**随快照同步给成员的（org.md §13）——不得被误伤
    assert_eq!(merged.org_secret(), Some("ab".repeat(32).as_str()));

    // 本机尚未持有根私钥时同样不得接受注入
    let merged = merge_organization_sync_snapshot(None, &snapshot, 1);
    assert!(
        merged.org_root_secret().is_none(),
        "空记录合并同样不得接受 orgRootSecret 注入"
    );
}

#[test]
fn normalize_rejects_garbage() {
    assert!(normalize_incoming_snapshot(&Value::Null).is_err());
    assert!(normalize_incoming_snapshot(&serde_json::json!({"foo": 1})).is_err());
    assert!(normalize_incoming_snapshot(&serde_json::json!("str")).is_err());
}

// ---------------------------------------------------------------------------
// 成员身份字段（M1 显式墓碑）：快照流动/清除传播/入站校验/旧线形兼容
// ---------------------------------------------------------------------------

const ORG_AVATAR: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";

#[test]
fn snapshot_member_identity_fields_flow() {
    let mut record = sample_record();
    record.members[0].nickname = Some("管理员小A".to_string());
    record.members[0].signature = Some("保持热爱".to_string());
    record.members[0].use_personal_identity = Some(true);

    // 构建：members 段带身份字段（M1：未设置的身份字符串字段上线为 ""）
    let snapshot = build_organization_sync_snapshot(&record, &[]);
    assert_eq!(snapshot.members[0].nickname.as_deref(), Some("管理员小A"));
    assert_eq!(snapshot.members[0].use_personal_identity, Some(true));
    assert_eq!(snapshot.members[0].avatar.as_deref(), Some(""), "未设置 → 显式空串上线");
    assert_eq!(snapshot.members[1].nickname.as_deref(), Some(""));

    // 合并：incoming 覆盖、键缺失（None）保留 existing
    let existing = merge_organization_sync_snapshot(None, &snapshot, 5000);
    let mut incoming = build_organization_sync_snapshot(&record, &[]);
    incoming.members[0].nickname = None; // 旧对端未携带
    incoming.members[0].signature = Some("奔赴山海".to_string()); // 覆盖
    incoming.members[0].use_personal_identity = None; // 旧对端未携带
    let merged = merge_organization_sync_snapshot(Some(&existing), &incoming, 6000);
    let member = &merged.members[0];
    assert_eq!(
        member.nickname.as_deref(),
        Some("管理员小A"),
        "incoming 为 None（键缺失）时保留 existing"
    );
    assert_eq!(
        member.signature.as_deref(),
        Some("奔赴山海"),
        "incoming 携带时覆盖"
    );
    assert_eq!(
        member.use_personal_identity,
        Some(true),
        "incoming 未携带 usePersonalIdentity 时保留 existing"
    );
}

#[test]
fn snapshot_identity_clear_propagates_via_tombstone() {
    // M1 墓碑：A 清除身份字段 → 快照携带 "" → B 合并后为 None（不被本地旧值复活）
    let mut record_a = sample_record();
    record_a.members[0].nickname = Some("旧昵称".to_string());
    record_a.members[0].avatar = Some(ORG_AVATAR.to_string());
    record_a.members[0].signature = Some("保持热爱".to_string());
    record_a.members[0].gender = Some("女".to_string());
    record_a.members[0].region = Some("杭州".to_string());
    record_a.members[0].use_personal_identity = Some(true);
    let existing = merge_organization_sync_snapshot(
        None,
        &build_organization_sync_snapshot(&record_a, &[]),
        5000,
    );
    assert_eq!(existing.members[0].nickname.as_deref(), Some("旧昵称"));

    // A 清除全部身份字段并关闭 usePersonalIdentity：本地记录为 None / Some(false)
    let mut record_cleared = record_a.clone();
    record_cleared.members[0].nickname = None;
    record_cleared.members[0].avatar = None;
    record_cleared.members[0].signature = None;
    record_cleared.members[0].gender = None;
    record_cleared.members[0].region = None;
    record_cleared.members[0].use_personal_identity = Some(false);
    let snapshot = build_organization_sync_snapshot(&record_cleared, &[]);
    // 上线线形：字符串字段为显式 ""，usePersonalIdentity 为 Some(false)
    assert_eq!(snapshot.members[0].nickname.as_deref(), Some(""));
    assert_eq!(snapshot.members[0].use_personal_identity, Some(false));

    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 6000);
    let member = &merged.members[0];
    assert_eq!(member.nickname, None, "清除可传播：'' → None");
    assert_eq!(member.avatar, None);
    assert_eq!(member.signature, None);
    assert_eq!(member.gender, None);
    assert_eq!(member.region, None);
    assert_eq!(
        member.use_personal_identity,
        Some(false),
        "true→false 可传播"
    );
}

#[test]
fn snapshot_merge_rejects_invalid_member_avatar_and_keeps_existing() {
    // 入站校验（R1 review m3）：快照 serve 方是组织成员（内部威胁面）——
    // 非 data:image / 超 200KB 的成员 avatar 不采用（保留 existing），
    // 与本地写路径 validate_avatar 同口径
    let mut record = sample_record();
    record.members[0].avatar = Some(ORG_AVATAR.to_string());
    let existing = merge_organization_sync_snapshot(
        None,
        &build_organization_sync_snapshot(&record, &[]),
        5000,
    );

    let mut snapshot = build_organization_sync_snapshot(&record, &[]);
    snapshot.members[0].avatar = Some("data:text/html;base64,PHNjcmlwdA==".to_string());
    snapshot.members[1].avatar = Some(format!("data:image/png;base64,{}", "A".repeat(300_000)));
    let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 6000);
    assert_eq!(
        merged.members[0].avatar.as_deref(),
        Some(ORG_AVATAR),
        "成员 avatar 非法值忽略，保留 existing"
    );
    assert_eq!(
        merged.members[1].avatar, None,
        "超大 avatar 忽略（existing 本就无）"
    );
}

#[test]
fn legacy_record_and_snapshot_without_identity_fields() {
    // 旧记录：无 avatar、成员无身份字段 → serde default 兼容
    let record: OrganizationRecord = serde_json::from_value(serde_json::json!({
        "orgId": "org_0123456789abcdef",
        "name": "星火",
        "createdAt": 1000,
        "createdBy": rid('a'),
        "updatedAt": 2000,
        "members": [{
            "rootId": rid('a'),
            "role": "admin",
            "joinedAt": 1000,
            "addedBy": rid('a')
        }]
    }))
    .unwrap();
    assert_eq!(record.avatar, "", "旧记录缺省 avatar 为空串");
    assert_eq!(record.members[0].nickname, None);
    assert_eq!(record.members[0].use_personal_identity, None);

    // 旧对端快照：summary 无 avatar、members 无身份字段（线形兼容）——
    // 反序列化为 None 后合并，保留 existing 的身份字段
    let mut with_identity = record.clone();
    with_identity.avatar = ORG_AVATAR.to_string();
    with_identity.members[0].nickname = Some("旧昵称".to_string());
    with_identity.members[0].use_personal_identity = Some(true);

    let mut value = serde_json::to_value(build_organization_sync_snapshot(&record, &[])).unwrap();
    value["summary"].as_object_mut().unwrap().remove("avatar");
    for member in value["members"].as_array_mut().unwrap() {
        let obj = member.as_object_mut().unwrap();
        for key in [
            "nickname",
            "avatar",
            "signature",
            "gender",
            "region",
            "usePersonalIdentity",
        ] {
            obj.remove(key);
        }
    }
    let legacy_snapshot: OrganizationSyncSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(legacy_snapshot.summary.avatar, None);
    assert_eq!(legacy_snapshot.members[0].nickname, None);
    assert_eq!(legacy_snapshot.members[0].use_personal_identity, None);

    let merged = merge_organization_sync_snapshot(Some(&with_identity), &legacy_snapshot, 7000);
    assert_eq!(merged.avatar, ORG_AVATAR, "缺省 avatar 保留 existing");
    assert_eq!(merged.members[0].nickname.as_deref(), Some("旧昵称"));
    assert_eq!(merged.members[0].use_personal_identity, Some(true));
}
