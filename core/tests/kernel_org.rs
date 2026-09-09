//! kernel 组织管理集成测试：创建/列表/副本概览/邀请码（自邀拒绝 + 纯逻辑
//! 接受）与成员增删、组织删除。

mod common;

use spark_core::org::OrganizationService;
use spark_core::org::invite::{OrgInviteInviter, OrgInvitePayload, encode_org_invite};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::node::system_now_ms;
use spark_core::storage::StorageBackend;

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
            ..Default::default()
        })
        .unwrap();
    assert_eq!(view.record.name, "测试组织", "组织名 trim");
    assert!(view.is_current_user_admin);
    assert_eq!(view.member_count, 1);
    let org_id = view.record.org_id.clone();

    // A16：创建者即发布 accessKey（写一次），org_user_id 可派生且 ≠ rootId。
    {
        let s = kernel.__test_storage().unwrap();
        let record = OrganizationService::get_record(&s, &org_id).unwrap().unwrap();
        let me = &record.members[0];
        let access_key = me.access_key.as_ref().expect("创建即发布 accessKey");
        let uid = me.org_user_id().expect("org_user_id 可派生");
        assert_eq!(uid.len(), 64);
        assert_ne!(uid, root_id, "org_user_id 不泄露 rootId 关联");
        assert!(
            spark_core::org::access_key::verify_access_key_binding(
                &org_id,
                &root_id,
                access_key
            ),
            "自发布验绑通过"
        );
        // 幂等：重复发布不覆盖（写一次）。
        assert!(
            !OrganizationService::publish_access_key(
                &mut s.clone(),
                &org_id,
                &root_id,
                access_key.clone(),
                system_now_ms(),
            )
            .unwrap(),
            "已存在不覆盖"
        );
    }

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
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    // 添加成员：role 固定 member
    let view = kernel.org_add_member(&org_id, &member_root, None).unwrap();
    assert_eq!(view.member_count, 2);
    assert_eq!(view.admin_count, 1);

    // 重复添加 = 更新 nodeInfo（成员数不变）
    let node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
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
        m.node_info
            .as_ref()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .peer_id
            .as_deref(),
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

    // 退出组织（A13：删除通路已移除，域只可退出）——最后一名成员退出
    // 即成空域：组织记录保留（只读档案），本机「我的组织」列表为空
    kernel.org_leave(&org_id).unwrap();
    assert!(kernel.list_orgs().unwrap().is_empty());
    assert_eq!(
        kernel.org_leave(&org_id).unwrap_err().to_string(),
        "Member not found"
    );

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 阶段四A P2（L3）：org-member-removed 移除通知通道
// ---------------------------------------------------------------------------

/// 双 kernel 全链：A 移除 B → B 收 `org-member-removed` 定向通知 → 本地擦除
/// （whole + 成员条目 + orgq 现场）。B 已出成员表，orgsync 不再覆盖 B——
/// 该 dm 是移除的**唯一**主动通道（成员条目墓碑收敛兜底）。
#[test]
fn org_member_removed_notify_wipes_local_org() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B（带 B 的真实 nodeInfo）→ P3：组织到达走邀请流
    // （org-share 推送已停发）——A 发 DM 邀请，B 应答 accept 完成 join
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "移除组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:app".to_string()),
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some(kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap()),
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a
        .org_add_member(&org_id, &root_b, Some(&b_node))
        .unwrap();
    kernel_a
        .org_send_invite(
            &org_id,
            &root_b,
            b_node.peer_id.as_deref(),
            &b_node.addresses,
            None,
        )
        .unwrap();
    wait_until(
        || {
            kernel_b
                .org_invite_records(&org_id)
                .map(|rs| {
                    rs.iter()
                        .any(|r| r.direction == spark_core::org::OrgInviteDirection::Incoming)
                })
                .unwrap_or(false)
        },
        15_000,
        "B 收到 org-invite",
    );
    let invite = kernel_b
        .org_invite_records(&org_id)
        .unwrap()
        .into_iter()
        .find(|r| r.direction == spark_core::org::OrgInviteDirection::Incoming)
        .expect("入站邀请已落库");
    kernel_b.org_respond_invite(&invite.id, true).unwrap();
    assert_eq!(kernel_b.list_orgs().unwrap().len(), 1, "B 已加入组织");

    // A 移除 B → B 收通知 → 本地擦除
    kernel_a.org_remove_member(&org_id, &root_b).unwrap();
    wait_until(
        || kernel_b.list_orgs().map(|l| l.is_empty()).unwrap_or(false),
        20_000,
        "B 收 org-member-removed 后本地组织擦除",
    );
    // 成员条目一并擦除（wipe_org_local：值与 pmeta 同删）
    let leftover = kernel_b
        .__test_storage()
        .unwrap()
        .scan(&spark_core::storage::ScanOptions::prefix(
            spark_core::org::types::ORG_MEMBER_PREFIX,
        ))
        .unwrap();
    assert!(leftover.is_empty(), "成员条目随移除擦除");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// 入站校验：非 admin 发送的 org-member-removed 不擦除（伪造面）；重复通知
/// （本地已无记录）幂等通过。
#[test]
fn org_member_removed_inbound_validation() {
    use sha2::{Digest as _, Sha256};
    use spark_core::kernel::{dm_envelope, handle_inbound_dm};
    use spark_core::storage::MemoryStorage;

    // 身份：rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致）
    let identity = |seed: u8| {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let root = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
        (key, root)
    };
    let (a_key, a_root) = identity(1);
    let (_b_key, b_root) = identity(2);
    let (c_key, c_root) = identity(3);
    let now = system_now_ms();

    let mut s = MemoryStorage::new();
    let record = OrganizationService::create_organization(
        &mut s,
        &CreateOrganizationInput {
            name: "t".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        },
        &a_root,
        now,
    )
    .unwrap();
    let org_id = record.org_id.clone();
    OrganizationService::add_member(&mut s, &org_id, &b_root, None, &a_root, now).unwrap();
    OrganizationService::add_member(&mut s, &org_id, &c_root, None, &a_root, now).unwrap();
    let deliver = |s: &mut MemoryStorage, key: &ed25519_dalek::SigningKey, from: &str| {
        let envelope = dm_envelope::build_envelope(
            dm_envelope::KIND_ORG_MEMBER_REMOVED,
            from,
            &b_root,
            now,
            serde_json::json!({ "orgId": org_id, "targetPeerIds": ["peer-b"] }),
            key,
        );
        handle_inbound_dm(
            s,
            &b_root,
            "B",
            envelope,
            "peer-a",
            &std::collections::HashSet::new(),
            now,
            "node-b",
            None,
        )
        .unwrap()
    };

    // 非 admin（C 是普通成员）发送 → rejected，组织保留
    let r = deliver(&mut s, &c_key, &c_root);
    assert_eq!(
        r.response,
        serde_json::json!({ "ok": false, "reason": "rejected" })
    );
    assert!(
        OrganizationService::get_record(&s, &org_id)
            .unwrap()
            .is_some(),
        "伪造通知不擦除"
    );

    // admin（A）发送 → 擦除 + OrgRemoved 事件
    let r = deliver(&mut s, &a_key, &a_root);
    assert_eq!(r.response, serde_json::json!({ "ok": true }));
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, spark_core::p2p::P2pEvent::OrgRemoved(_))),
        "擦除后发 OrgRemoved 事件"
    );
    assert!(
        OrganizationService::get_record(&s, &org_id)
            .unwrap()
            .is_none(),
        "whole 已擦除"
    );
    let leftover = s
        .scan(&spark_core::storage::ScanOptions::prefix(
            spark_core::org::types::ORG_MEMBER_PREFIX,
        ))
        .unwrap();
    assert!(leftover.is_empty(), "成员条目（值+pmeta）已擦除");

    // 重复通知（本地已无记录）→ 幂等通过
    let r = deliver(&mut s, &a_key, &a_root);
    assert_eq!(
        r.response,
        serde_json::json!({ "ok": true }),
        "重复通知幂等"
    );
}

// ---------------------------------------------------------------------------
// 组织身份与组织 logo：update_my_identity / update_org_info avatar
// ---------------------------------------------------------------------------

const ORG_LOGO: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";

#[test]
fn org_update_my_identity_self_only_and_view_readback() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);

    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "身份组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    // 加一名成员，验证只改自己的记录
    let member_root = "cd".repeat(32);
    kernel.org_add_member(&org_id, &member_root, None).unwrap();

    let patch = spark_core::org::service::OrgIdentityPatch {
        nickname: Some("小火".to_string()),
        avatar: Some(Some(ORG_LOGO.to_string())),
        signature: Some("保持热爱".to_string()),
        use_personal_identity: Some(true),
        ..Default::default()
    };
    let view = kernel.org_update_my_identity(&org_id, &patch).unwrap();
    let me = view.members.iter().find(|m| m.root_id == root_id).unwrap();
    assert_eq!(me.nickname.as_deref(), Some("小火"));
    assert_eq!(me.avatar.as_deref(), Some(ORG_LOGO));
    assert_eq!(me.signature.as_deref(), Some("保持热爱"));
    assert_eq!(me.use_personal_identity, Some(true));
    let other = view
        .members
        .iter()
        .find(|m| m.root_id == member_root)
        .unwrap();
    assert_eq!(other.nickname, None, "他人成员记录不可改");
    assert_eq!(
        other.use_personal_identity, None,
        "未设置 = None（视同 false）"
    );

    // 视图回读（list_orgs 持久化后再读）
    let views = kernel.list_orgs().unwrap();
    let me = views[0]
        .members
        .iter()
        .find(|m| m.root_id == root_id)
        .unwrap();
    assert_eq!(me.nickname.as_deref(), Some("小火"));
    assert_eq!(me.use_personal_identity, Some(true));

    kernel.shutdown().unwrap();
}

#[test]
fn org_update_info_avatar_patch() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);

    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "logo 组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    assert_eq!(view.record.avatar, "", "新建无 logo 为空串");

    // None = 不变
    let view = kernel.org_update_info(&org_id, None, None, None).unwrap();
    assert_eq!(view.record.avatar, "");

    // Some(非空) = 设置（视图带出 avatar）
    let view = kernel
        .org_update_info(&org_id, None, None, Some(ORG_LOGO))
        .unwrap();
    assert_eq!(view.record.avatar, ORG_LOGO);

    // 非法 logo 拒绝（非 data:image/ 前缀）
    assert!(
        kernel
            .org_update_info(&org_id, None, None, Some("https://x.png"))
            .is_err(),
        "非 data URL 的 logo 应被拒绝"
    );

    // None = 不变（已设置的值保留）
    let view = kernel.org_update_info(&org_id, None, None, None).unwrap();
    assert_eq!(view.record.avatar, ORG_LOGO);

    // Some("") = 清除
    let view = kernel
        .org_update_info(&org_id, None, None, Some(""))
        .unwrap();
    assert_eq!(view.record.avatar, "");

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// A16 切片三（membership §4.4-4）：存量迁移执行——unlock 时 accessKey 补齐
// ---------------------------------------------------------------------------

/// 存量形态（whole 与条目均无 accessKey）经 unlock 迁移补齐：seed 确定性
/// 派生 + 写一次发布（whole 与 per-member 条目双写），验绑通过、org_user_id
/// 双键可解析；再次 unlock 幂等不放大。
#[test]
fn access_key_backfill_on_unlock_restores_legacy_member() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "迁移组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    // 模拟存量形态：whole 与 per-member 条目都剥掉 accessKey（A16 前成员）
    {
        let mut s = kernel.__test_storage().unwrap();
        let mut record = OrganizationService::get_record(&s, &org_id)
            .unwrap()
            .expect("组织记录存在");
        record.members[0].access_key = None;
        OrganizationService::save_record(&mut s, &record).unwrap();
        s.put(
            &spark_core::org::types::org_member_key(&org_id, &root_id),
            &serde_json::to_string(&record.members[0]).unwrap(),
        )
        .unwrap();
    }

    // unlock 触发存量迁移（lock 后重解，幂等路径同生产）
    kernel.lock();
    kernel.unlock(PASSWORD, None).unwrap();

    let s = kernel.__test_storage().unwrap();
    let record = OrganizationService::get_record(&s, &org_id)
        .unwrap()
        .expect("组织记录存在");
    let me = &record.members[0];
    let access_key = me.access_key.as_ref().expect("unlock 迁移补齐 accessKey");
    assert!(
        spark_core::org::access_key::verify_access_key_binding(&org_id, &root_id, access_key),
        "补齐的 accessKey 验绑通过"
    );
    let uid = me.org_user_id().expect("org_user_id 可派生");
    assert!(
        record.find_member_any_key(&uid).is_some(),
        "org_user_id 双键命中名册"
    );
    // per-member 条目双写同步补齐
    let raw = s
        .get(&spark_core::org::types::org_member_key(&org_id, &root_id))
        .unwrap()
        .expect("成员条目存在");
    let entry: spark_core::org::types::OrganizationMember =
        serde_json::from_str(&raw).unwrap();
    assert_eq!(
        entry.access_key.as_ref(),
        Some(access_key),
        "条目与 whole 同步补齐"
    );

    // 再次 unlock 幂等（写一次，不放大不覆盖）
    let published = access_key.clone();
    kernel.lock();
    kernel.unlock(PASSWORD, None).unwrap();
    let s = kernel.__test_storage().unwrap();
    let record = OrganizationService::get_record(&s, &org_id).unwrap().unwrap();
    assert_eq!(
        record.members[0].access_key.as_ref(),
        Some(&published),
        "再次 unlock 幂等不覆盖"
    );

    kernel.shutdown().unwrap();
}
