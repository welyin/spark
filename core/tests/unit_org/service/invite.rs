//! 邀请码链路：`createOrgInvite`（admin 校验/地址必填）、`prepareAcceptInvite`
//! （拒绝自邀）、`checkInviteAccepted`（拉取前后的加入确认）。

use super::*;

use spark_core::org::OrgError;

#[test]
fn invite_create_prepare_and_check() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    // 非 admin 不能生成
    assert!(matches!(
        OrganizationService::create_org_invite(&storage, &record.org_id, &rid('x'), None, &[], NOW),
        Err(OrgError::AdminRequired)
    ));
    // 无地址无 peerId
    assert!(matches!(
        OrganizationService::create_org_invite(
            &storage,
            &record.org_id,
            &admin,
            Some("  "),
            &[" ".to_string()],
            NOW
        ),
        Err(OrgError::NetworkUnavailable)
    ));
    // 正常生成
    let created = OrganizationService::create_org_invite(
        &storage,
        &record.org_id,
        &admin,
        Some("12D3KooWAdmin"),
        &[" /ip4/1.2.3.4/tcp/15002/ws ".to_string()],
        NOW,
    )
    .unwrap();
    assert_eq!(created.org_id, record.org_id);
    assert_eq!(created.org_name, "星火 组织");

    // 自己接受自己的邀请 → 拒绝
    assert!(matches!(
        OrganizationService::prepare_accept_invite(&created.invite, &admin, NOW),
        Err(OrgError::SelfInvite)
    ));
    // 他人接受：decode 通过
    let member_id = root_id_of(MNEMONIC2);
    let payload =
        OrganizationService::prepare_accept_invite(&created.invite, &member_id, NOW).unwrap();
    assert_eq!(payload.inviter.root_id, admin);

    // 拉取前确认 → 未加入
    assert!(matches!(
        OrganizationService::check_invite_accepted(&storage, &record.org_id, &member_id),
        Err(OrgError::NotJoined)
    ));
    // 模拟拉取成功（成员已在记录中）
    OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member_id,
        None,
        &admin,
        NOW + 1,
    )
    .unwrap();
    let accepted =
        OrganizationService::check_invite_accepted(&storage, &record.org_id, &member_id).unwrap();
    assert_eq!(accepted.org_id, record.org_id);
    assert_eq!(accepted.member_count, 2);
    // 不存在的组织
    assert!(matches!(
        OrganizationService::check_invite_accepted(&storage, "org_nope", &member_id),
        Err(OrgError::NotJoined)
    ));
}
