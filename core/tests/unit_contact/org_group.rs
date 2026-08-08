//! 组织分组树（建/改/删/同级重排/跨级移动）（自 unit_contact.rs 拆出，§2.1）。

use super::*;
// ------------------------------------------------------------------
// 组织空间分组树
// ------------------------------------------------------------------

fn seed_tree(s: &mut MemoryStorage) -> (OrgGroupNode, OrgGroupNode, OrgGroupNode, OrgGroupNode) {
    let hq = ContactService::create_org_group_with_id(s, ORG, "", "og-hq", "总部", NOW, NODE)
        .unwrap()
        .unwrap();
    let tech = ContactService::create_org_group_with_id(s, ORG, &hq.id, "og-tech", "技术部", NOW, NODE)
        .unwrap()
        .unwrap();
    let market = ContactService::create_org_group_with_id(s, ORG, &hq.id, "og-market", "市场部", NOW, NODE)
        .unwrap()
        .unwrap();
    let branch = ContactService::create_org_group_with_id(s, ORG, "", "og-branch", "分部", NOW, NODE)
        .unwrap()
        .unwrap();
    (hq, tech, market, branch)
}

#[test]
fn org_group_create_rename_and_invalid_parent() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);

    // 父不存在返回 None
    assert!(
        ContactService::create_org_group_with_id(&mut s, ORG, "og-x", "og-ghost", "幽灵部", NOW, NODE)
            .unwrap()
            .is_none()
    );

    // rename
    ContactService::rename_org_group(&mut s, ORG, &tech.id, "研发部", NOW, NODE).unwrap();
    ContactService::rename_org_group(&mut s, ORG, "og-x", "无效", NOW, NODE).unwrap(); // 不存在忽略

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
    let fin = ContactService::create_org_group_with_id(&mut s, ORG, &hq.id, "og-fin", "财务部", NOW, NODE)
        .unwrap()
        .unwrap();

    // 根层重排：[总部, 分部] → [分部, 总部]
    ContactService::move_org_group_sibling(&mut s, ORG, &branch.id, 0, NOW, NODE).unwrap();
    // 同级内移动：[技术部, 市场部, 财务部] → [市场部, 技术部, 财务部]
    ContactService::move_org_group_sibling(&mut s, ORG, &market.id, 0, NOW, NODE).unwrap();
    // 不存在忽略；越界夹紧到 len（toIndex == len 表示移到末尾）
    ContactService::move_org_group_sibling(&mut s, ORG, "og-x", 0, NOW, NODE).unwrap();
    ContactService::move_org_group_sibling(&mut s, ORG, &tech.id, 99, NOW, NODE).unwrap();
    // 对齐 TS splice 语义（toIndex 以原序为准，源在目标位之前时摘除后前移一位）：
    // [市场部, 财务部, 技术部] 把市场部移到下标 2（原技术部之前）→ [财务部, 市场部, 技术部]
    ContactService::move_org_group_sibling(&mut s, ORG, &market.id, 2, NOW, NODE).unwrap();

    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![branch.id.as_str(), hq.id.as_str()]);
    assert_eq!(
        tree_ids(&view.group_tree[1].children),
        vec![fin.id.as_str(), market.id.as_str(), tech.id.as_str()]
    );
}

#[test]
fn org_group_move_cross_level() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);

    // 根 → 子：分部移入总部下首位 → 根 [总部]，总部子 [分部, 技术部, 市场部]
    ContactService::move_org_group(&mut s, ORG, &branch.id, &hq.id, 0, NOW, NODE).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str()]);
    assert_eq!(
        tree_ids(&view.group_tree[0].children),
        vec![branch.id.as_str(), tech.id.as_str(), market.id.as_str()]
    );

    // 子 → 另一父：技术部移到分部下 → 总部子 [分部, 市场部]，分部子 [技术部]
    ContactService::move_org_group(&mut s, ORG, &tech.id, &branch.id, 0, NOW, NODE).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(
        tree_ids(&view.group_tree[0].children),
        vec![branch.id.as_str(), market.id.as_str()]
    );
    assert_eq!(
        tree_ids(&view.group_tree[0].children[0].children),
        vec![tech.id.as_str()]
    );

    // 子 → 根：技术部移回根层（new_parent_id = ""，越界夹紧到末尾）→ 根 [总部, 技术部]
    ContactService::move_org_group(&mut s, ORG, &tech.id, "", 99, NOW, NODE).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str(), tech.id.as_str()]);
    assert!(view.group_tree[0].children[0].children.is_empty());

    // 目标父不存在 → 静默忽略（树不变）
    ContactService::move_org_group(&mut s, ORG, &tech.id, "og-x", 0, NOW, NODE).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str(), tech.id.as_str()]);

    // 防环：总部移入自己的子树（市场部）或自身 → 静默忽略（树不变）
    ContactService::move_org_group(&mut s, ORG, &hq.id, &market.id, 0, NOW, NODE).unwrap();
    ContactService::move_org_group(&mut s, ORG, &hq.id, &hq.id, 0, NOW, NODE).unwrap();
    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(tree_ids(&view.group_tree), vec![hq.id.as_str(), tech.id.as_str()]);
    assert_eq!(
        tree_ids(&view.group_tree[0].children),
        vec![branch.id.as_str(), market.id.as_str()]
    );

    // 源节点不存在 → 静默忽略
    ContactService::move_org_group(&mut s, ORG, "og-x", "", 0, NOW, NODE).unwrap();
}

#[test]
fn org_group_move_cross_level_same_parent_matches_splice() {
    // new_parent == 源父级（同层）时落点语义须与 move_org_group_sibling /
    // TS splice 一致：toIndex 以原序为准，源在目标位之前时摘除后前移一位
    let mut s = MemoryStorage::new();
    let (hq, tech, market, _branch) = seed_tree(&mut s);
    let fin = ContactService::create_org_group_with_id(&mut s, ORG, &hq.id, "og-fin", "财务部", NOW, NODE)
        .unwrap()
        .unwrap();
    // 总部子：[技术部, 市场部, 财务部]
    // 后移：技术部 → 下标 2（原财务部之前）→ [市场部, 技术部, 财务部]
    ContactService::move_org_group(&mut s, ORG, &tech.id, &hq.id, 2, NOW, NODE).unwrap();
    // 前移：财务部 → 下标 0 → [财务部, 市场部, 技术部]
    ContactService::move_org_group(&mut s, ORG, &fin.id, &hq.id, 0, NOW, NODE).unwrap();
    // toIndex == len 移到末尾：市场部 → 3 → [财务部, 技术部, 市场部]
    ContactService::move_org_group(&mut s, ORG, &market.id, &hq.id, 3, NOW, NODE).unwrap();

    let view = ContactService::overview(&s, ORG).unwrap();
    assert_eq!(
        tree_ids(&view.group_tree[0].children),
        vec![fin.id.as_str(), tech.id.as_str(), market.id.as_str()]
    );
}

#[test]
fn org_group_delete_promotes_children_and_resets_members() {
    let mut s = MemoryStorage::new();
    let (hq, tech, market, branch) = seed_tree(&mut s);
    // 技术部下的孙节点
    let backend = ContactService::create_org_group_with_id(&mut s, ORG, &tech.id, "og-backend", "后端组", NOW, NODE)
        .unwrap()
        .unwrap();

    // 成员挂到 tech / backend / market
    for (ch, group_id) in [('a', &tech.id), ('b', &backend.id), ('c', &market.id)] {
        ContactService::set_contact_group(&mut s, ORG, &rid(ch), group_id, NOW, NODE).unwrap();
    }

    // 删除技术部：后端组提升到总部层；tech/backend 涉及的成员复位，market 不受影响
    ContactService::delete_org_group(&mut s, ORG, &tech.id, NOW, NODE).unwrap();
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
    ContactService::delete_org_group(&mut s, ORG, "og-x", NOW, NODE).unwrap();
}

#[test]
fn org_group_requires_org_space() {
    let mut s = MemoryStorage::new();
    let err = ContactService::create_org_group_with_id(&mut s, PERSONAL, "", "og-hq", "总部", NOW, NODE).unwrap_err();
    assert!(matches!(err, ContactError::InvalidSpace));
    let err = ContactService::overview(&s, "weird-space").unwrap_err();
    assert!(matches!(err, ContactError::InvalidSpace));
}

