//! kernel 共同体域编排集成测试（组织加入共同体的邀请/接受流 + 退出留史）：
//! 守卫错误（锁定/非 admin/域类型）+ 单 kernel 全流程（创建邀请 → 接受落库
//! → 成员列表 → 退出归档；公开绑定 opt-in、幂等、成环拒绝）。

mod common;

use spark_core::org::service::CreateOrganizationInput;
use spark_core::org::{DomainType, OrganizationService};
use spark_core::storage::StorageBackend;

use common::*;

fn community_input(name: &str) -> CreateOrganizationInput {
    CreateOrganizationInput {
        name: name.to_string(),
        domain_type: Some(DomainType::Community),
        ..Default::default()
    }
}

fn leaf_input(name: &str) -> CreateOrganizationInput {
    CreateOrganizationInput {
        name: name.to_string(),
        ..Default::default()
    }
}

#[test]
fn community_ops_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 未解锁：创建/接受报 Locked
    assert_eq!(
        kernel
            .community_create_invite("org_x")
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );
    assert_eq!(
        kernel
            .community_accept_invite("org_x", "whatever", false)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );
    assert_eq!(
        kernel
            .community_leave("org_x", "org_y")
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );
    // 未初始化：列成员报错（查询口径，不要求解锁）
    assert!(kernel.community_list_members("org_x").is_err());

    let (_root_id, _) = init_identity(&mut kernel);

    // 坏邀请码 → 解码错误
    assert!(
        kernel
            .community_accept_invite("org_x", "not-a-code", false)
            .is_err()
    );
    // 组织不存在
    assert_eq!(
        kernel
            .community_create_invite("org_nope")
            .unwrap_err()
            .to_string(),
        "Organization not found"
    );
    assert!(kernel.community_list_members("org_nope").is_err());

    kernel.shutdown().unwrap();
}

#[test]
fn community_invite_accept_full_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);

    let community = kernel.create_org(community_input("阳光共同体")).unwrap();
    let joiner = kernel.create_org(leaf_input("银杏组织")).unwrap();
    let community_id = community.record.org_id.clone();
    let joiner_id = joiner.record.org_id.clone();

    // leaf 域不能创建共同体邀请（域类型硬规则）
    assert_eq!(
        kernel
            .community_create_invite(&joiner_id)
            .unwrap_err()
            .to_string(),
        "Leaf domain only accepts person members"
    );

    // 创建邀请码（p2p 在线：邀请人携带节点线索 + 回信域身份）
    let created = kernel.community_create_invite(&community_id).unwrap();
    assert_eq!(created.community_org_id, community_id);

    // 接受（公开绑定 opt-in = true）：域身份入册 + orgBinding 公开
    let accepted = kernel
        .community_accept_invite(&joiner_id, &created.code, true)
        .unwrap();
    assert_eq!(accepted.community_org_id, community_id);
    assert!(!accepted.already_member);
    // 邀请载荷未带回信寻址（共同体未发布地址记录）→ 通知跳过，不阻塞落库
    assert!(!accepted.notice_sent);
    assert_eq!(accepted.member_identity.len(), 64);

    // 域身份与组织根密钥派生一致（OrgDomainIdentity 生产接线核账）
    let storage = kernel.__test_storage().unwrap();
    let joiner_record = OrganizationService::get_record(&storage, &joiner_id)
        .unwrap()
        .unwrap();
    let root_key = spark_core::org::org_root_signing_key(&joiner_record).unwrap();
    let expected = spark_core::org::OrgDomainIdentity::derive(
        &root_key,
        &spark_core::org::community_domain(&community_id),
    )
    .identity();
    assert_eq!(accepted.member_identity, expected);

    // 列成员：kind=org 条目 + 公开绑定
    let members = kernel.community_list_members(&community_id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].identity, expected);
    let binding = members[0].org_binding.clone().expect("公开绑定已写入");
    assert_eq!(binding.org_id.as_deref(), Some(joiner_id.as_str()));
    assert!(binding.org_address.is_some());

    // stub 不涉及（共同体记录本地已存在）；重复接受幂等
    let again = kernel
        .community_accept_invite(&joiner_id, &created.code, false)
        .unwrap();
    assert!(again.already_member);
    assert_eq!(
        kernel.community_list_members(&community_id).unwrap().len(),
        1
    );

    // 另一组织不公开绑定加入：名册只见域身份 id
    let joiner2 = kernel.create_org(leaf_input("青松组织")).unwrap();
    let accepted2 = kernel
        .community_accept_invite(&joiner2.record.org_id, &created.code, false)
        .unwrap();
    let members = kernel.community_list_members(&community_id).unwrap();
    assert_eq!(members.len(), 2);
    let hidden = members
        .iter()
        .find(|m| m.identity == accepted2.member_identity)
        .unwrap();
    assert!(hidden.org_binding.is_none());

    kernel.shutdown().unwrap();
}

#[test]
fn community_accept_rejects_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);

    // A/B/C 均为共同体域：B 加入 A、C 加入 B（公开绑定进成环图）
    let a = kernel.create_org(community_input("共同体A")).unwrap();
    let b = kernel.create_org(community_input("共同体B")).unwrap();
    let c = kernel.create_org(community_input("共同体C")).unwrap();
    let (a_id, b_id, c_id) = (
        a.record.org_id.clone(),
        b.record.org_id.clone(),
        c.record.org_id.clone(),
    );

    let code_a = kernel.community_create_invite(&a_id).unwrap().code;
    let code_b = kernel.community_create_invite(&b_id).unwrap().code;
    kernel
        .community_accept_invite(&b_id, &code_a, true)
        .unwrap();
    kernel
        .community_accept_invite(&c_id, &code_b, true)
        .unwrap();

    // A 加入 C：传递环（C→B→A），落库前 validate_org_join 拒绝
    let code_c = kernel.community_create_invite(&c_id).unwrap().code;
    assert_eq!(
        kernel
            .community_accept_invite(&a_id, &code_c, true)
            .unwrap_err()
            .to_string(),
        "Membership would create a cycle"
    );
    // 拒绝后 A 不在 C 的名册中
    assert!(
        kernel
            .community_list_members(&c_id)
            .unwrap()
            .iter()
            .all(
                |m| m.org_binding.as_ref().and_then(|b| b.org_id.as_deref()) != Some(a_id.as_str())
            )
    );

    kernel.shutdown().unwrap();
}

// ------------------------------------------------------------------
// 组织退出共同体与空域只读档案（community-model：退出留史；全员退出后
// 域成为空域——只读历史档案，无人能写入）
// ------------------------------------------------------------------

#[test]
fn community_leave_full_flow_and_archive() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);

    let community = kernel.create_org(community_input("阳光共同体")).unwrap();
    let joiner = kernel.create_org(leaf_input("银杏组织")).unwrap();
    let joiner2 = kernel.create_org(leaf_input("青松组织")).unwrap();
    let community_id = community.record.org_id.clone();
    let joiner_id = joiner.record.org_id.clone();
    let joiner2_id = joiner2.record.org_id.clone();

    let code = kernel.community_create_invite(&community_id).unwrap().code;
    let accepted = kernel
        .community_accept_invite(&joiner_id, &code, true)
        .unwrap();

    // 非成员组织退出 → NotCommunityMember
    assert_eq!(
        kernel
            .community_leave(&community_id, &joiner2_id)
            .unwrap_err()
            .to_string(),
        "该组织不是此共同体成员"
    );

    // 退出：域身份与加入时名册条目一致；单成员退出即归档
    let left = kernel.community_leave(&community_id, &joiner_id).unwrap();
    assert_eq!(left.community_org_id, community_id);
    assert_eq!(left.member_identity, accepted.member_identity);
    assert!(left.domain_archived, "最后一个成员组织退出 → 归档");
    assert!(
        kernel
            .community_list_members(&community_id)
            .unwrap()
            .is_empty(),
        "退出后名册无组织成员（读路径正常）"
    );

    // 留史记录已落库（org:cleave: append-only）
    let storage = kernel.__test_storage().unwrap();
    let leaves = storage
        .scan(&spark_core::storage::ScanOptions::prefix(
            spark_core::org::community_leave_prefix(&community_id),
        ))
        .unwrap();
    assert_eq!(leaves.len(), 1, "退出留史记录已追加");

    // 空域只读档案：写路径一律拒绝（明确错误），读路径不受影响
    assert_eq!(
        kernel
            .community_create_invite(&community_id)
            .unwrap_err()
            .to_string(),
        "共同体域已是空域只读档案（全员已退出，历史保留，无法写入）"
    );
    assert_eq!(
        kernel
            .community_leave(&community_id, &joiner_id)
            .unwrap_err()
            .to_string(),
        "共同体域已是空域只读档案（全员已退出，历史保留，无法写入）"
    );
    // 历史查询正常：成员列表（空）与域身份历史可读
    assert!(kernel.community_list_members(&community_id).is_ok());

    kernel.shutdown().unwrap();
}
