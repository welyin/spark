//! 成员增删与视图：`addMember`（新增/重复添加更新 nodeInfo）、`removeMember`
//! admin 守卫、`listMine` 过滤排序、`sync_recipients` 筛选。

use super::*;

use spark_core::org::OrgError;
use spark_core::org::service::OrgIdentityPatch;
use spark_core::org::tx::OrganizationTransactionType;
use spark_core::org::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
};

/// pdsync 变体：成员变更落库写 org:meta pmeta（vv 递增）。
#[test]
fn add_member_pdsync_writes_pmeta() {
    use spark_core::sync::get_personal_meta;

    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    // setup_org 走裸 save_record（无 pmeta）；首次 pdsync 写入从 1 起计
    OrganizationService::add_member_pdsync(
        &mut storage,
        &record.org_id,
        &member_id,
        None,
        &admin,
        NOW + 1,
        "node-a",
    )
    .unwrap();
    let key = format!("org:meta:{}", record.org_id);
    let meta = get_personal_meta(&storage, &key).unwrap().unwrap();
    assert_eq!(meta.vv.get("node-a"), Some(&1));

    OrganizationService::remove_member_pdsync(
        &mut storage,
        &record.org_id,
        &member_id,
        &admin,
        NOW + 2,
        "node-a",
    )
    .unwrap();
    let meta = get_personal_meta(&storage, &key).unwrap().unwrap();
    assert_eq!(meta.vv.get("node-a"), Some(&2));
}

#[test]
fn add_member_new_and_repeat_update() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    let node = OrganizationNodeInfo {
        peer_id: Some("12D3KooWMember".to_string()),
        addresses: vec!["/ip4/1.1.1.1/tcp/1".to_string()],
    };
    // 新成员：role 固定 member
    let updated = OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member_id,
        Some(&node),
        &admin,
        NOW + 1,
    )
    .unwrap();
    assert_eq!(updated.members.len(), 2);
    let m = updated.find_member(&member_id).unwrap();
    assert_eq!(m.role, OrganizationRole::Member);
    assert_eq!(m.added_by, admin);
    assert_eq!(m.joined_at, NOW + 1);
    assert_eq!(updated.updated_at, NOW + 1);
    let sync = updated.sync.as_ref().unwrap();
    assert_eq!(sync.versions.members_version, NOW + 1);

    // 重复添加不带 nodeInfo → 保留原值，记 member-update
    let updated2 = OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member_id,
        None,
        &admin,
        NOW + 2,
    )
    .unwrap();
    let m2 = updated2.find_member(&member_id).unwrap();
    assert_eq!(
        m2.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some("12D3KooWMember")
    );
    assert_eq!(updated2.members.len(), 2);

    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 20).unwrap();
    let types: Vec<_> = txs.iter().map(|t| t.type_).collect();
    assert_eq!(
        types,
        vec![
            OrganizationTransactionType::MemberUpdate,
            OrganizationTransactionType::MemberAdd,
            OrganizationTransactionType::Create,
        ]
    );
    assert_eq!(txs[0].summary, format!("更新成员节点信息 {member_id}"));
    assert_eq!(txs[1].summary, format!("添加成员 {member_id}"));
    // payload 的 nodeInfo 键：未提供时缺省
    assert!(!txs[0].payload.as_ref().unwrap().contains_key("nodeInfo"));
    assert!(txs[1].payload.as_ref().unwrap().contains_key("nodeInfo"));
}

#[test]
fn add_member_requires_admin_and_valid_root() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    // 非 admin
    assert!(matches!(
        OrganizationService::add_member(
            &mut storage,
            &record.org_id,
            &member_id,
            None,
            &rid('x'),
            NOW
        ),
        Err(OrgError::AdminRequired)
    ));
    // rootId 非法
    assert!(matches!(
        OrganizationService::add_member(&mut storage, &record.org_id, "zz", None, &admin, NOW),
        Err(OrgError::InvalidMemberRootId)
    ));
    // 组织不存在
    assert!(matches!(
        OrganizationService::add_member(&mut storage, "org_nope", &member_id, None, &admin, NOW),
        Err(OrgError::OrganizationNotFound)
    ));
}

#[test]
fn remove_member_admin_guard() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    // 移除唯一 admin → 拒绝
    assert!(matches!(
        OrganizationService::remove_member(&mut storage, &record.org_id, &admin, &admin, NOW),
        Err(OrgError::MustKeepAdmin)
    ));
    // 移除不存在的成员（rootId 合法但不在组织中）
    assert!(matches!(
        OrganizationService::remove_member(&mut storage, &record.org_id, &rid('f'), &admin, NOW),
        Err(OrgError::MemberNotFound)
    ));
    // 正常移除
    let member_id = root_id_of(MNEMONIC2);
    OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member_id,
        None,
        &admin,
        NOW + 1,
    )
    .unwrap();
    let updated = OrganizationService::remove_member(
        &mut storage,
        &record.org_id,
        &member_id,
        &admin,
        NOW + 2,
    )
    .unwrap();
    assert_eq!(updated.members.len(), 1);
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].type_, OrganizationTransactionType::MemberRemove);
    assert_eq!(txs[0].payload.as_ref().unwrap()["removedRole"], "member");
}

#[test]
fn list_mine_filters_and_sorts() {
    let mut storage = MemoryStorage::new();
    let (admin, org1) = setup_org(&mut storage);
    // 第二个组织，updatedAt 更大
    let org2 = OrganizationService::create_organization(&mut storage, &input(), &admin, NOW + 100)
        .unwrap();
    // 第三个组织：admin 不是成员（手工构造）
    let mut other = OrganizationRecord {
        org_id: "org_other".to_string(),
        name: "别人".to_string(),
        created_by: rid('z'),
        updated_at: NOW + 200,
        ..Default::default()
    };
    other.members.push(OrganizationMember {
        root_id: rid('z'),
        role: OrganizationRole::Admin,
        joined_at: NOW,
        added_by: rid('z'),
        node_info: None,
        ..Default::default()
    });
    OrganizationService::save_record(&mut storage, &other).unwrap();

    let mine = OrganizationService::list_mine(&storage, &admin).unwrap();
    let ids: Vec<&str> = mine.iter().map(|v| v.record.org_id.as_str()).collect();
    assert_eq!(ids, vec![org2.org_id.as_str(), org1.org_id.as_str()]);
    assert!(mine[0].is_current_user_admin);
    assert_eq!(mine[0].current_user_role, Some(OrganizationRole::Admin));
    assert_eq!(mine[0].member_count, 1);
    assert_eq!(mine[0].admin_count, 1);
}

#[test]
fn sync_recipients_filters() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let with_peer = root_id_of(MNEMONIC2);
    let node = OrganizationNodeInfo {
        peer_id: Some("12D3KooWMember".to_string()),
        addresses: vec![],
    };
    OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &with_peer,
        Some(&node),
        &admin,
        NOW,
    )
    .unwrap();
    OrganizationService::add_member(&mut storage, &record.org_id, &rid('e'), None, &admin, NOW)
        .unwrap();
    let record = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    let recipients = OrganizationService::sync_recipients(&record, &admin);
    assert_eq!(recipients.len(), 1, "排除 actor 与无 nodeInfo 成员");
    assert_eq!(recipients[0].root_id, with_peer);
}

// ------------------------------------------------------------------
// updateMyIdentity：成员身份 patch（仅本人可改、校验、幂等、清除语义）
// ------------------------------------------------------------------

const IDENTITY_AVATAR: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";

fn setup_two_member_org(storage: &mut MemoryStorage) -> (String, String, OrganizationRecord) {
    let (admin, record) = setup_org(storage);
    let member_id = root_id_of(MNEMONIC2);
    OrganizationService::add_member(storage, &record.org_id, &member_id, None, &admin, NOW + 1)
        .unwrap();
    (admin, member_id, record)
}

#[test]
fn update_my_identity_patch_and_merge_semantics() {
    let mut storage = MemoryStorage::new();
    let (admin, member_id, record) = setup_two_member_org(&mut storage);

    // 非成员不可改（他人记录不可改：非成员一律 MemberNotFound）
    let patch = OrgIdentityPatch {
        nickname: Some("他人".to_string()),
        ..Default::default()
    };
    assert!(matches!(
        OrganizationService::update_my_identity(&mut storage, &record.org_id, &patch, &rid('q'), NOW + 2),
        Err(OrgError::MemberNotFound)
    ));

    // 普通成员改自己（无需 admin）：全字段设置，昵称 trim
    let patch = OrgIdentityPatch {
        nickname: Some("  小火  ".to_string()),
        avatar: Some(Some(IDENTITY_AVATAR.to_string())),
        gender: Some("女".to_string()),
        region: Some("杭州".to_string()),
        signature: Some("保持热爱".to_string()),
        use_personal_identity: Some(true),
    };
    let updated = OrganizationService::update_my_identity(
        &mut storage,
        &record.org_id,
        &patch,
        &member_id,
        NOW + 3,
    )
    .unwrap();
    let member = updated.find_member(&member_id).unwrap();
    assert_eq!(member.nickname.as_deref(), Some("小火"));
    assert_eq!(member.avatar.as_deref(), Some(IDENTITY_AVATAR));
    assert_eq!(member.gender.as_deref(), Some("女"));
    assert_eq!(member.region.as_deref(), Some("杭州"));
    assert_eq!(member.signature.as_deref(), Some("保持热爱"));
    assert_eq!(member.use_personal_identity, Some(true));
    assert_eq!(updated.updated_at, NOW + 3);
    // 他人（admin）记录不受影响
    assert_eq!(updated.find_member(&admin).unwrap().nickname, None);
    // 记事务（对齐 update_info/members 写法）
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].type_, OrganizationTransactionType::MemberUpdate);
    assert_eq!(txs[0].target_root_id.as_deref(), Some(member_id.as_str()));
    assert_eq!(txs[0].summary, "更新组织身份信息");
    // m1：avatar/gender/region/signature 纳入审计但只记摘要（设置 → 长度）
    let payload = txs[0].payload.as_ref().unwrap();
    assert_eq!(payload["nickname"], serde_json::json!("  小火  "));
    assert_eq!(
        payload["avatar"],
        serde_json::json!(IDENTITY_AVATAR.len() as i64),
        "审计只记长度，不落 data URL"
    );
    assert_eq!(payload["gender"], serde_json::json!("女".len() as i64));
    assert_eq!(payload["region"], serde_json::json!("杭州".len() as i64));
    assert_eq!(payload["signature"], serde_json::json!("保持热爱".len() as i64));
    assert_eq!(payload["usePersonalIdentity"], serde_json::json!(true));

    // 幂等：同值重复设置不 bump 版本
    let same = OrganizationService::update_my_identity(
        &mut storage,
        &record.org_id,
        &patch,
        &member_id,
        NOW + 99,
    )
    .unwrap();
    assert_eq!(same.updated_at, NOW + 3);

    // None 不变 / Some(None) 清除 avatar / Some("") 清除签名
    let clear = OrgIdentityPatch {
        avatar: Some(None),
        signature: Some("   ".to_string()),
        ..Default::default()
    };
    let cleared = OrganizationService::update_my_identity(
        &mut storage,
        &record.org_id,
        &clear,
        &member_id,
        NOW + 4,
    )
    .unwrap();
    let member = cleared.find_member(&member_id).unwrap();
    assert_eq!(member.avatar, None, "Some(None) 清除头像");
    assert_eq!(member.signature, None, "Some(空白) 清除签名");
    assert_eq!(member.nickname.as_deref(), Some("小火"), "None 不变");
    assert_eq!(member.gender.as_deref(), Some("女"), "None 不变");
    assert_eq!(member.use_personal_identity, Some(true), "None 不变");
    // m1：清除的审计摘要为 false；未变更字段为 Null
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    let payload = txs[0].payload.as_ref().unwrap();
    assert_eq!(payload["avatar"], serde_json::json!(false), "清除 → false");
    assert_eq!(payload["signature"], serde_json::json!(false), "空白清除 → false");
    assert_eq!(payload["gender"], serde_json::Value::Null, "未变更 → Null");
}

#[test]
fn update_my_identity_rejects_invalid_fields() {
    let mut storage = MemoryStorage::new();
    let (_, member_id, record) = setup_two_member_org(&mut storage);
    let mut reject = |patch: OrgIdentityPatch| {
        assert!(
            matches!(
                OrganizationService::update_my_identity(
                    &mut storage,
                    &record.org_id,
                    &patch,
                    &member_id,
                    NOW + 2
                ),
                Err(OrgError::InvalidIdentityField(_))
            ),
            "应拒绝非法字段"
        );
    };
    // 昵称 > 24 字符
    reject(OrgIdentityPatch {
        nickname: Some("啊".repeat(25)),
        ..Default::default()
    });
    // 头像非 data:image/ 前缀
    reject(OrgIdentityPatch {
        avatar: Some(Some("https://example.com/a.png".to_string())),
        ..Default::default()
    });
    // 性别 > 16 / 地区 > 64 / 签名 > 128 字符
    reject(OrgIdentityPatch {
        gender: Some("x".repeat(17)),
        ..Default::default()
    });
    reject(OrgIdentityPatch {
        region: Some("x".repeat(65)),
        ..Default::default()
    });
    reject(OrgIdentityPatch {
        signature: Some("x".repeat(129)),
        ..Default::default()
    });
}
