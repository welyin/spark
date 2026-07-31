//! 通讯录命令单测：直调 *_inner，不依赖 WebView。

use super::*;
use spark_core::contact::{ContactService, FriendRecord};
use spark_core::kernel::KernelConfig;

const PASSWORD: &str = "correct-horse-battery";
const PERSONAL: &str = "personal";
const ORG_SPACE: &str = "org:org_test00000001";

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

/// 直接向个人空间写入一个朋友（测试辅助；绕开 p2p 申请流程）。
fn seed_friend(kernel: &mut Kernel, root_id: &str) {
    let friend = FriendRecord {
        root_id: root_id.to_string(),
        nickname: "Bob".to_string(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: 1,
        peer: None,
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked: false,
    };
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::upsert_friend(&mut storage, &friend).unwrap();
}

#[test]
fn overview_empty_shape() {
    let (_dir, mut kernel) = unlocked_kernel();
    let my_root = kernel.current_root_id().unwrap().unwrap();
    let view = overview_inner(&mut kernel, PERSONAL).unwrap();
    // friends 恒含自己（nickname 取当前身份昵称）
    assert_eq!(view.friends.len(), 1);
    assert_eq!(view.friends[0].root_id, my_root);
    assert_eq!(view.friends[0].nickname, "alice");
    assert!(view.requests.is_empty());
    assert!(view.outgoing.is_empty());
    assert!(view.tags.is_empty());
    assert!(view.groups.is_empty());
    assert!(view.group_tree.is_empty());
    assert!(view.member_extras.is_empty());

    // 序列化线形：七字段 camelCase 全在
    let json = serde_json::to_value(&view).unwrap();
    for key in [
        "friends",
        "requests",
        "outgoing",
        "tags",
        "groups",
        "groupTree",
        "memberExtras",
    ] {
        assert!(json.get(key).is_some(), "missing key {key}");
    }
}

#[test]
fn tag_create_rename_delete_flow() {
    let (_dir, mut kernel) = unlocked_kernel();

    // client id 透传
    let tag = tag_create_inner(&mut kernel, PERSONAL, "tag_001", "家人").unwrap();
    assert_eq!(tag.id, "tag_001");
    assert_eq!(tag.name, "家人");
    assert_eq!(overview_inner(&mut kernel, PERSONAL).unwrap().tags.len(), 1);

    assert!(tag_rename_inner(&mut kernel, PERSONAL, "tag_001", "亲属").unwrap().success);
    let view = overview_inner(&mut kernel, PERSONAL).unwrap();
    assert_eq!(view.tags[0].name, "亲属");

    assert!(tag_delete_inner(&mut kernel, PERSONAL, "tag_001").unwrap().success);
    assert!(overview_inner(&mut kernel, PERSONAL).unwrap().tags.is_empty());
}

#[test]
fn group_create_rename_move_delete_flow() {
    let (_dir, mut kernel) = unlocked_kernel();

    // client id 透传（space_key 仅对齐前端参数表，inner 不落空间维度）
    let g1 = group_create_inner(&mut kernel, "grp_001", "同事").unwrap();
    let g2 = group_create_inner(&mut kernel, "grp_002", "同学").unwrap();
    assert_eq!((g1.id.as_str(), g2.id.as_str()), ("grp_001", "grp_002"));
    let groups = overview_inner(&mut kernel, PERSONAL).unwrap().groups;
    assert_eq!(groups.len(), 2);

    assert!(group_rename_inner(&mut kernel, "grp_001", "工友").unwrap().success);
    let groups = overview_inner(&mut kernel, PERSONAL).unwrap().groups;
    assert_eq!(groups[0].name, "工友");

    // 拖拽重排：grp_002 移到最前
    assert!(group_move_inner(&mut kernel, "grp_002", 0).unwrap().success);
    let groups = overview_inner(&mut kernel, PERSONAL).unwrap().groups;
    assert_eq!(groups[0].id, "grp_002");
    assert_eq!(groups[1].id, "grp_001");

    assert!(group_delete_inner(&mut kernel, "grp_001").unwrap().success);
    let groups = overview_inner(&mut kernel, PERSONAL).unwrap().groups;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, "grp_002");
}

#[test]
fn org_group_tree_flow() {
    let (_dir, mut kernel) = unlocked_kernel();

    // 根层节点（client id 透传）
    let root = org_group_create_inner(&mut kernel, ORG_SPACE, "", "og_root", "总部")
        .unwrap()
        .expect("root node created");
    assert_eq!(root.id, "og_root");
    // 子节点
    let child = org_group_create_inner(&mut kernel, ORG_SPACE, "og_root", "og_child", "研发部")
        .unwrap()
        .expect("child node created");
    assert_eq!(child.id, "og_child");
    // 父不存在 → None（前端得 null）
    assert!(
        org_group_create_inner(&mut kernel, ORG_SPACE, "og_nope", "og_x", "幻影")
            .unwrap()
            .is_none()
    );

    let tree = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].children.len(), 1);
    assert_eq!(tree[0].children[0].id, "og_child");

    // 改名 / 同级重排 / 删除（子节点提升一层）
    assert!(org_group_rename_inner(&mut kernel, ORG_SPACE, "og_child", "研发一部")
        .unwrap()
        .success);
    let sibling = org_group_create_inner(&mut kernel, ORG_SPACE, "", "og_root2", "分部")
        .unwrap()
        .unwrap();
    assert_eq!(sibling.id, "og_root2");
    assert!(org_group_move_inner(&mut kernel, ORG_SPACE, "og_root2", 0, None)
        .unwrap()
        .success);
    let tree = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert_eq!(tree[0].id, "og_root2");
    assert_eq!(tree[1].children[0].name, "研发一部");

    assert!(org_group_delete_inner(&mut kernel, ORG_SPACE, "og_root")
        .unwrap()
        .success);
    let tree = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    // og_root 删除后子节点提升一层：og_root2 + og_child 同级
    assert_eq!(tree.len(), 2);
    assert!(tree.iter().any(|n| n.id == "og_child"));
}

#[test]
fn profile_blocked_group_remove_friend_flow() {
    let (_dir, mut kernel) = unlocked_kernel();
    let bob = "bb".repeat(32);
    seed_friend(&mut kernel, &bob);

    // overview 含「自己」条目：按 rootId 找 bob，不依赖数组位置
    let friend_of = |kernel: &mut Kernel| {
        overview_inner(kernel, PERSONAL)
            .unwrap()
            .friends
            .into_iter()
            .find(|f| f.root_id == bob)
            .expect("bob 在 friends 中")
    };

    // update_profile：camelCase 入参直接反序列化为 ProfilePatch
    let patch: ProfilePatch = serde_json::from_value(serde_json::json!({
        "remark": "小博",
        "tagIds": ["tag_a"],
        "memo": "前同事"
    }))
    .unwrap();
    assert!(update_profile_inner(&mut kernel, PERSONAL, &bob, patch)
        .unwrap()
        .success);
    let friend = friend_of(&mut kernel);
    assert_eq!(friend.remark, "小博");
    assert_eq!(friend.tag_ids, vec!["tag_a".to_string()]);
    assert_eq!(friend.memo, "前同事");

    // set_blocked 往返
    assert!(set_blocked_inner(&mut kernel, PERSONAL, &bob, true)
        .unwrap()
        .success);
    assert!(friend_of(&mut kernel).blocked);
    assert!(set_blocked_inner(&mut kernel, PERSONAL, &bob, false)
        .unwrap()
        .success);
    assert!(!friend_of(&mut kernel).blocked);

    // set_group：建组 → 挂组 → 复位未分组
    group_create_inner(&mut kernel, "grp_001", "同事").unwrap();
    assert!(set_group_inner(&mut kernel, PERSONAL, &bob, "grp_001")
        .unwrap()
        .success);
    assert_eq!(friend_of(&mut kernel).group_id, "grp_001");
    assert!(set_group_inner(&mut kernel, PERSONAL, &bob, "")
        .unwrap()
        .success);
    assert!(friend_of(&mut kernel).group_id.is_empty());

    // remove_friend（自己条目保留）
    assert!(remove_friend_inner(&mut kernel, &bob, false).unwrap().success);
    let friends = overview_inner(&mut kernel, PERSONAL).unwrap().friends;
    assert!(friends.iter().all(|f| f.root_id != bob));
    assert_eq!(friends.len(), 1, "仅剩自己条目");
    // 拉黑是独立集合（`ct:blocked:`）：删友后拉黑/解禁仍生效，
    // 重新加回或收到其私信时仍以拉黑处理
    assert!(set_blocked_inner(&mut kernel, PERSONAL, &bob, true)
        .unwrap()
        .success);
    assert!(set_blocked_inner(&mut kernel, PERSONAL, &bob, false)
        .unwrap()
        .success);
}

#[test]
fn self_blocked_and_remove_rejected() {
    let (_dir, mut kernel) = unlocked_kernel();
    let my_root = kernel.current_root_id().unwrap().unwrap();
    assert_eq!(
        set_blocked_inner(&mut kernel, PERSONAL, &my_root, true).unwrap_err(),
        "不能拉黑自己"
    );
    assert_eq!(
        remove_friend_inner(&mut kernel, &my_root, false).unwrap_err(),
        "不能删除自己"
    );
    // 自己条目仍在
    let friends = overview_inner(&mut kernel, PERSONAL).unwrap().friends;
    assert!(friends.iter().any(|f| f.root_id == my_root));
}

#[test]
fn resolve_request_unknown_errors() {
    let (_dir, mut kernel) = unlocked_kernel();
    assert_eq!(
        resolve_request_inner(&mut kernel, "req_nope", true, None).unwrap_err(),
        "好友申请不存在或已处理"
    );
    assert_eq!(
        resolve_request_inner(&mut kernel, "req_nope", false, Some("chatOnly")).unwrap_err(),
        "好友申请不存在或已处理"
    );
}

#[test]
fn reply_request_state_gate_and_thread() {
    use spark_core::contact::{FriendRequestRecord, FriendRequestStatus, PeerRef};

    let (_dir, mut kernel) = unlocked_kernel();
    // 申请不存在
    assert_eq!(
        reply_request_inner(&mut kernel, "req_nope", "我是张三").unwrap_err(),
        "申请不存在"
    );
    // 空文本
    assert_eq!(
        reply_request_inner(&mut kernel, "req_nope", "   ").unwrap_err(),
        "回复内容为空或过长"
    );

    let bob = "bb".repeat(32);
    let seed = |kernel: &mut Kernel, status: FriendRequestStatus| {
        let record = FriendRequestRecord {
            id: "req-1".to_string(),
            root_id: bob.clone(),
            nickname: String::new(),
            message: "hi".to_string(),
            source: "扫码".to_string(),
            status,
            created_at: 1,
            updated_at: 1,
            peer: Some(PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec![],
            }),
            thread: Vec::new(),
            invite_code: None,
            avatar: None,
        };
        let mut storage = kernel.__test_storage().unwrap();
        ContactService::put_outgoing_request(&mut storage, &record).unwrap();
    };

    // pending 状态不可回复（前端只在 replied 开放回复框）
    seed(&mut kernel, FriendRequestStatus::Pending);
    assert_eq!(
        reply_request_inner(&mut kernel, "req-1", "我是张三").unwrap_err(),
        "当前状态不可回复"
    );

    // replied → 回复成功：status 回 pending、thread 追加 from=me（p2p 未运行跳过投递）
    seed(&mut kernel, FriendRequestStatus::Replied);
    let record = reply_request_inner(&mut kernel, "req-1", " 我是张三 ").unwrap();
    assert_eq!(record.status, FriendRequestStatus::Pending);
    assert_eq!(record.thread.len(), 1);
    assert_eq!(record.thread[0].text, "我是张三", "trim 后落库");
    let storage = kernel.__test_storage().unwrap();
    let stored = ContactService::get_outgoing_request(&storage, "req-1")
        .unwrap()
        .expect("outbox 记录已落库");
    assert_eq!(stored.status, FriendRequestStatus::Pending);
    assert_eq!(stored.thread.len(), 1);
}

#[test]
fn ask_request_state_gate_and_thread() {
    use spark_core::contact::{FriendRequestRecord, FriendRequestStatus, PeerRef};

    let (_dir, mut kernel) = unlocked_kernel();
    // 申请不存在
    assert_eq!(
        ask_request_inner(&mut kernel, "req_nope", "请问你是哪位？").unwrap_err(),
        "申请不存在"
    );
    // 空文本
    assert_eq!(
        ask_request_inner(&mut kernel, "req_nope", "   ").unwrap_err(),
        "询问内容为空或过长"
    );

    let bob = "bb".repeat(32);
    let seed = |kernel: &mut Kernel, status: FriendRequestStatus| {
        let record = FriendRequestRecord {
            id: "req-1".to_string(),
            root_id: bob.clone(),
            nickname: String::new(),
            message: "hi".to_string(),
            source: "扫码".to_string(),
            status,
            created_at: 1,
            updated_at: 1,
            peer: Some(PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec![],
            }),
            thread: Vec::new(),
            invite_code: None,
            avatar: None,
        };
        let mut storage = kernel.__test_storage().unwrap();
        ContactService::put_incoming_request(&mut storage, &record).unwrap();
    };

    // 非 pending 不可询问（已处理申请不再受理）
    seed(&mut kernel, FriendRequestStatus::Accepted);
    assert_eq!(
        ask_request_inner(&mut kernel, "req-1", "请问你是哪位？").unwrap_err(),
        "当前状态不可询问"
    );

    // pending → 询问成功：status 保持 pending、thread 追加 from=me（p2p 未运行跳过投递）
    seed(&mut kernel, FriendRequestStatus::Pending);
    let record = ask_request_inner(&mut kernel, "req-1", " 请问你是哪位？ ").unwrap();
    assert_eq!(record.status, FriendRequestStatus::Pending);
    assert_eq!(record.thread.len(), 1);
    assert_eq!(record.thread[0].text, "请问你是哪位？", "trim 后落库");
    let storage = kernel.__test_storage().unwrap();
    let stored = ContactService::get_incoming_request(&storage, "req-1")
        .unwrap()
        .expect("inbox 记录已落库");
    assert_eq!(stored.status, FriendRequestStatus::Pending);
    assert_eq!(stored.thread.len(), 1);
}

#[test]
fn send_request_unaddressable_errors() {
    let (_dir, mut kernel) = unlocked_kernel();
    // raw 既不是节点名片、组织成员里也没有该 rootId → 寻址失败
    let input: SendFriendRequestInput = serde_json::from_value(serde_json::json!({
        "id": "req_001",
        "rootId": "cc".repeat(32),
        "raw": "not-a-node-card",
        "source": "RootID 搜索",
        "message": "你好"
    }))
    .unwrap();
    assert_eq!(
        send_request_inner(&mut kernel, input).unwrap_err(),
        "无法确定对方节点地址，请使用扫码名片添加"
    );
}

#[test]
fn org_group_move_cross_level() {
    let (_dir, mut kernel) = unlocked_kernel();
    org_group_create_inner(&mut kernel, ORG_SPACE, "", "og_root", "总部").unwrap();
    org_group_create_inner(&mut kernel, ORG_SPACE, "og_root", "og_child", "研发部").unwrap();
    org_group_create_inner(&mut kernel, ORG_SPACE, "", "og_root2", "分部").unwrap();

    // 跨级：根层 og_root2 移入 og_root 下首位 → 总部[分部, 研发部]
    assert!(
        org_group_move_inner(&mut kernel, ORG_SPACE, "og_root2", 0, Some("og_root"))
            .unwrap()
            .success
    );
    let tree = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].children[0].id, "og_root2");
    assert_eq!(tree[0].children[1].id, "og_child");

    // 跨级：og_child 移回根层（Some("")）→ [研发部, 总部]
    assert!(
        org_group_move_inner(&mut kernel, ORG_SPACE, "og_child", 0, Some(""))
            .unwrap()
            .success
    );
    let tree = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert_eq!(tree[0].id, "og_child");
    assert_eq!(tree[1].id, "og_root");

    // 防环：把 og_root 移入自己的子树 og_root2 → 静默忽略，树不变
    let before = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert!(
        org_group_move_inner(&mut kernel, ORG_SPACE, "og_root", 0, Some("og_root2"))
            .unwrap()
            .success
    );
    let after = overview_inner(&mut kernel, ORG_SPACE).unwrap().group_tree;
    assert_eq!(before, after, "成环移动应被忽略");
}

#[test]
fn remove_friend_with_block() {
    let (_dir, mut kernel) = unlocked_kernel();
    let bob = "bb".repeat(32);
    seed_friend(&mut kernel, &bob);

    // 删除 + 同时拉黑（§5.5）：friend 记录删除，拉黑集合写入
    assert!(remove_friend_inner(&mut kernel, &bob, true).unwrap().success);
    let friends = overview_inner(&mut kernel, PERSONAL).unwrap().friends;
    assert!(friends.iter().all(|f| f.root_id != bob));
    let storage = kernel.__test_storage().unwrap();
    assert!(
        ContactService::is_blocked(&storage, &bob).unwrap(),
        "删除同时拉黑后拉黑集合应含 bob"
    );
}
