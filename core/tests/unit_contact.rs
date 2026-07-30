//! contact 模块单元测试（MemoryStorage）：朋友 CRUD、资料补丁、拉黑、申请状态机、
//! 标签增删改与摘除、个人分组 CRUD/重排/删除复位、组织分组树的建/改/删/同级重排、
//! overview 形状。

use spark_core::contact::{
    ContactError, ContactProfileRecord, ContactService, ContactTag, FriendRecord,
    FriendRequestRecord, FriendRequestStatus, OrgGroupNode, PeerRef, ProfilePatch,
};
use spark_core::storage::MemoryStorage;

const NOW: i64 = 1_720_000_000_000;
const PERSONAL: &str = "personal";
const ORG: &str = "org:org-1";

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn friend(root_id: &str, nickname: &str) -> FriendRecord {
    FriendRecord {
        root_id: root_id.to_string(),
        nickname: nickname.to_string(),
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

fn incoming(id: &str, root_id: &str) -> FriendRequestRecord {
    FriendRequestRecord {
        id: id.to_string(),
        root_id: root_id.to_string(),
        nickname: "申请人".to_string(),
        avatar: None,
        message: "你好".to_string(),
        source: "RootID 搜索".to_string(),
        status: FriendRequestStatus::Pending,
        created_at: NOW,
        updated_at: NOW,
        peer: None,
    }
}

// ------------------------------------------------------------------
// 朋友 CRUD
// ------------------------------------------------------------------

#[test]
fn friend_crud_roundtrip() {
    let mut s = MemoryStorage::new();
    assert_eq!(ContactService::get_friend(&s, &rid('a')).unwrap(), None);

    let mut f = friend(&rid('a'), "阿强");
    f.signature = "越努力越幸运".to_string();
    f.gender = Some("male".to_string());
    f.peer = Some(PeerRef {
        peer_id: "peer-1".to_string(),
        addresses: vec!["/ip4/1.2.3.4/tcp/4001".to_string()],
    });
    ContactService::upsert_friend(&mut s, &f).unwrap();
    assert_eq!(ContactService::get_friend(&s, &rid('a')).unwrap(), Some(f.clone()));

    // upsert 覆盖
    let mut updated = f.clone();
    updated.nickname = "阿强2".to_string();
    ContactService::upsert_friend(&mut s, &updated).unwrap();
    assert_eq!(
        ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().nickname,
        "阿强2"
    );

    ContactService::remove_friend(&mut s, &rid('a')).unwrap();
    assert_eq!(ContactService::get_friend(&s, &rid('a')).unwrap(), None);
    // 删除不存在的键不报错
    ContactService::remove_friend(&mut s, &rid('a')).unwrap();
}

// ------------------------------------------------------------------
// 资料补丁与拉黑
// ------------------------------------------------------------------

#[test]
fn update_profile_personal_patches_friend() {
    let mut s = MemoryStorage::new();
    ContactService::upsert_friend(&mut s, &friend(&rid('a'), "阿强")).unwrap();

    ContactService::update_profile(
        &mut s,
        PERSONAL,
        &rid('a'),
        ProfilePatch {
            remark: Some("强哥".to_string()),
            phones: Some(vec!["138****1234".to_string()]),
            tag_ids: Some(vec!["tag-1".to_string()]),
            memo: Some("周三值班".to_string()),
            photos: Some(vec!["photo-1".to_string()]),
            permission: Some("chatOnly".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .unwrap();
    let f = ContactService::get_friend(&s, &rid('a')).unwrap().unwrap();
    assert_eq!(f.remark, "强哥");
    assert_eq!(f.phones, vec!["138****1234"]);
    assert_eq!(f.tag_ids, vec!["tag-1"]);
    assert_eq!(f.memo, "周三值班");
    assert_eq!(f.photos, vec!["photo-1"]);
    assert_eq!(f.permission, "chatOnly");
    // None 字段保持不变
    assert_eq!(f.nickname, "阿强");
    assert_eq!(f.group_id, "");

    // 不存在的联系人报错
    let err = ContactService::update_profile(
        &mut s,
        PERSONAL,
        &rid('b'),
        ProfilePatch::default(),
        NOW,
    )
    .unwrap_err();
    assert!(matches!(err, ContactError::ContactNotFound));
}

#[test]
fn update_profile_org_lazily_creates_extra() {
    let mut s = MemoryStorage::new();
    ContactService::update_profile(
        &mut s,
        ORG,
        &rid('m'),
        ProfilePatch {
            remark: Some("同事甲".to_string()),
            ..Default::default()
        },
        NOW,
    )
    .unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    let extra = view.member_extras.get(&rid('m')).unwrap();
    assert_eq!(extra.remark, "同事甲");
    // 惰性建的默认值
    assert_eq!(extra.permission, "open");
    assert!(!extra.blocked);
    assert!(extra.tag_ids.is_empty());
}

#[test]
fn set_blocked_both_spaces() {
    let mut s = MemoryStorage::new();
    ContactService::upsert_friend(&mut s, &friend(&rid('a'), "阿强")).unwrap();
    ContactService::set_blocked(&mut s, PERSONAL, &rid('a'), true, NOW).unwrap();
    assert!(ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().blocked);
    ContactService::set_blocked(&mut s, PERSONAL, &rid('a'), false, NOW).unwrap();
    assert!(!ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().blocked);

    // 陌生人（无 friend 记录）也可拉黑：写独立集合，不报错
    ContactService::set_blocked(&mut s, PERSONAL, &rid('b'), true, NOW).unwrap();
    assert!(ContactService::is_blocked(&s, &rid('b')).unwrap());
    assert!(ContactService::get_friend(&s, &rid('b')).unwrap().is_none());

    // 组织空间惰性建
    ContactService::set_blocked(&mut s, ORG, &rid('m'), true, NOW).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert!(view.member_extras.get(&rid('m')).unwrap().blocked);
}

#[test]
fn blocked_survives_remove_friend_and_overview_overlays() {
    let mut s = MemoryStorage::new();
    ContactService::upsert_friend(&mut s, &friend(&rid('a'), "阿强")).unwrap();
    ContactService::set_blocked(&mut s, PERSONAL, &rid('a'), true, NOW).unwrap();

    // 删除朋友不清拉黑集合
    ContactService::remove_friend(&mut s, &rid('a')).unwrap();
    assert!(ContactService::is_blocked(&s, &rid('a')).unwrap(), "删除朋友后拉黑仍生效");

    // 重新加成朋友：overview 的 blocked 以集合为准 overlay（镜像字段可能滞后）
    ContactService::upsert_friend(&mut s, &friend(&rid('a'), "阿强")).unwrap();
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert!(view.friends.iter().find(|f| f.root_id == rid('a')).unwrap().blocked);

    // 取消拉黑：集合清除 + friend 镜像复位
    ContactService::set_blocked(&mut s, PERSONAL, &rid('a'), false, NOW).unwrap();
    assert!(!ContactService::is_blocked(&s, &rid('a')).unwrap());
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert!(!view.friends.iter().find(|f| f.root_id == rid('a')).unwrap().blocked);
}

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

// ------------------------------------------------------------------
// 标签
// ------------------------------------------------------------------

#[test]
fn tag_create_rename_delete_strips_references() {
    let mut s = MemoryStorage::new();
    let tag1 = ContactService::create_tag_with_id(&mut s, PERSONAL, "tag-1", "邻居").unwrap();
    let tag2 = ContactService::create_tag_with_id(&mut s, PERSONAL, "tag-2", "同事").unwrap();

    ContactService::rename_tag(&mut s, PERSONAL, &tag1.id, "好邻居").unwrap();
    ContactService::rename_tag(&mut s, PERSONAL, "tag-x", "无效").unwrap(); // 不存在忽略

    // 朋友引用两个标签
    let mut f = friend(&rid('a'), "阿强");
    f.tag_ids = vec![tag1.id.clone(), tag2.id.clone()];
    ContactService::upsert_friend(&mut s, &f).unwrap();

    // 删除 tag1：从数组与朋友 tagIds 中摘除
    ContactService::delete_tag(&mut s, PERSONAL, &tag1.id).unwrap();
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(
        view.tags,
        vec![ContactTag {
            id: tag2.id.clone(),
            name: "同事".to_string()
        }]
    );
    assert_eq!(
        ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().tag_ids,
        vec![tag2.id.clone()]
    );
}

#[test]
fn org_tag_delete_strips_member_extras() {
    let mut s = MemoryStorage::new();
    let tag = ContactService::create_tag_with_id(&mut s, ORG, "tag-core", "核心成员").unwrap();
    ContactService::update_profile(
        &mut s,
        ORG,
        &rid('m'),
        ProfilePatch {
            tag_ids: Some(vec![tag.id.clone()]),
            ..Default::default()
        },
        NOW,
    )
    .unwrap();

    ContactService::delete_tag(&mut s, ORG, &tag.id).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert!(view.tags.is_empty());
    assert!(view.member_extras.get(&rid('m')).unwrap().tag_ids.is_empty());
}

// ------------------------------------------------------------------
// 个人空间扁平分组
// ------------------------------------------------------------------

#[test]
fn personal_group_crud_reorder_and_reset() {
    let mut s = MemoryStorage::new();
    let g1 = ContactService::create_group_with_id(&mut s, "group-1", "家人").unwrap();
    let g2 = ContactService::create_group_with_id(&mut s, "group-2", "同学").unwrap();
    let g3 = ContactService::create_group_with_id(&mut s, "group-3", "同事").unwrap();

    ContactService::rename_group(&mut s, &g2.id, "老同学").unwrap();
    ContactService::rename_group(&mut s, "group-x", "无效").unwrap(); // 不存在忽略

    // 组内成员
    let mut f = friend(&rid('a'), "阿强");
    f.group_id = g2.id.clone();
    ContactService::upsert_friend(&mut s, &f).unwrap();
    ContactService::set_contact_group(&mut s, PERSONAL, &rid('a'), &g3.id, NOW).unwrap();
    assert_eq!(
        ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().group_id,
        g3.id
    );
    // set_contact_group 对缺失 friend 报错
    let err = ContactService::set_contact_group(&mut s, PERSONAL, &rid('b'), &g1.id, NOW)
        .unwrap_err();
    assert!(matches!(err, ContactError::ContactNotFound));

    // 重排（对齐 TS splice 语义：toIndex 以原序为准，源在目标位之前时摘除后
    // 目标前移一位）：[g1, g2, g3] → 把 g1 移到下标 2（原 g3 之前）→ [g2, g1, g3]
    ContactService::move_group(&mut s, &g1.id, 2).unwrap();
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(
        view.groups.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
        vec![g2.id.as_str(), g1.id.as_str(), g3.id.as_str()]
    );
    // toIndex == len 表示移到末尾：越界夹紧到 len；不存在忽略
    ContactService::move_group(&mut s, &g2.id, 99).unwrap();
    ContactService::move_group(&mut s, "group-x", 0).unwrap();
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(
        view.groups.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(),
        vec![g1.id.as_str(), g3.id.as_str(), g2.id.as_str()]
    );

    // 删除分组：组内 friend.groupId 复位为 ""
    ContactService::delete_group(&mut s, &g3.id).unwrap();
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(view.groups.len(), 2);
    assert_eq!(ContactService::get_friend(&s, &rid('a')).unwrap().unwrap().group_id, "");
}

#[test]
fn personal_group_move_matches_frontend_splice() {
    // 逐字对齐 TS moveGroup：target = clamp(toIndex, 0, len)；摘除后
    // from < target 时落点为 target-1，否则 target
    let mut s = MemoryStorage::new();
    for (id, name) in [("a", "甲"), ("b", "乙"), ("c", "丙"), ("d", "丁")] {
        ContactService::create_group_with_id(&mut s, id, name).unwrap();
    }
    let order = |s: &MemoryStorage| {
        ContactService::overview(s, PERSONAL)
            .unwrap()
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect::<Vec<_>>()
    };

    // 后移：a → 下标 2（原 c 之前）：[b, a, c, d]
    ContactService::move_group(&mut s, "a", 2).unwrap();
    assert_eq!(order(&s), vec!["b", "a", "c", "d"]);
    // 前移：d → 下标 0：[d, b, a, c]
    ContactService::move_group(&mut s, "d", 0).unwrap();
    assert_eq!(order(&s), vec!["d", "b", "a", "c"]);
    // toIndex == len 移到末尾：b → 4：[d, a, c, b]
    ContactService::move_group(&mut s, "b", 4).unwrap();
    assert_eq!(order(&s), vec!["d", "a", "c", "b"]);
    // 越界夹紧到 len：d → 99：[a, c, b, d]
    ContactService::move_group(&mut s, "d", 99).unwrap();
    assert_eq!(order(&s), vec!["a", "c", "b", "d"]);
    // 原位移动（from == target）：c → 1：摘除后落点不变 [a, c, b, d]
    ContactService::move_group(&mut s, "c", 1).unwrap();
    assert_eq!(order(&s), vec!["a", "c", "b", "d"]);
}

// ------------------------------------------------------------------
// 组织空间分组树
// ------------------------------------------------------------------

fn seed_tree(s: &mut MemoryStorage) -> (OrgGroupNode, OrgGroupNode, OrgGroupNode, OrgGroupNode) {
    let hq = ContactService::create_org_group_with_id(s, ORG, "", "og-hq", "总部")
        .unwrap()
        .unwrap();
    let tech = ContactService::create_org_group_with_id(s, ORG, &hq.id, "og-tech", "技术部")
        .unwrap()
        .unwrap();
    let market = ContactService::create_org_group_with_id(s, ORG, &hq.id, "og-market", "市场部")
        .unwrap()
        .unwrap();
    let branch = ContactService::create_org_group_with_id(s, ORG, "", "og-branch", "分部")
        .unwrap()
        .unwrap();
    (hq, tech, market, branch)
}

fn tree_ids(tree: &[OrgGroupNode]) -> Vec<&str> {
    tree.iter().map(|n| n.id.as_str()).collect()
}

#[test]
fn org_group_create_rename_and_invalid_parent() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);

    // 父不存在返回 None
    assert!(
        ContactService::create_org_group_with_id(&mut s, ORG, "og-x", "og-ghost", "幽灵部")
            .unwrap()
            .is_none()
    );

    // rename
    ContactService::rename_org_group(&mut s, ORG, &tech.id, "研发部").unwrap();
    ContactService::rename_org_group(&mut s, ORG, "og-x", "无效").unwrap(); // 不存在忽略

    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str(), branch.id.as_str()]);
    let hq_node = &view.group_tree[0];
    assert_eq!(tree_ids(&hq_node.children), vec![tech.id.as_str(), market.id.as_str()]);
    assert_eq!(hq_node.children[0].name, "研发部");
}

#[test]
fn org_group_move_sibling_only() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);
    let fin = ContactService::create_org_group_with_id(&mut s, ORG, &hq.id, "og-fin", "财务部")
        .unwrap()
        .unwrap();

    // 根层重排：[总部, 分部] → [分部, 总部]
    ContactService::move_org_group_sibling(&mut s, ORG, &branch.id, 0).unwrap();
    // 同级内移动：[技术部, 市场部, 财务部] → [市场部, 技术部, 财务部]
    ContactService::move_org_group_sibling(&mut s, ORG, &market.id, 0).unwrap();
    // 不存在忽略；越界夹紧到 len（toIndex == len 表示移到末尾）
    ContactService::move_org_group_sibling(&mut s, ORG, "og-x", 0).unwrap();
    ContactService::move_org_group_sibling(&mut s, ORG, &tech.id, 99).unwrap();
    // 对齐 TS splice 语义（toIndex 以原序为准，源在目标位之前时摘除后前移一位）：
    // [市场部, 财务部, 技术部] 把市场部移到下标 2（原技术部之前）→ [财务部, 市场部, 技术部]
    ContactService::move_org_group_sibling(&mut s, ORG, &market.id, 2).unwrap();

    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![branch.id.as_str(), hq.id.as_str()]);
    assert_eq!(
        tree_ids(&view.group_tree[1].children),
        vec![fin.id.as_str(), market.id.as_str(), tech.id.as_str()]
    );
}

#[test]
fn org_group_delete_promotes_children_and_resets_members() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);
    // 技术部下的孙节点
    let backend = ContactService::create_org_group_with_id(&mut s, ORG, &tech.id, "og-backend", "后端组")
        .unwrap()
        .unwrap();

    // 成员挂到 tech / backend / market
    for (ch, group_id) in [('a', &tech.id), ('b', &backend.id), ('c', &market.id)] {
        ContactService::set_contact_group(&mut s, ORG, &rid(ch), group_id, NOW).unwrap();
    }

    // 删除技术部：后端组提升到总部层；tech/backend 涉及的成员复位，market 不受影响
    ContactService::delete_org_group(&mut s, ORG, &tech.id).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str(), branch.id.as_str()]);
    assert_eq!(
        tree_ids(&view.group_tree[0].children),
        vec![backend.id.as_str(), market.id.as_str()]
    );
    assert_eq!(view.member_extras.get(&rid('a')).unwrap().group_id, "");
    assert_eq!(view.member_extras.get(&rid('b')).unwrap().group_id, "");
    assert_eq!(
        view.member_extras.get(&rid('c')).unwrap().group_id,
        market.id
    );

    // 删除不存在节点忽略
    ContactService::delete_org_group(&mut s, ORG, "og-x").unwrap();
}

#[test]
fn org_group_requires_org_space() {
    let mut s = MemoryStorage::new();
    let err = ContactService::create_org_group_with_id(&mut s, PERSONAL, "", "og-hq", "总部").unwrap_err();
    assert!(matches!(err, ContactError::InvalidSpace));
    let err = ContactService::overview(&s, "weird-space").unwrap_err();
    assert!(matches!(err, ContactError::InvalidSpace));
}

// ------------------------------------------------------------------
// overview 形状
// ------------------------------------------------------------------

#[test]
fn overview_shapes_per_space() {
    let mut s = MemoryStorage::new();

    // 空个人空间
    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert!(view.friends.is_empty());
    assert!(view.requests.is_empty());
    assert!(view.outgoing.is_empty());
    assert!(view.tags.is_empty());
    assert!(view.groups.is_empty());
    assert!(view.group_tree.is_empty());
    assert!(view.member_extras.is_empty());

    // 填充个人空间
    ContactService::upsert_friend(&mut s, &friend(&rid('a'), "阿强")).unwrap();
    ContactService::put_incoming_request(&mut s, &incoming("req-1", &rid('r'))).unwrap();
    ContactService::create_outgoing_request(&mut s, &rid('b'), "博哥", "", "扫码", None, NOW)
        .unwrap();
    ContactService::create_tag_with_id(&mut s, PERSONAL, "tag-1", "邻居").unwrap();
    ContactService::create_group_with_id(&mut s, "group-1", "家人").unwrap();

    let view = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(view.friends.len(), 1);
    assert_eq!(view.requests.len(), 1);
    assert_eq!(view.outgoing.len(), 1);
    assert_eq!(view.tags.len(), 1);
    assert_eq!(view.groups.len(), 1);
    assert!(view.group_tree.is_empty());
    assert!(view.member_extras.is_empty());

    // 组织空间：friends/requests/outgoing/groups 恒空（个人数据不串入）
    let tag = ContactService::create_tag_with_id(&mut s, ORG, "tag-core", "核心成员").unwrap();
    let hq = ContactService::create_org_group_with_id(&mut s, ORG, "", "og-hq", "总部")
        .unwrap()
        .unwrap();
    ContactService::update_profile(
        &mut s,
        ORG,
        &rid('m'),
        ProfilePatch {
            remark: Some("同事甲".to_string()),
            tag_ids: Some(vec![tag.id.clone()]),
            ..Default::default()
        },
        NOW,
    )
    .unwrap();

    let view = ContactService::overview(&s, ORG).unwrap();
    assert!(view.friends.is_empty());
    assert!(view.requests.is_empty());
    assert!(view.outgoing.is_empty());
    assert!(view.groups.is_empty());
    assert_eq!(view.tags.len(), 1);
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str()]);
    assert_eq!(view.member_extras.len(), 1);
    assert_eq!(view.member_extras.get(&rid('m')).unwrap().remark, "同事甲");

    // 两个组织空间相互隔离
    let other = ContactService::overview(&s, "org:org-2").unwrap();
    assert!(other.tags.is_empty());
    assert!(other.group_tree.is_empty());
    assert!(other.member_extras.is_empty());
}

// ------------------------------------------------------------------
// 存储形状（camelCase 紧凑 JSON）
// ------------------------------------------------------------------

#[test]
fn storage_keys_and_camel_case_json() {
    let mut s = MemoryStorage::new();
    use spark_core::storage::StorageBackend;

    let mut f = friend(&rid('a'), "阿强");
    f.gender = Some("male".to_string());
    ContactService::upsert_friend(&mut s, &f).unwrap();
    let raw = s.get(&format!("ct:friend:{}", rid('a'))).unwrap().unwrap();
    assert!(raw.contains("\"rootId\""));
    assert!(raw.contains("\"addedAt\""));
    assert!(raw.contains("\"tagIds\""));
    assert!(raw.contains("\"permission\":\"open\""));
    // None 字段不序列化
    assert!(!raw.contains("\"peer\""));

    ContactService::put_incoming_request(&mut s, &incoming("req-1", &rid('r'))).unwrap();
    let raw = s.get("ct:req:in:req-1").unwrap().unwrap();
    assert!(raw.contains("\"status\":\"pending\""));
    assert!(raw.contains("\"createdAt\""));
    assert!(raw.contains("\"updatedAt\""));

    let extra_key = format!("ct:org:org-1:extra:{}", rid('m'));
    ContactService::set_blocked(&mut s, ORG, &rid('m'), true, NOW).unwrap();
    let raw = s.get(&extra_key).unwrap().unwrap();
    assert!(raw.contains("\"blocked\":true"));
    assert!(raw.contains("\"groupId\":\"\""));
}

#[test]
fn contact_profile_record_default_matches_empty_profile() {
    // 对齐 TS emptyProfile()：permission 默认 "open"，其余空
    let profile = ContactProfileRecord::default();
    assert_eq!(profile.permission, "open");
    assert_eq!(profile.remark, "");
    assert!(!profile.blocked);
    assert!(profile.phones.is_empty());
    assert!(profile.photos.is_empty());
}
