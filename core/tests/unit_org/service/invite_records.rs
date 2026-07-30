//! 组织邀请记录 CRUD：put/get/list/mark_status 与 (orgId, peer) 单条幂等语义。

use super::*;

use spark_core::org::{OrgInviteDirection, OrgInviteRecord, OrgInviteStatus};

fn record(id: &str, org_id: &str, peer: &str, direction: OrgInviteDirection) -> OrgInviteRecord {
    OrgInviteRecord {
        id: id.to_string(),
        org_id: org_id.to_string(),
        org_name: "星火组织".to_string(),
        org_avatar: None,
        peer_root_id: peer.to_string(),
        peer_nickname: "对方".to_string(),
        direction,
        status: OrgInviteStatus::Pending,
        invite_code: (direction == OrgInviteDirection::Incoming).then(|| "code-1".to_string()),
        created_at: NOW,
        updated_at: NOW,
    }
}

#[test]
fn invite_record_put_get_and_list() {
    let mut storage = MemoryStorage::new();
    let peer_a = rid('a');
    let peer_b = rid('b');

    let outgoing = record("inv-1", "org_aaaabbbbccccdddd", &peer_a, OrgInviteDirection::Outgoing);
    let incoming = record("inv-2", "org_aaaabbbbccccdddd", &peer_b, OrgInviteDirection::Incoming);
    let other_org = record("inv-3", "org_eeeeffff00001111", &peer_a, OrgInviteDirection::Outgoing);
    for r in [&outgoing, &incoming, &other_org] {
        OrganizationService::put_invite_record(&mut storage, r).unwrap();
    }

    // 按键直取（两个 direction 各一条）
    let got = OrganizationService::get_outgoing_invite(&storage, "org_aaaabbbbccccdddd", &peer_a)
        .unwrap()
        .expect("出站记录存在");
    assert_eq!(got.id, "inv-1");
    assert!(got.invite_code.is_none(), "出站记录不存邀请码");
    let got_in =
        OrganizationService::get_incoming_invite(&storage, "org_aaaabbbbccccdddd", &peer_b)
            .unwrap()
            .expect("入站记录存在");
    assert_eq!(got_in.invite_code.as_deref(), Some("code-1"));
    assert!(
        OrganizationService::get_outgoing_invite(&storage, "org_aaaabbbbccccdddd", &peer_b)
            .unwrap()
            .is_none()
    );

    // 同 (orgId, peer) 原地覆盖 = 幂等（只留一条）
    let mut updated = outgoing.clone();
    updated.peer_nickname = "新昵称".to_string();
    OrganizationService::put_invite_record(&mut storage, &updated).unwrap();
    let all = OrganizationService::list_all_invite_records(&storage).unwrap();
    assert_eq!(all.len(), 3, "同键覆盖不产生重复记录");

    // list_by_org 只含指定组织（两个 direction 合并）
    let org_records =
        OrganizationService::list_invite_records(&storage, "org_aaaabbbbccccdddd").unwrap();
    assert_eq!(org_records.len(), 2);

    // 按 id 查找（入站 / 任意方向）
    assert_eq!(
        OrganizationService::find_incoming_invite_by_id(&storage, "inv-2")
            .unwrap()
            .unwrap()
            .peer_root_id,
        peer_b
    );
    assert!(
        OrganizationService::find_incoming_invite_by_id(&storage, "inv-1")
            .unwrap()
            .is_none(),
        "出站记录不在入站查找内"
    );
    assert!(
        OrganizationService::find_invite_by_id(&storage, "inv-1")
            .unwrap()
            .is_some()
    );
}

#[test]
fn invite_record_mark_status_terminal_irreversible() {
    let mut storage = MemoryStorage::new();
    let peer = rid('a');
    let org_id = "org_aaaabbbbccccdddd";
    OrganizationService::put_invite_record(
        &mut storage,
        &record("inv-1", org_id, &peer, OrgInviteDirection::Outgoing),
    )
    .unwrap();

    // 不存在 → None
    assert!(
        OrganizationService::mark_invite_status(
            &mut storage,
            OrgInviteDirection::Outgoing,
            org_id,
            &rid('z'),
            OrgInviteStatus::Accepted,
            NOW,
        )
        .unwrap()
        .is_none()
    );

    // pending → accepted
    let marked = OrganizationService::mark_invite_status(
        &mut storage,
        OrgInviteDirection::Outgoing,
        org_id,
        &peer,
        OrgInviteStatus::Accepted,
        NOW + 1,
    )
    .unwrap()
    .expect("pending 可流转");
    assert_eq!(marked.status, OrgInviteStatus::Accepted);
    assert_eq!(marked.updated_at, NOW + 1);

    // 终态不可逆：再次流转返回 None，记录保持 accepted
    assert!(
        OrganizationService::mark_invite_status(
            &mut storage,
            OrgInviteDirection::Outgoing,
            org_id,
            &peer,
            OrgInviteStatus::Declined,
            NOW + 2,
        )
        .unwrap()
        .is_none()
    );
    let stored = OrganizationService::get_outgoing_invite(&storage, org_id, &peer)
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, OrgInviteStatus::Accepted);
    assert_eq!(stored.updated_at, NOW + 1);
}
