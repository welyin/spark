//! kernel 组织管理集成测试：创建/列表/副本概览/邀请码（自邀拒绝 + 纯逻辑
//! 接受）与成员增删、组织删除。

mod common;

use spark_core::org::invite::{OrgInviteInviter, OrgInvitePayload, encode_org_invite};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::node::system_now_ms;

use common::*;

// ---------------------------------------------------------------------------
// 组织：创建/列表/副本概览/邀请码（自邀拒绝 + 纯逻辑接受）
// ---------------------------------------------------------------------------

#[test]
fn org_create_invite_and_overview() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);

    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "  测试组织  ".to_string(),
            description: Some("描述".to_string()),
            avatar: None,
            base_plugin_domain: Some("plugin:notes".to_string()),
        })
        .unwrap();
    assert_eq!(view.record.name, "测试组织", "组织名 trim");
    assert!(view.is_current_user_admin);
    assert_eq!(view.member_count, 1);
    let org_id = view.record.org_id.clone();

    assert_eq!(kernel.list_orgs().unwrap().len(), 1);

    // 副本概览：本机恒算 1 个副本
    let overview = kernel.org_overview(&org_id).unwrap();
    assert_eq!(overview.replica_target, 3);
    assert_eq!(overview.synced_peers, 1);
    assert_eq!(overview.members.len(), 1);
    assert!(overview.members[0].is_self && overview.members[0].ever_synced);

    // 未启动 p2p 生成邀请码 → 网络不可用（登录即在线：先停再测未启动路径）
    kernel.stop_p2p().unwrap();
    let err = kernel.create_org_invite(&org_id).unwrap_err();
    assert_eq!(
        err.to_string(),
        "本机 P2P 节点尚未启动，请先启动网络后再生成邀请码"
    );

    // 自邀拒绝（直接用 org 模块构造邀请码，纯逻辑路径；inviter 需带节点信息
    // 否则解码阶段先报"缺少邀请人的节点地址"）
    let self_code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "测试组织".to_string(),
        OrgInviteInviter {
            root_id: root_id.clone(),
            peer_id: Some("self-peer-123456".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    let err = kernel.join_by_invite(&self_code).unwrap_err();
    assert_eq!(err.to_string(), "不能接受自己发出的邀请码");

    // 他人邀请码：解码通过（后续连接拉取由壳层完成）
    let other_root = "ab".repeat(32);
    let other_code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "测试组织".to_string(),
        OrgInviteInviter {
            root_id: other_root.clone(),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    let payload = kernel.join_by_invite(&other_code).unwrap();
    assert_eq!(payload.org_id, org_id);
    assert_eq!(payload.inviter.root_id, other_root);
    // 尚未拉取到成员记录 → 确认加入失败（用本机不存在的组织 id 模拟拉取后仍非成员）
    let err = kernel.check_join("org_nonexistent").unwrap_err();
    assert_eq!(
        err.to_string(),
        "未能加入组织：请确认管理员已先将你的 RootID 录入组织成员"
    );

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 组织成员管理：添加/更新 nodeInfo/移除/删除组织
// ---------------------------------------------------------------------------

#[test]
fn org_member_management() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let member_root = "ab".repeat(32);

    // 未解锁一律 Locked
    assert_eq!(
        kernel
            .org_add_member("org_x", &member_root, None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    init_identity(&mut kernel);
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "成员组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:app".to_string()),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    // 添加成员：role 固定 member
    let view = kernel.org_add_member(&org_id, &member_root, None).unwrap();
    assert_eq!(view.member_count, 2);
    assert_eq!(view.admin_count, 1);

    // 重复添加 = 更新 nodeInfo（成员数不变）
    let node = spark_core::org::OrganizationNodeInfo {
        peer_id: Some("12D3KooWMemberPeerX".to_string()),
        addresses: vec!["/ip4/1.2.3.4/tcp/15002".to_string()],
    };
    let view = kernel
        .org_add_member(&org_id, &member_root, Some(&node))
        .unwrap();
    assert_eq!(view.member_count, 2);
    let m = view
        .members
        .iter()
        .find(|m| m.root_id == member_root)
        .unwrap();
    assert_eq!(
        m.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some("12D3KooWMemberPeerX")
    );

    // 移除成员
    let view = kernel.org_remove_member(&org_id, &member_root).unwrap();
    assert_eq!(view.member_count, 1);
    // 移除唯一 admin（自己）→ 拒绝
    let self_root = kernel.current_root_id().unwrap().unwrap();
    assert_eq!(
        kernel
            .org_remove_member(&org_id, &self_root)
            .unwrap_err()
            .to_string(),
        "Organization must keep at least one admin"
    );
    // 未知组织
    assert_eq!(
        kernel
            .org_add_member("org_nope", &member_root, None)
            .unwrap_err()
            .to_string(),
        "Organization not found"
    );

    // 删除组织
    kernel.org_delete(&org_id).unwrap();
    assert!(kernel.list_orgs().unwrap().is_empty());
    assert_eq!(
        kernel.org_delete(&org_id).unwrap_err().to_string(),
        "Organization not found"
    );

    kernel.shutdown().unwrap();
}
