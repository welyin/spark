//! kernel 通讯录门面集成测试：overview 空形、资料/拉黑/删除朋友、
//! 标签/分组/组织树的 client-id 命令、好友申请出站（名片寻址）与确认建朋友。

mod common;

use spark_core::contact::{
    ContactService, FriendRecord, FriendRequestRecord, FriendRequestStatus, PeerRef, ProfilePatch,
};
use spark_core::kernel::SendFriendRequestInput;
use spark_core::p2p::P2pEvent;
use spark_core::p2p::node::system_now_ms;

use common::*;

const PERSONAL: &str = "personal";
const ORG: &str = "org:o1";
const NOW: i64 = 1_720_000_000_000;

fn friend_record(root_id: &str) -> FriendRecord {
    FriendRecord {
        root_id: root_id.to_string(),
        nickname: "朋友昵称".to_string(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: NOW,
        peer: None,
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked: false,
    }
}

fn request_input(id: &str, root_id: &str, raw: &str) -> SendFriendRequestInput {
    SendFriendRequestInput {
        id: id.to_string(),
        root_id: root_id.to_string(),
        raw: raw.to_string(),
        peer_id: None,
        addresses: None,
        source: "扫码".to_string(),
        message: "交个朋友".to_string(),
    }
}

#[test]
fn overview_empty_shape() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);

    let personal = kernel.contact_overview(PERSONAL).unwrap();
    // friends 恒含自己（详见 overview_contains_self_and_refreshes_nickname）
    assert_eq!(personal.friends.len(), 1);
    assert_eq!(personal.friends[0].root_id, root_id);
    assert!(personal.requests.is_empty());
    assert!(personal.outgoing.is_empty());
    assert!(personal.tags.is_empty());
    assert!(personal.groups.is_empty());
    assert!(personal.group_tree.is_empty());
    assert!(personal.member_extras.is_empty());

    let org = kernel.contact_overview(ORG).unwrap();
    assert!(org.friends.is_empty(), "组织空间无朋友列表（不注入自己）");
    assert!(org.group_tree.is_empty());
    assert!(org.member_extras.is_empty());
}

#[test]
fn update_profile_blocked_remove_friend() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    let root_id = "bb".repeat(32);
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::upsert_friend(&mut storage, &friend_record(&root_id)).unwrap();

    kernel
        .contact_update_profile(
            PERSONAL,
            &root_id,
            ProfilePatch {
                remark: Some("备注".to_string()),
                memo: Some("备忘".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    kernel.contact_set_blocked(PERSONAL, &root_id, true).unwrap();
    let friend = ContactService::get_friend(&storage, &root_id).unwrap().unwrap();
    assert_eq!(friend.remark, "备注");
    assert_eq!(friend.memo, "备忘");
    assert!(friend.blocked);

    kernel.contact_remove_friend(&root_id).unwrap();
    assert!(ContactService::get_friend(&storage, &root_id).unwrap().is_none());

    // 删除后再改资料报 ContactNotFound
    let err = kernel
        .contact_update_profile(PERSONAL, &root_id, ProfilePatch::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "Contact not found");
}

#[test]
fn tags_groups_org_tree_use_client_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 标签（个人空间）
    let tag = kernel.contact_tag_create(PERSONAL, "tag-1", "邻居").unwrap();
    assert_eq!(tag.id, "tag-1", "标签 id 由前端生成传入");
    kernel.contact_tag_rename(PERSONAL, "tag-1", "好邻居").unwrap();
    let tags = kernel.contact_overview(PERSONAL).unwrap().tags;
    assert_eq!(tags[0].name, "好邻居");

    // 扁平分组
    kernel.contact_group_create("g1", "家人").unwrap();
    kernel.contact_group_create("g2", "同事").unwrap();
    kernel.contact_group_move("g2", 0).unwrap();
    let groups = kernel.contact_overview(PERSONAL).unwrap().groups;
    assert_eq!(groups[0].id, "g2", "拖拽重排生效");
    kernel.contact_group_rename("g2", "同学").unwrap();
    kernel.contact_group_delete("g1").unwrap();
    assert_eq!(kernel.contact_overview(PERSONAL).unwrap().groups.len(), 1);

    // 组织分组树
    let hq = kernel
        .contact_org_group_create(ORG, "", "og-1", "总部")
        .unwrap()
        .expect("根层创建成功");
    assert_eq!(hq.id, "og-1");
    let tech = kernel
        .contact_org_group_create(ORG, "og-1", "og-2", "技术部")
        .unwrap()
        .expect("子层创建成功");
    assert_eq!(tech.id, "og-2");
    assert!(
        kernel
            .contact_org_group_create(ORG, "og-x", "og-3", "幽灵部")
            .unwrap()
            .is_none(),
        "父不存在返回 None"
    );
    kernel.contact_org_group_rename(ORG, "og-2", "研发部").unwrap();
    let tree = kernel.contact_overview(ORG).unwrap().group_tree;
    assert_eq!(tree[0].children[0].name, "研发部");
    kernel.contact_org_group_delete(ORG, "og-1").unwrap();
    let tree = kernel.contact_overview(ORG).unwrap().group_tree;
    assert_eq!(tree.len(), 1, "子节点提升到根层");
    assert_eq!(tree[0].id, "og-2");

    kernel.contact_tag_delete(PERSONAL, "tag-1").unwrap();
    assert!(kernel.contact_overview(PERSONAL).unwrap().tags.is_empty());
}

#[test]
fn resolve_request_accept_creates_friend() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "cc".repeat(32);
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::put_incoming_request(
        &mut storage,
        &FriendRequestRecord {
            id: "req-1".to_string(),
            root_id: peer_root.clone(),
            nickname: "申请人".to_string(),
            avatar: None,
            message: "hi".to_string(),
            source: "扫码".to_string(),
            status: FriendRequestStatus::Pending,
            created_at: NOW,
            updated_at: NOW,
            peer: Some(PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec!["/ip4/1.2.3.4/tcp/9000".to_string()],
            }),
        },
    )
    .unwrap();

    let resolved = kernel
        .contact_resolve_request("req-1", true, Some("chatOnly"))
        .unwrap();
    assert_eq!(resolved.status, FriendRequestStatus::Accepted);
    let friend = ContactService::get_friend(&storage, &peer_root)
        .unwrap()
        .expect("接受后建朋友");
    assert_eq!(friend.permission, "chatOnly", "permission 写入资料");
    assert_eq!(friend.peer.as_ref().unwrap().peer_id, "peer-1", "peer 取请求记录");
    assert_eq!(friend.nickname, "申请人");

    // 重复处理报错
    let err = kernel.contact_resolve_request("req-1", true, None).unwrap_err();
    assert_eq!(err.to_string(), "好友申请不存在或已处理");
}

#[test]
fn send_request_requires_node_address() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "dd".repeat(32);

    // 无名片、无组织成员：指定错误文案
    let err = kernel
        .contact_send_request(request_input("req-1", &peer_root, "随便一段文本"))
        .unwrap_err();
    assert_eq!(err.to_string(), "无法确定对方节点地址，请使用扫码名片添加");

    // 已是朋友：拒绝
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::upsert_friend(&mut storage, &friend_record(&peer_root)).unwrap();
    let err = kernel
        .contact_send_request(request_input("req-2", &peer_root, "x"))
        .unwrap_err();
    assert_eq!(err.to_string(), "对方已经是你的朋友");
}

#[test]
fn send_request_with_node_card() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();

    // 现场签发一张节点名片（libp2p 密钥签名）
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let card = spark_core::org::make_node_card(
        &keypair,
        &peer_id,
        &["/ip4/127.0.0.1/tcp/19000".to_string()],
        system_now_ms(),
        None,
    )
    .unwrap();

    let peer_root = "ee".repeat(32);
    let record = kernel
        .contact_send_request(request_input("req-1", &peer_root, &card))
        .unwrap();
    assert_eq!(record.id, "req-1", "outbox id 用前端传入值");
    assert_eq!(record.status, FriendRequestStatus::Pending, "落库 pending 即返回");
    assert_eq!(record.peer.as_ref().unwrap().peer_id, peer_id);

    let outgoing = kernel.contact_overview(PERSONAL).unwrap().outgoing;
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].root_id, peer_root);
}

#[test]
fn send_request_with_explicit_peer_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "ee".repeat(32);

    // 前端已解析名片（spark-card JSON / 名片内容文本）上行 peerId/addresses：
    // raw 为原文（内核解析不了也不影响），显式字段优先于节点名片解析
    let record = kernel
        .contact_send_request(SendFriendRequestInput {
            id: "req-1".to_string(),
            root_id: peer_root.clone(),
            raw: "RootID: ...\nPeerId: ...\nP2P Addresses:\n...".to_string(),
            peer_id: Some("12D3KooWPeer".to_string()),
            addresses: Some(vec!["/ip4/192.168.31.98/tcp/15002".to_string()]),
            source: "名片".to_string(),
            message: String::new(),
        })
        .unwrap();
    let peer = record.peer.as_ref().unwrap();
    assert_eq!(peer.peer_id, "12D3KooWPeer");
    assert_eq!(peer.addresses, vec!["/ip4/192.168.31.98/tcp/15002".to_string()]);

    // 空 addresses 视为未提供，回退后续寻址（此处无名片/组织成员 → 报错）
    let err = kernel
        .contact_send_request(SendFriendRequestInput {
            id: "req-2".to_string(),
            root_id: peer_root.clone(),
            raw: "x".to_string(),
            peer_id: Some("12D3KooWPeer".to_string()),
            addresses: Some(Vec::new()),
            source: "名片".to_string(),
            message: String::new(),
        })
        .unwrap_err();
    assert_eq!(err.to_string(), "无法确定对方节点地址，请使用扫码名片添加");
}

#[test]
fn send_request_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let peer_root = "ee".repeat(32);
    let root_id = {
        let mut kernel = fresh_kernel(dir.path());
        let (root_id, _) = init_identity(&mut kernel);
        kernel.stop_p2p().unwrap();
        kernel
            .contact_send_request(SendFriendRequestInput {
                id: "req-1".to_string(),
                root_id: peer_root.clone(),
                raw: "名片内容文本".to_string(),
                peer_id: Some("12D3KooWPeer".to_string()),
                addresses: Some(vec!["/ip4/192.168.31.98/tcp/15002".to_string()]),
                source: "名片".to_string(),
            message: "交个朋友".to_string(),
            })
            .unwrap();
        kernel.shutdown().unwrap();
        root_id
    };

    // 重启（重开同一数据目录 + 解锁）：outbox 记录应从库中水合回来
    let mut kernel = fresh_kernel(dir.path());
    kernel.unlock(PASSWORD, Some(&root_id)).unwrap();
    kernel.stop_p2p().unwrap();
    let outgoing = kernel.contact_overview(PERSONAL).unwrap().outgoing;
    assert_eq!(outgoing.len(), 1, "重启后发出的申请应仍在");
    assert_eq!(outgoing[0].id, "req-1");
    assert_eq!(outgoing[0].root_id, peer_root);
    assert_eq!(
        outgoing[0].peer.as_ref().unwrap().addresses,
        vec!["/ip4/192.168.31.98/tcp/15002".to_string()],
        "peer 地址随记录持久化（重试/后续投递依赖）"
    );
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 好友申请投递终态 / 重试
// ---------------------------------------------------------------------------

#[test]
fn send_request_offline_marks_failed_and_emits_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (card, _) = make_node_card();
    let peer_root = "ee".repeat(32);
    let mut events = kernel.subscribe_p2p_events();

    // 命令落库 pending 立即返回；p2p 未运行 → 投递终态 Failed 经事件回传
    let record = kernel
        .contact_send_request(request_input("req-1", &peer_root, &card))
        .unwrap();
    assert_eq!(record.status, FriendRequestStatus::Pending);

    let stored = kernel.contact_overview(PERSONAL).unwrap().outgoing;
    assert_eq!(stored[0].status, FriendRequestStatus::Failed, "p2p 未运行置 Failed");
    assert!(stored[0].updated_at >= stored[0].created_at);

    let event = events.try_recv().expect("应发出 FriendRequestSent 事件");
    let P2pEvent::FriendRequestSent(data) = event else {
        panic!("应发出 FriendRequestSent 事件");
    };
    assert_eq!(data["request"]["id"], "req-1");
    assert_eq!(data["request"]["status"], "failed");
    assert!(data["request"]["updatedAt"].is_number());
}

#[test]
fn send_request_retry_reuses_stored_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "ee".repeat(32);
    let mut storage = kernel.__test_storage().unwrap();
    // 已失败的出站申请（名片来源：peer 带地址）
    ContactService::put_outgoing_request(
        &mut storage,
        &FriendRequestRecord {
            id: "req-1".to_string(),
            root_id: peer_root.clone(),
            nickname: "对方昵称".to_string(),
            avatar: None,
            message: "旧验证消息".to_string(),
            source: "扫码".to_string(),
            status: FriendRequestStatus::Failed,
            created_at: NOW,
            updated_at: NOW,
            peer: Some(PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec!["/ip4/1.2.3.4/tcp/9000".to_string()],
            }),
        },
    )
    .unwrap();
    let mut events = kernel.subscribe_p2p_events();

    // 重试：raw 只是 rootId（名片已丢，重新寻址必败）——复用已存记录
    let record = kernel
        .contact_send_request(SendFriendRequestInput {
            id: "req-1".to_string(),
            root_id: peer_root.clone(),
            raw: peer_root.clone(),
            peer_id: None,
            addresses: None,
            source: "RootID 搜索".to_string(),
            message: "新验证消息".to_string(),
        })
        .unwrap();
    assert_eq!(record.status, FriendRequestStatus::Pending, "重置 pending");
    assert_eq!(record.peer.as_ref().unwrap().peer_id, "peer-1", "复用已存 peer");
    assert_eq!(record.message, "旧验证消息", "复用已存 message");
    assert_eq!(record.source, "扫码", "复用已存 source");
    assert_eq!(record.created_at, NOW, "createdAt 保留首次时间");
    assert!(record.updated_at > NOW, "updatedAt 刷新");

    // p2p 未运行：再次投递失败置 Failed 并发事件（peer 仍保留）
    let event = events.try_recv().expect("应发出 FriendRequestSent 事件");
    let P2pEvent::FriendRequestSent(data) = event else {
        panic!("应发出 FriendRequestSent 事件");
    };
    assert_eq!(data["request"]["status"], "failed");
    assert_eq!(data["request"]["peer"]["peerId"], "peer-1");

    // 已处理的申请不允许重试
    let mut accepted = ContactService::get_outgoing_request(&storage, "req-1")
        .unwrap()
        .unwrap();
    accepted.status = FriendRequestStatus::Accepted;
    ContactService::put_outgoing_request(&mut storage, &accepted).unwrap();
    let err = kernel
        .contact_send_request(request_input("req-1", &peer_root, "x"))
        .unwrap_err();
    assert_eq!(err.to_string(), "该申请已处理，无法重新发送");
}

// ---------------------------------------------------------------------------
// 自己作为联系人 / 设备配对
// ---------------------------------------------------------------------------

fn make_node_card() -> (String, String) {
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let peer_id = libp2p::PeerId::from_public_key(&keypair.public()).to_base58();
    let card = spark_core::org::make_node_card(
        &keypair,
        &peer_id,
        &["/ip4/127.0.0.1/tcp/19001".to_string()],
        system_now_ms(),
        None,
    )
    .unwrap();
    (card, peer_id)
}

#[test]
fn overview_contains_self_and_refreshes_nickname() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);

    let view = kernel.contact_overview(PERSONAL).unwrap();
    let me = view
        .friends
        .iter()
        .find(|f| f.root_id == root_id)
        .expect("friends 恒含自己");
    assert_eq!(me.nickname, "小明", "nickname 取当前身份昵称");
    assert!(me.peer.is_none());
    assert_eq!(me.permission, "open");
    let added_at = me.added_at;

    // 改名后再次 overview：nickname 刷新、addedAt 保留首次创建时间
    kernel.update_profile_session(Some("新名字"), None).unwrap();
    let view = kernel.contact_overview(PERSONAL).unwrap();
    let me = view.friends.iter().find(|f| f.root_id == root_id).unwrap();
    assert_eq!(me.nickname, "新名字");
    assert_eq!(me.added_at, added_at);
    assert_eq!(
        view.friends.iter().filter(|f| f.root_id == root_id).count(),
        1,
        "自己条目不重复"
    );
}

#[test]
fn self_blocked_and_remove_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);
    kernel.contact_overview(PERSONAL).unwrap();

    let err = kernel
        .contact_set_blocked(PERSONAL, &root_id, true)
        .unwrap_err();
    assert_eq!(err.to_string(), "不能拉黑自己");
    let err = kernel.contact_remove_friend(&root_id).unwrap_err();
    assert_eq!(err.to_string(), "不能删除自己");

    // 自己条目仍在
    let view = kernel.contact_overview(PERSONAL).unwrap();
    assert!(view.friends.iter().any(|f| f.root_id == root_id));
}

#[test]
fn send_request_to_self_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    // overview 注入「自己」条目后，send_request 对自己仍放行（重复配对刷新设备地址）
    kernel.contact_overview(PERSONAL).unwrap();

    let (card, peer_id) = make_node_card();
    let record = kernel
        .contact_send_request(request_input("req-self-1", &root_id, &card))
        .unwrap();
    assert_eq!(record.root_id, root_id);
    assert_eq!(record.peer.as_ref().unwrap().peer_id, peer_id);
}


// ---------------------------------------------------------------------------
// 安全/正确性回归：接受合并保留本地资料、拉黑独立集合
// ---------------------------------------------------------------------------

#[test]
fn resolve_request_accept_merges_existing_friend() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "cc".repeat(32);
    let mut storage = kernel.__test_storage().unwrap();
    // 既有朋友记录：带本地资料与既有 addedAt/permission
    let mut existing = friend_record(&peer_root);
    existing.remark = "旧备注".to_string();
    existing.tag_ids = vec!["tag-1".to_string()];
    existing.permission = "chatOnly".to_string();
    existing.added_at = NOW - 1000;
    ContactService::upsert_friend(&mut storage, &existing).unwrap();
    ContactService::put_incoming_request(
        &mut storage,
        &FriendRequestRecord {
            id: format!("{peer_root}:req-1"),
            root_id: peer_root.clone(),
            nickname: "新昵称".to_string(),
            avatar: None,
            message: "hi".to_string(),
            source: "扫码".to_string(),
            status: FriendRequestStatus::Pending,
            created_at: NOW,
            updated_at: NOW,
            peer: Some(PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec!["/ip4/1.2.3.4/tcp/9000".to_string()],
            }),
        },
    )
    .unwrap();

    kernel
        .contact_resolve_request(&format!("{peer_root}:req-1"), true, None)
        .unwrap();
    let friend = ContactService::get_friend(&storage, &peer_root).unwrap().unwrap();
    assert_eq!(friend.nickname, "新昵称", "非空 nickname 刷新");
    assert_eq!(friend.peer.as_ref().unwrap().peer_id, "peer-1", "Some peer 刷新");
    assert_eq!(friend.remark, "旧备注", "本地资料保留");
    assert_eq!(friend.tag_ids, vec!["tag-1".to_string()], "标签保留");
    assert_eq!(friend.permission, "chatOnly", "permission 不被重置");
    assert_eq!(friend.added_at, NOW - 1000, "addedAt 保留首次时间");
}

#[test]
fn blocked_stranger_and_blocked_survives_remove_friend() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let peer_root = "ff".repeat(32);
    let storage = kernel.__test_storage().unwrap();

    // 拉黑陌生人（无 friend 记录）
    kernel.contact_set_blocked(PERSONAL, &peer_root, true).unwrap();
    assert!(ContactService::is_blocked(&storage, &peer_root).unwrap());

    // 加成朋友后删除：拉黑仍生效；overview 以集合为准
    ContactService::upsert_friend(&mut kernel.__test_storage().unwrap(), &friend_record(&peer_root))
        .unwrap();
    kernel.contact_remove_friend(&peer_root).unwrap();
    assert!(
        ContactService::is_blocked(&storage, &peer_root).unwrap(),
        "删除朋友后拉黑仍生效"
    );
}
