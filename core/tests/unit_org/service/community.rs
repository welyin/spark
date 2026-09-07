//! 共同体邀请/接受/退出的服务层链路：`create_community_org_invite`（admin + 域类型
//! 硬规则）、`accept_community_org_invite`（stub 自举 → validate_org_join →
//! kind=org 成员条目 + orgBinding opt-in；幂等/成环/leaf 拒绝）、
//! `list_community_org_members`、`leave_community`（留史 + 名册移除 + 空域
//! 只读档案推导与写拒绝）。

use super::*;

use spark_core::org::community_invite::{
    CommunityOrgInvitePayload, community_domain, decode_community_org_invite_at,
};
use spark_core::org::tx::OrganizationTransactionType;
use spark_core::org::types::{
    DomainType, MemberKind, OrgBinding, OrganizationRole, org_member_key,
};
use spark_core::org::{OrgDomainIdentity, OrgError, OrgInviteInviter, org_root_signing_key};
use spark_core::storage::StorageBackend;

fn community_input() -> CreateOrganizationInput {
    CreateOrganizationInput {
        domain_type: Some(DomainType::Community),
        ..input()
    }
}

fn inviter(admin: &str) -> OrgInviteInviter {
    OrgInviteInviter {
        root_id: admin.to_string(),
        peer_id: Some("12D3KooWAdmin".to_string()),
        addresses: vec![],
    }
}

/// 真实派生接线：组织根私钥 → 共同体域身份 id（org-genesis §4）。
fn member_identity_of(joiner: &OrganizationRecord, community_org_id: &str) -> String {
    let root_key = org_root_signing_key(joiner).expect("创建路径封存根私钥");
    OrgDomainIdentity::derive(&root_key, &community_domain(community_org_id)).identity()
}

#[test]
fn create_invite_admin_and_domain_enforced() {
    let mut storage = MemoryStorage::new();
    let (admin, community) = {
        let record = OrganizationService::create_organization(
            &mut storage,
            &community_input(),
            &root_id_of(MNEMONIC),
            NOW,
        )
        .unwrap();
        (root_id_of(MNEMONIC), record)
    };
    // 非 admin 不能创建
    assert!(matches!(
        OrganizationService::create_community_org_invite(
            &storage,
            &community.org_id,
            &rid('x'),
            inviter(&rid('x')),
            None,
            None,
            NOW,
        ),
        Err(OrgError::AdminRequired)
    ));
    // leaf 域创建组织邀请 → 域类型硬规则拒绝
    let (_admin2, leaf) = setup_org(&mut storage);
    assert!(matches!(
        OrganizationService::create_community_org_invite(
            &storage,
            &leaf.org_id,
            &admin,
            inviter(&admin),
            None,
            None,
            NOW,
        ),
        Err(OrgError::MemberKindNotAllowed(_))
    ));
    // 共同体域正常创建；邀请码可解码且字段正确
    let created = OrganizationService::create_community_org_invite(
        &storage,
        &community.org_id,
        &admin,
        inviter(&admin),
        Some("{\"orgAddress\":\"...\"}".to_string()),
        Some("dGVzdA==".to_string()),
        NOW,
    )
    .unwrap();
    assert_eq!(created.community_org_id, community.org_id);
    let payload = decode_community_org_invite_at(&created.code, NOW).unwrap();
    assert_eq!(payload.community_org_id, community.org_id);
    assert_eq!(payload.community_org_name, "星火 组织");
    assert_eq!(payload.inviter.root_id, admin);
    assert_eq!(
        payload.community_org_address.as_deref(),
        Some("{\"orgAddress\":\"...\"}")
    );
    assert_eq!(payload.reply_domain_id.as_deref(), Some("dGVzdA=="));
}

#[test]
fn accept_writes_org_member_entry_with_optional_binding() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    let joiner =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let identity = member_identity_of(&joiner, &community.org_id);
    let payload = CommunityOrgInvitePayload::new(
        community.org_id.clone(),
        community.name.clone(),
        inviter(&admin),
        NOW,
    );

    // 公开绑定 opt-in = true：orgBinding 携带 joiner orgId + 组织地址
    let outcome = OrganizationService::accept_community_org_invite(
        &mut storage,
        &payload,
        &joiner.org_id,
        &admin,
        &identity,
        true,
        NOW + 1,
        None,
    )
    .unwrap();
    assert_eq!(outcome.community_org_id, community.org_id);
    assert!(!outcome.already_member);
    assert!(!outcome.stub_bootstrapped); // 共同体记录本已存在
    assert_eq!(outcome.member_identity, identity);

    // 名册条目：kind=org、rootId 槽位 = 域身份 id、orgBinding 公开
    let record = OrganizationService::get_record(&storage, &community.org_id)
        .unwrap()
        .unwrap();
    let member = record.find_member(&identity).expect("成员条目已写入");
    assert_eq!(member.member_kind(), MemberKind::Org);
    assert_eq!(
        member.org_binding,
        Some(OrgBinding {
            org_id: Some(joiner.org_id.clone()),
            org_address: joiner.org_address.clone(),
        })
    );
    assert_eq!(member.added_by, admin);
    // per-member 条目双写（阶段四A 分拆口径）
    let raw = storage
        .get(&org_member_key(&community.org_id, &identity))
        .unwrap()
        .expect("成员条目键存在");
    assert!(raw.contains("\"kind\":\"org\""));

    // 重复接受幂等（不产生新写入）
    let again = OrganizationService::accept_community_org_invite(
        &mut storage,
        &payload,
        &joiner.org_id,
        &admin,
        &identity,
        false,
        NOW + 2,
        None,
    )
    .unwrap();
    assert!(again.already_member);
    assert_eq!(
        OrganizationService::list_community_org_members(&storage, &community.org_id)
            .unwrap()
            .len(),
        1
    );

    // 另一组织不公开绑定加入：名册只呈现域身份 id
    let joiner2 =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let identity2 = member_identity_of(&joiner2, &community.org_id);
    OrganizationService::accept_community_org_invite(
        &mut storage,
        &payload,
        &joiner2.org_id,
        &admin,
        &identity2,
        false,
        NOW + 3,
        None,
    )
    .unwrap();
    let members =
        OrganizationService::list_community_org_members(&storage, &community.org_id).unwrap();
    assert_eq!(members.len(), 2);
    let hidden = members.iter().find(|m| m.identity == identity2).unwrap();
    assert_eq!(hidden.org_binding, None);
}

#[test]
fn accept_bootstraps_stub_when_community_record_missing() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let joiner =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let community_id = format!("org_{}", "e".repeat(64));
    let identity = member_identity_of(&joiner, &community_id);
    // 邀请载荷自报共同体（本地无记录——跨组织异步场景的接受侧起点）
    let payload =
        CommunityOrgInvitePayload::new(community_id.clone(), "远方共同体", inviter(&rid('f')), NOW);
    let outcome = OrganizationService::accept_community_org_invite(
        &mut storage,
        &payload,
        &joiner.org_id,
        &admin,
        &identity,
        true,
        NOW,
        None,
    )
    .unwrap();
    assert!(outcome.stub_bootstrapped);
    // stub 记录：域类型 community、零时刻（真实记录到达后覆盖）
    let stub = OrganizationService::get_record(&storage, &community_id)
        .unwrap()
        .unwrap();
    assert_eq!(stub.domain_type, Some(DomainType::Community));
    assert_eq!(stub.updated_at, 0);
    assert_eq!(stub.name, "远方共同体");
    assert!(stub.find_member(&identity).is_some());
}

#[test]
fn accept_guard_errors() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    let joiner =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let identity = member_identity_of(&joiner, &community.org_id);
    let payload = CommunityOrgInvitePayload::new(
        community.org_id.clone(),
        community.name.clone(),
        inviter(&admin),
        NOW,
    );

    // 待加入组织不存在
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &payload,
            "org_nope",
            &admin,
            &identity,
            false,
            NOW,
            None,
        ),
        Err(OrgError::OrganizationNotFound)
    ));
    // 当前身份不是待加入组织 admin
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &payload,
            &joiner.org_id,
            &rid('x'),
            &identity,
            false,
            NOW,
            None,
        ),
        Err(OrgError::AdminRequired)
    ));
    // 域身份 id 形状非法
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &payload,
            &joiner.org_id,
            &admin,
            "not-hex",
            false,
            NOW,
            None,
        ),
        Err(OrgError::InvalidMemberRootId)
    ));

    // leaf 域作为目标：手工构造指向 leaf 的邀请载荷（create 侧已拒，accept
    // 侧的 validate_org_join 仍须独立执法）
    let (_a2, leaf) = setup_org(&mut storage);
    let leaf_payload = CommunityOrgInvitePayload::new(
        leaf.org_id.clone(),
        leaf.name.clone(),
        inviter(&admin),
        NOW,
    );
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &leaf_payload,
            &joiner.org_id,
            &admin,
            &identity,
            false,
            NOW,
            None,
        ),
        Err(OrgError::MemberKindNotAllowed(_))
    ));

    // 自加入（joiner == 目标共同体，二者均为 community 域）→ 成环拒绝
    let self_payload = CommunityOrgInvitePayload::new(
        community.org_id.clone(),
        community.name.clone(),
        inviter(&admin),
        NOW,
    );
    let self_identity = member_identity_of(&community, &community.org_id);
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &self_payload,
            &community.org_id,
            &admin,
            &self_identity,
            false,
            NOW,
            None,
        ),
        Err(OrgError::MembershipCycle)
    ));
}

#[test]
fn accept_enforces_cycle_via_published_bindings() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    // 三个共同体域：B 加入 A，C 加入 B（均公开绑定 → 进成环图）
    let a = OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
        .unwrap();
    let b = OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
        .unwrap();
    let c = OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
        .unwrap();
    let join = |storage: &mut MemoryStorage,
                joiner: &OrganizationRecord,
                target: &OrganizationRecord,
                at: i64| {
        let payload = CommunityOrgInvitePayload::new(
            target.org_id.clone(),
            target.name.clone(),
            inviter(&admin),
            at,
        );
        let identity = member_identity_of(joiner, &target.org_id);
        OrganizationService::accept_community_org_invite(
            storage,
            &payload,
            &joiner.org_id,
            &admin,
            &identity,
            true,
            at,
            None,
        )
    };
    join(&mut storage, &b, &a, NOW + 1).unwrap();
    join(&mut storage, &c, &b, NOW + 2).unwrap();
    // A 再加入 C：C 的可达祖先集 = {B, A}，传递环拒绝
    assert!(matches!(
        join(&mut storage, &a, &c, NOW + 3),
        Err(OrgError::MembershipCycle)
    ));
    // 反向无环：A 的祖先集为空，C 加入 A 放行
    join(&mut storage, &c, &a, NOW + 4).unwrap();
}

// ------------------------------------------------------------------
// 组织退出共同体与空域只读档案（community-model：退出留史；域不可解散，
// 只可退出——全员退出后域成为空域，只读历史档案，无人能写入）
// ------------------------------------------------------------------

/// 加入快捷函数：邀请载荷 + 真实派生域身份接受落库。
fn join(
    storage: &mut MemoryStorage,
    joiner: &OrganizationRecord,
    target: &OrganizationRecord,
    publish_binding: bool,
    at: i64,
) -> String {
    let admin = root_id_of(MNEMONIC);
    let payload = CommunityOrgInvitePayload::new(
        target.org_id.clone(),
        target.name.clone(),
        inviter(&admin),
        at,
    );
    let identity = member_identity_of(joiner, &target.org_id);
    OrganizationService::accept_community_org_invite(
        storage,
        &payload,
        &joiner.org_id,
        &admin,
        &identity,
        publish_binding,
        at,
        None,
    )
    .unwrap();
    identity
}

#[test]
fn leave_flow_appends_history_and_updates_roster() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    // 新建共同体（无成员组织、无留史记录）不是空域档案
    assert!(
        !OrganizationService::is_community_archived(&storage, &community).unwrap(),
        "新建共同体不是空域档案"
    );

    let joiner1 =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let joiner2 =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let id1 = join(&mut storage, &joiner1, &community, true, NOW + 1);
    let id2 = join(&mut storage, &joiner2, &community, false, NOW + 2);

    // joiner1 退出（公开绑定）：留史 + 名册移除，仍有 joiner2 → 未归档
    let out1 = OrganizationService::leave_community(
        &mut storage,
        &community.org_id,
        &joiner1.org_id,
        &admin,
        &id1,
        NOW + 3,
        None,
    )
    .unwrap();
    assert_eq!(out1.community_org_id, community.org_id);
    assert_eq!(out1.member_identity, id1);
    assert!(!out1.domain_archived, "仍有成员组织，未进入档案态");

    let record = OrganizationService::get_record(&storage, &community.org_id)
        .unwrap()
        .unwrap();
    assert!(record.find_member(&id1).is_none(), "名册条目已移除");
    assert!(
        storage
            .get(&org_member_key(&community.org_id, &id1))
            .unwrap()
            .is_none(),
        "per-member 条目已删除"
    );
    assert!(
        !OrganizationService::is_community_archived(&storage, &record).unwrap()
    );

    // 留史记录：append-only，含退出时公开绑定快照
    let leaves = storage
        .scan(&spark_core::storage::ScanOptions::prefix(
            spark_core::org::community_leave_prefix(&community.org_id),
        ))
        .unwrap();
    assert_eq!(leaves.len(), 1);
    let leave: spark_core::org::CommunityLeaveRecord =
        serde_json::from_str(&leaves[0].1).unwrap();
    assert_eq!(leave.leave_v, 1);
    assert_eq!(leave.community_org_id, community.org_id);
    assert_eq!(leave.member_identity, id1);
    assert_eq!(leave.actor_root_id, admin);
    assert_eq!(leave.left_at, NOW + 3);
    assert_eq!(
        leave.org_binding.and_then(|b| b.org_id),
        Some(joiner1.org_id.clone()),
        "公开绑定快照随留史保留"
    );

    // 本地事务审计：member-leave，target = 域身份 id
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &community.org_id, 5)
        .unwrap();
    assert_eq!(txs[0].type_, OrganizationTransactionType::MemberLeave);
    assert_eq!(txs[0].target_root_id.as_deref(), Some(id1.as_str()));

    // joiner2 退出（未公开绑定）：最后一个成员组织 → 域进入空域只读档案
    let out2 = OrganizationService::leave_community(
        &mut storage,
        &community.org_id,
        &joiner2.org_id,
        &admin,
        &id2,
        NOW + 4,
        None,
    )
    .unwrap();
    assert!(out2.domain_archived, "最后一个成员组织退出 → 归档");
    let record = OrganizationService::get_record(&storage, &community.org_id)
        .unwrap()
        .unwrap();
    assert!(
        OrganizationService::is_community_archived(&storage, &record).unwrap(),
        "空域只读档案状态可由既有记录确定性推导"
    );
    // 未公开绑定的退出：留史不记 orgId（不扩大披露面）
    let leaves = storage
        .scan(&spark_core::storage::ScanOptions::prefix(
            spark_core::org::community_leave_prefix(&community.org_id),
        ))
        .unwrap();
    assert_eq!(leaves.len(), 2, "两次退出各留一史（历史不抹除）");
    let leave2: spark_core::org::CommunityLeaveRecord = serde_json::from_str(
        &leaves
            .iter()
            .find(|(_, v)| v.contains(&id2))
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(leave2.org_binding, None);
    // 读路径不受影响：名册（空）、历史事务照常可查
    assert!(
        OrganizationService::list_community_org_members(&storage, &community.org_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn leave_guard_errors() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    let joiner =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let identity = join(&mut storage, &joiner, &community, true, NOW + 1);

    // 退出组织不存在
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            "org_nope",
            &admin,
            &identity,
            NOW + 2,
            None,
        ),
        Err(OrgError::OrganizationNotFound)
    ));
    // 当前身份不是退出组织 admin
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &joiner.org_id,
            &rid('x'),
            &identity,
            NOW + 2,
            None,
        ),
        Err(OrgError::AdminRequired)
    ));
    // 域身份 id 形状非法
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &joiner.org_id,
            &admin,
            "not-hex",
            NOW + 2,
            None,
        ),
        Err(OrgError::InvalidMemberRootId)
    ));
    // 不在名册中的域身份
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &joiner.org_id,
            &admin,
            &rid('9'),
            NOW + 2,
            None,
        ),
        Err(OrgError::NotCommunityMember)
    ));
    // 个人成员条目（创建者 admin）不是组织成员，不能走退出
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &joiner.org_id,
            &admin,
            &admin,
            NOW + 2,
            None,
        ),
        Err(OrgError::NotCommunityMember)
    ));
    // 公开绑定不符：以他人（joiner）的域身份、用另一个组织的名义退出
    let other =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &other.org_id,
            &admin,
            &identity,
            NOW + 2,
            None,
        ),
        Err(OrgError::NotCommunityMember)
    ));
    // 目标为 leaf 域：退出语义只对共同体域成立
    let (_a2, leaf) = setup_org(&mut storage);
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &leaf.org_id,
            &joiner.org_id,
            &admin,
            &identity,
            NOW + 2,
            None,
        ),
        Err(OrgError::MemberKindNotAllowed(_))
    ));
    // 守卫全部失败后成员仍在名册（无半边写入）
    assert!(
        OrganizationService::get_record(&storage, &community.org_id)
            .unwrap()
            .unwrap()
            .find_member(&identity)
            .is_some()
    );
}

#[test]
fn archived_domain_rejects_writes_and_keeps_reads() {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    let joiner =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let identity = join(&mut storage, &joiner, &community, true, NOW + 1);
    let out = OrganizationService::leave_community(
        &mut storage,
        &community.org_id,
        &joiner.org_id,
        &admin,
        &identity,
        NOW + 2,
        None,
    )
    .unwrap();
    assert!(out.domain_archived);

    // 组织记录更新
    assert!(matches!(
        OrganizationService::update_org_info(
            &mut storage,
            &community.org_id,
            Some("改名"),
            None,
            None,
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    // 网关/数据账号/公开标志
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &community.org_id,
            &[admin.clone()],
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    assert!(matches!(
        OrganizationService::set_org_data_accounts(
            &mut storage,
            &community.org_id,
            &[admin.clone()],
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    assert!(matches!(
        OrganizationService::set_org_public(
            &mut storage,
            &community.org_id,
            true,
            None,
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    // 成员变更（移除/角色/自写身份）
    assert!(matches!(
        OrganizationService::remove_member(&mut storage, &community.org_id, &admin, &admin, NOW + 3,),
        Err(OrgError::CommunityDomainArchived)
    ));
    assert!(matches!(
        OrganizationService::set_member_role(
            &mut storage,
            &community.org_id,
            &admin,
            OrganizationRole::Member,
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    assert!(matches!(
        OrganizationService::update_my_identity(
            &mut storage,
            &community.org_id,
            &spark_core::org::service::OrgIdentityPatch {
                nickname: Some("新昵称".to_string()),
                ..Default::default()
            },
            &admin,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    // 邀请与加入（空域不再产生新成员关系）
    assert!(matches!(
        OrganizationService::create_community_org_invite(
            &storage,
            &community.org_id,
            &admin,
            inviter(&admin),
            None,
            None,
            NOW + 3,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    let latecomer =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let late_identity = member_identity_of(&latecomer, &community.org_id);
    let payload = CommunityOrgInvitePayload::new(
        community.org_id.clone(),
        community.name.clone(),
        inviter(&admin),
        NOW + 3,
    );
    assert!(matches!(
        OrganizationService::accept_community_org_invite(
            &mut storage,
            &payload,
            &latecomer.org_id,
            &admin,
            &late_identity,
            true,
            NOW + 3,
            None,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));
    // 已归档域上的再次退出：档案错误先于成员缺失（无人能写入）
    assert!(matches!(
        OrganizationService::leave_community(
            &mut storage,
            &community.org_id,
            &joiner.org_id,
            &admin,
            &identity,
            NOW + 4,
            None,
        ),
        Err(OrgError::CommunityDomainArchived)
    ));

    // 读路径不受影响：记录/名册（空）/历史事务/留史均可查
    let record = OrganizationService::get_record(&storage, &community.org_id)
        .unwrap()
        .unwrap();
    assert!(OrganizationService::is_community_archived(&storage, &record).unwrap());
    assert!(
        OrganizationService::list_community_org_members(&storage, &community.org_id)
            .unwrap()
            .is_empty()
    );
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &community.org_id, 20)
        .unwrap();
    assert!(
        txs.iter()
            .any(|t| t.type_ == OrganizationTransactionType::MemberLeave),
        "退出事务留痕可读"
    );
    assert!(
        !storage
            .scan(&spark_core::storage::ScanOptions::prefix(
                spark_core::org::community_leave_prefix(&community.org_id),
            ))
            .unwrap()
            .is_empty(),
        "留史记录可读"
    );
}

#[test]
fn rejoin_after_partial_leave_unarchives() {
    // 部分退出（仍有成员组织在册）不进入档案态，留史保留但域可写
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let community =
        OrganizationService::create_organization(&mut storage, &community_input(), &admin, NOW)
            .unwrap();
    let joiner1 =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let joiner2 =
        OrganizationService::create_organization(&mut storage, &input(), &admin, NOW).unwrap();
    let id1 = join(&mut storage, &joiner1, &community, true, NOW + 1);
    join(&mut storage, &joiner2, &community, true, NOW + 2);
    OrganizationService::leave_community(
        &mut storage,
        &community.org_id,
        &joiner1.org_id,
        &admin,
        &id1,
        NOW + 3,
        None,
    )
    .unwrap();
    let record = OrganizationService::get_record(&storage, &community.org_id)
        .unwrap()
        .unwrap();
    assert!(
        !OrganizationService::is_community_archived(&storage, &record).unwrap(),
        "仍有成员组织：留史存在但不构成空域档案"
    );
    // 域仍可写（邀请创建正常）
    assert!(
        OrganizationService::create_community_org_invite(
            &storage,
            &community.org_id,
            &admin,
            inviter(&admin),
            None,
            None,
            NOW + 4,
        )
        .is_ok()
    );
}
