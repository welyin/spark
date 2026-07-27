//! 成员增删与视图：`addMember`（新增/重复添加更新 nodeInfo）、`removeMember`
//! admin 守卫、`listMine` 过滤排序、`sync_recipients` 筛选。

use super::*;

use spark_core::org::OrgError;
use spark_core::org::tx::OrganizationTransactionType;
use spark_core::org::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
};

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
        extra: Default::default(),
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
