//! 组织空间邀请 outbox（自 unit_contact.rs 拆出，§2.1）。

use super::*;
// ------------------------------------------------------------------
// 组织空间邀请 outbox（ct:org:{orgId}:req:out:{id}）
// ------------------------------------------------------------------

fn org_outgoing(id: &str, root_id: &str) -> FriendRequestRecord {
    FriendRequestRecord {
        id: id.to_string(),
        root_id: root_id.to_string(),
        nickname: "待加入成员".to_string(),
        avatar: None,
        message: String::new(),
        source: "邀请码".to_string(),
        status: FriendRequestStatus::Pending,
        created_at: NOW,
        updated_at: NOW,
        peer: None,
        thread: Vec::new(),
        invite_code: Some("invite-code-1".to_string()),
    }
}

#[test]
fn org_outgoing_request_crud_and_mark_accepted() {
    use spark_core::storage::StorageBackend;

    let mut s = MemoryStorage::new();
    ContactService::put_org_outgoing_request(&mut s, "org-1", &org_outgoing("inv-1", &rid('b')))
        .unwrap();
    ContactService::put_org_outgoing_request(&mut s, "org-1", &org_outgoing("inv-2", &rid('c')))
        .unwrap();
    // 不同组织互不可见
    ContactService::put_org_outgoing_request(&mut s, "org-2", &org_outgoing("inv-9", &rid('b')))
        .unwrap();

    let stored = ContactService::get_org_outgoing_request(&s, "org-1", "inv-1")
        .unwrap()
        .unwrap();
    assert_eq!(stored.invite_code.as_deref(), Some("invite-code-1"));
    assert!(ContactService::get_org_outgoing_request(&s, "org-1", "inv-9").unwrap().is_none());

    let list = ContactService::list_org_outgoing_requests(&s, "org-1").unwrap();
    assert_eq!(list.len(), 2);

    // pending → accepted + 刷 updated_at；重复（非 pending）与不存在返回 Ok(false)
    assert!(ContactService::mark_org_outgoing_accepted(&mut s, "org-1", "inv-1", NOW + 10).unwrap());
    let accepted = ContactService::get_org_outgoing_request(&s, "org-1", "inv-1")
        .unwrap()
        .unwrap();
    assert_eq!(accepted.status, FriendRequestStatus::Accepted);
    assert_eq!(accepted.updated_at, NOW + 10);
    assert!(!ContactService::mark_org_outgoing_accepted(&mut s, "org-1", "inv-1", NOW + 20).unwrap());
    assert!(!ContactService::mark_org_outgoing_accepted(&mut s, "org-1", "inv-x", NOW).unwrap());

    // 线形：invite_code 为 Some 时带 camelCase inviteCode，None 时不序列化
    let raw = s.get(&format!("ct:org:org-1:req:out:inv-1")).unwrap().unwrap();
    assert!(raw.contains("\"inviteCode\":\"invite-code-1\""));
    let raw = s.get("ct:req:out:missing").unwrap();
    assert!(raw.is_none());
    let mut no_code = org_outgoing("inv-3", &rid('d'));
    no_code.invite_code = None;
    ContactService::put_org_outgoing_request(&mut s, "org-1", &no_code).unwrap();
    let raw = s.get("ct:org:org-1:req:out:inv-3").unwrap().unwrap();
    assert!(!raw.contains("inviteCode"));
}

#[test]
fn overview_org_space_includes_outgoing_invites() {
    let mut s = MemoryStorage::new();
    // 空组织空间 outgoing 为空 vec
    assert!(ContactService::overview(&s, ORG).unwrap().outgoing.is_empty());

    ContactService::put_org_outgoing_request(&mut s, "org-1", &org_outgoing("inv-1", &rid('b')))
        .unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(view.outgoing.len(), 1);
    assert_eq!(view.outgoing[0].id, "inv-1");
    assert_eq!(view.outgoing[0].invite_code.as_deref(), Some("invite-code-1"));
    // 组织空间 org outbox 不串到个人空间
    assert!(ContactService::overview(&s, PERSONAL).unwrap().outgoing.is_empty());
}
