//! 好友申请状态机与回复线程（自 unit_contact.rs 拆出，§2.1 测试文件上限）。

use super::*;
// ------------------------------------------------------------------
// 好友申请状态机
// ------------------------------------------------------------------

#[test]
fn incoming_request_state_machine() {
    let mut s = MemoryStorage::new();
    ContactService::put_incoming_request(&mut s, &incoming("req-1", &rid('r'))).unwrap();
    assert_eq!(
        ContactService::get_incoming_request(&s, "req-1").unwrap().unwrap().status,
        FriendRequestStatus::Pending
    );

    // 接受：pending → accepted
    assert!(ContactService::resolve_incoming_request(&mut s, "req-1", true, NOW).unwrap());
    assert_eq!(
        ContactService::get_incoming_request(&s, "req-1").unwrap().unwrap().status,
        FriendRequestStatus::Accepted
    );
    // 非 pending 忽略并返回 false
    assert!(!ContactService::resolve_incoming_request(&mut s, "req-1", false, NOW).unwrap());
    assert_eq!(
        ContactService::get_incoming_request(&s, "req-1").unwrap().unwrap().status,
        FriendRequestStatus::Accepted
    );
    // 不存在返回 false
    assert!(!ContactService::resolve_incoming_request(&mut s, "req-x", true, NOW).unwrap());

    // 拒绝：pending → ignored
    ContactService::put_incoming_request(&mut s, &incoming("req-2", &rid('s'))).unwrap();
    assert!(ContactService::resolve_incoming_request(&mut s, "req-2", false, NOW).unwrap());
    assert_eq!(
        ContactService::get_incoming_request(&s, "req-2").unwrap().unwrap().status,
        FriendRequestStatus::Ignored
    );
}

#[test]
fn outgoing_request_lifecycle() {
    let mut s = MemoryStorage::new();
    // 同毫秒两条：id 不冲突
    let first = ContactService::create_outgoing_request(
        &mut s,
        &rid('a'),
        "阿强",
        "加个朋友",
        "RootID 搜索",
        None,
        NOW,
    )
    .unwrap();
    let second = ContactService::create_outgoing_request(
        &mut s,
        &rid('b'),
        "博哥",
        "",
        "扫码",
        Some(PeerRef {
            peer_id: "peer-2".to_string(),
            addresses: vec![],
        }),
        NOW,
    )
    .unwrap();
    assert!(first.id.starts_with(&format!("out-{NOW}-")));
    assert_ne!(first.id, second.id);
    assert_eq!(first.status, FriendRequestStatus::Pending);
    assert_eq!(first.created_at, NOW);

    // find_outgoing_by_root
    let found = ContactService::find_outgoing_by_root(&s, &rid('b')).unwrap().unwrap();
    assert_eq!(found.id, second.id);
    assert_eq!(found.peer.as_ref().unwrap().peer_id, "peer-2");
    assert_eq!(ContactService::find_outgoing_by_root(&s, &rid('z')).unwrap(), None);

    // mark_outgoing_accepted：pending → accepted；重复与非存在返回 false
    assert!(ContactService::mark_outgoing_accepted(&mut s, &first.id, NOW).unwrap());
    assert!(!ContactService::mark_outgoing_accepted(&mut s, &first.id, NOW).unwrap());
    assert!(!ContactService::mark_outgoing_accepted(&mut s, "out-x", NOW).unwrap());
    let accepted = ContactService::find_outgoing_by_root(&s, &rid('a')).unwrap().unwrap();
    assert_eq!(accepted.status, FriendRequestStatus::Accepted);
}

#[test]
fn request_writes_maintain_updated_at() {
    let mut s = MemoryStorage::new();

    // 新建 = created_at
    let outgoing = ContactService::create_outgoing_request(
        &mut s,
        &rid('a'),
        "阿强",
        "加个朋友",
        "RootID 搜索",
        None,
        NOW,
    )
    .unwrap();
    assert_eq!(outgoing.updated_at, NOW);

    // 入站处理刷新 updated_at、保留 created_at
    ContactService::put_incoming_request(&mut s, &incoming("req-1", &rid('r'))).unwrap();
    assert!(ContactService::resolve_incoming_request(&mut s, "req-1", true, NOW + 10).unwrap());
    let resolved = ContactService::get_incoming_request(&s, "req-1").unwrap().unwrap();
    assert_eq!(resolved.created_at, NOW);
    assert_eq!(resolved.updated_at, NOW + 10);

    // 出站被接受刷新 updated_at
    assert!(ContactService::mark_outgoing_accepted(&mut s, &outgoing.id, NOW + 20).unwrap());
    let accepted = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(accepted.created_at, NOW);
    assert_eq!(accepted.updated_at, NOW + 20);

    // put_outgoing_request 兜底：updated_at 未填（0）时取 created_at
    let mut legacy = outgoing.clone();
    legacy.id = "out-legacy".to_string();
    legacy.updated_at = 0;
    ContactService::put_outgoing_request(&mut s, &legacy).unwrap();
    let stored = ContactService::get_outgoing_request(&s, "out-legacy").unwrap().unwrap();
    assert_eq!(stored.updated_at, stored.created_at);
}

#[test]
fn friend_request_status_failed_wire_form() {
    // Failed 仅 outbox 用；线形对齐 camelCase rename
    assert_eq!(
        serde_json::to_value(FriendRequestStatus::Failed).unwrap(),
        serde_json::json!("failed")
    );
    assert_eq!(
        serde_json::from_value::<FriendRequestStatus>(serde_json::json!("failed")).unwrap(),
        FriendRequestStatus::Failed
    );
}

#[test]
fn friend_request_status_replied_wire_form() {
    // Replied 仅 outbox 用（对方回复询问）；线形对齐 camelCase rename
    assert_eq!(
        serde_json::to_value(FriendRequestStatus::Replied).unwrap(),
        serde_json::json!("replied")
    );
    assert_eq!(
        serde_json::from_value::<FriendRequestStatus>(serde_json::json!("replied")).unwrap(),
        FriendRequestStatus::Replied
    );
}

#[test]
fn append_outgoing_thread_state_transitions() {
    let mut s = MemoryStorage::new();
    let outgoing = ContactService::create_outgoing_request(
        &mut s,
        &rid('a'),
        "阿强",
        "加个朋友",
        "RootID 搜索",
        None,
        NOW,
    )
    .unwrap();

    // 对方来消息：pending → replied，thread 追加，updated_at 刷新
    let peer_msg = RequestThreadMessage {
        from: ThreadFrom::Peer,
        text: "请问你是哪位？".to_string(),
        ts: NOW + 1,
    };
    let record = ContactService::append_outgoing_thread(&mut s, &outgoing.id, peer_msg.clone(), NOW + 1)
        .unwrap()
        .expect("记录存在");
    assert_eq!(record.status, FriendRequestStatus::Replied);
    assert_eq!(record.thread, vec![peer_msg.clone()]);
    assert_eq!(record.updated_at, NOW + 1);

    // 我回复：replied → pending（等待对方），thread 续接
    let my_msg = RequestThreadMessage {
        from: ThreadFrom::Me,
        text: "我是张三".to_string(),
        ts: NOW + 2,
    };
    let record = ContactService::append_outgoing_thread(&mut s, &outgoing.id, my_msg.clone(), NOW + 2)
        .unwrap()
        .expect("记录存在");
    assert_eq!(record.status, FriendRequestStatus::Pending);
    assert_eq!(record.thread, vec![peer_msg, my_msg]);
    assert_eq!(record.updated_at, NOW + 2);

    // 落库内容与返回值一致
    let stored = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(stored, record);

    // 记录不存在返回 Ok(None)
    let ghost = RequestThreadMessage {
        from: ThreadFrom::Peer,
        text: "hi".to_string(),
        ts: NOW,
    };
    assert!(
        ContactService::append_outgoing_thread(&mut s, "out-x", ghost, NOW)
            .unwrap()
            .is_none()
    );
}

#[test]
fn append_incoming_thread_keeps_status() {
    let mut s = MemoryStorage::new();
    ContactService::put_incoming_request(&mut s, &incoming("req-1", &rid('r'))).unwrap();

    // 对方回答我的询问：thread 追加、status 不变（仍待我接受/忽略）
    let msg = RequestThreadMessage {
        from: ThreadFrom::Peer,
        text: "我是张三".to_string(),
        ts: NOW + 1,
    };
    let record = ContactService::append_incoming_thread(&mut s, "req-1", msg.clone(), NOW + 1)
        .unwrap()
        .expect("记录存在");
    assert_eq!(record.status, FriendRequestStatus::Pending);
    assert_eq!(record.thread, vec![msg.clone()]);
    assert_eq!(record.updated_at, NOW + 1);

    // 记录不存在返回 Ok(None)
    assert!(
        ContactService::append_incoming_thread(&mut s, "req-x", msg, NOW)
            .unwrap()
            .is_none()
    );
}

#[test]
fn append_thread_truncates_to_cap() {
    // thread 条数上限（R1 review M3：对端持一条 pending/replied 申请即可
    // 持续追加刷大 sled）：超出丢弃最旧，保留最新 100 条
    let mut s = MemoryStorage::new();
    let outgoing = ContactService::create_outgoing_request(
        &mut s,
        &rid('a'),
        "阿强",
        "加个朋友",
        "RootID 搜索",
        None,
        NOW,
    )
    .unwrap();
    for i in 0..105 {
        let msg = RequestThreadMessage {
            from: ThreadFrom::Peer,
            text: format!("第 {i} 条"),
            ts: NOW + i,
        };
        ContactService::append_outgoing_thread(&mut s, &outgoing.id, msg, NOW + i).unwrap();
    }
    let stored = ContactService::get_outgoing_request(&s, &outgoing.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.thread.len(), 100, "截断到上限");
    assert_eq!(stored.thread.first().unwrap().text, "第 5 条", "最旧的被丢弃");
    assert_eq!(stored.thread.last().unwrap().text, "第 104 条", "最新的保留");

    // 入站申请 thread 同口径
    ContactService::put_incoming_request(&mut s, &incoming("req-cap", &rid('r'))).unwrap();
    for i in 0..101 {
        let msg = RequestThreadMessage {
            from: ThreadFrom::Peer,
            text: format!("m{i}"),
            ts: NOW + i,
        };
        ContactService::append_incoming_thread(&mut s, "req-cap", msg, NOW + i).unwrap();
    }
    let stored = ContactService::get_incoming_request(&s, "req-cap")
        .unwrap()
        .unwrap();
    assert_eq!(stored.thread.len(), 100);
    assert_eq!(stored.thread.first().unwrap().text, "m1");
}

#[test]
fn mark_outgoing_accepted_allows_replied() {
    let mut s = MemoryStorage::new();
    let outgoing = ContactService::create_outgoing_request(
        &mut s,
        &rid('a'),
        "阿强",
        "加个朋友",
        "RootID 搜索",
        None,
        NOW,
    )
    .unwrap();
    // replied 状态下对方接受应放行（pending/replied 均可 accepted）
    let msg = RequestThreadMessage {
        from: ThreadFrom::Peer,
        text: "请问你是哪位？".to_string(),
        ts: NOW,
    };
    ContactService::append_outgoing_thread(&mut s, &outgoing.id, msg, NOW).unwrap();
    assert!(ContactService::mark_outgoing_accepted(&mut s, &outgoing.id, NOW).unwrap());
    let stored = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(stored.status, FriendRequestStatus::Accepted);
    assert_eq!(stored.thread.len(), 1, "thread 保留");
}

