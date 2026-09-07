//! kernel 接受邀请与组织同步编排集成测试：accept_invite 守卫/全流程
//! （双 kernel 互连对跑，P2 orgsync 收敛通道）与组织到达/邀请应答。

mod common;

use std::time::Duration;

use spark_core::org::invite::{OrgInviteInviter, OrgInvitePayload, encode_org_invite};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::org::{
    OrgInviteDirection, OrgInviteRecord, OrgInviteStatus, OrganizationNodeInfo, OrganizationService,
};
use spark_core::p2p::node::system_now_ms;
use spark_core::storage::StorageBackend;

use common::*;

#[test]
fn accept_invite_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 未解锁 → Locked
    assert_eq!(
        kernel.accept_invite("whatever").unwrap_err().to_string(),
        "Root identity is locked"
    );

    let (root_id, _) = init_identity(&mut kernel);
    // 登录即在线：init_identity 已自动启动 p2p；本用例覆盖未启动路径，先停
    kernel.stop_p2p().unwrap();

    // 坏邀请码 → 解析错误（发生在 p2p 检查之前，对齐 TS 先解码）
    assert!(kernel.accept_invite("not-a-code").is_err());

    // 自邀拒绝（p2p 未启动也先报自邀，对齐 TS 校验顺序）
    let self_code = encode_org_invite(&OrgInvitePayload::new(
        "org_selfinvite1".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id,
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&self_code).unwrap_err().to_string(),
        "不能接受自己发出的邀请码"
    );

    // 合法他人邀请码但 p2p 未启动 → TS 文案
    let other_code = encode_org_invite(&OrgInvitePayload::new(
        "org_otherinvite".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id: "cd".repeat(32),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&other_code).unwrap_err().to_string(),
        "P2P 网络未启动，无法通过邀请码加入"
    );

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 阶段③c 组织同步编排：双 kernel 互连对跑
// （组织到达 / accept_invite 全流程）
// ---------------------------------------------------------------------------

/// P3 组织到达通道：A 预录 B + DM 邀请 → B accept（P2 join：stub + orgsync
/// 收敛）→ B 落库 + A 经 orgsync 活动反哺记账（ever_synced）。（前身：
/// org-share 推送编排用例——P3 出站停发后 org-share 不再发送。）
#[test]
fn org_share_push_delivers_between_kernels() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织并把 B 预录为成员（带 B 的真实 nodeInfo）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "推送组织".to_string(),
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

    // P3：组织到达走邀请流（org-share 推送已停发；预录后 B 不会自动收到——
    // A 发 DM 邀请，B 应答 accept 完成 join）
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
                        .any(|r| r.direction == OrgInviteDirection::Incoming)
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
        .find(|r| r.direction == OrgInviteDirection::Incoming)
        .expect("入站邀请已落库");
    kernel_b.org_respond_invite(&invite.id, true).unwrap();

    // B 落库（join 收敛完成）：成员数 2、B 为 member 角色
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert_eq!(mine_b[0].record.org_id, org_id);
    assert_eq!(mine_b[0].member_count, 2);
    assert!(!mine_b[0].is_current_user_admin, "B 为 member 角色");
    let members_a = kernel_a.list_orgs().unwrap();
    assert_eq!(members_a[0].member_count, 2);

    // A 对 B 的记账经 orgsync 活动反哺（join 的 hello/data 交换已发生）
    wait_until(
        || {
            kernel_a
                .org_overview(&org_id)
                .ok()
                .and_then(|o| o.members.into_iter().find(|m| m.root_id == root_b))
                .is_some_and(|e| e.ever_synced)
        },
        15_000,
        "A 侧 ever_synced 经 orgsync 活动记账",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// P2 join 新通道全流程：双 kernel accept_invite（邀请方也是 kernel）——
/// stub 自举 + 即时 orgsync-hello → 邀请人回推 data → 收敛落库确认；
/// 成员自写条目取代 claim 回填（B 的端点经 org:member 条目扩散到 A）。
#[test]
fn accept_invite_two_kernels_full() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B（无 nodeInfo——P2 起由 B 自写条目扩散端点）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "邀请组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:app".to_string()),
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    kernel_a.org_add_member(&org_id, &root_b, None).unwrap();

    // 邀请码：A 的 rootId + 真实节点信息
    let code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "邀请组织".to_string(),
        OrgInviteInviter {
            root_id: root_a.clone(),
            peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
            addresses: dialable_addrs(&kernel_a),
        },
        system_now_ms(),
    ));

    // B 接受邀请：connect → stub 自举 + orgsync-hello → 收敛 → 落库确认
    let acceptance = kernel_b.accept_invite(&code).unwrap();
    assert_eq!(acceptance.org_id, org_id);
    assert_eq!(acceptance.member_count, 2);
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert!(!mine_b[0].is_current_user_admin);
    // B 侧 stub 已与真实记录合并（updatedAt 取真实值，非 stub 的 0）
    assert!(mine_b[0].record.updated_at > 0, "stub 已被真实版本合并");
    // B 的成员条目已自写（含本机端点；L2 claim 退役的取代通道）
    let b_entry_key = spark_core::org::types::org_member_key(&org_id, &root_b);
    let b_entry_raw = kernel_b
        .__test_storage()
        .unwrap()
        .get(&b_entry_key)
        .unwrap()
        .expect("B 自写成员条目");
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();
    assert!(
        b_entry_raw.contains(&b_peer),
        "B 条目携带本机 peerId（端点自写）"
    );

    // A 侧经 orgsync 收敛见到 B 的端点（装配视图：B 条目覆盖 whole）——
    // 取代原「claim 回填 B 的 nodeInfo」断言（claim 通道已退役）
    wait_until(
        || {
            kernel_a
                .list_orgs()
                .ok()
                .and_then(|orgs| {
                    orgs[0]
                        .members
                        .iter()
                        .find(|m| m.root_id == root_b)
                        .and_then(|m| m.node_info.clone())
                })
                .is_some_and(|set| {
                    set.iter()
                        .any(|e| e.peer_id.as_deref() == Some(b_peer.as_str()))
                })
        },
        30_000,
        "A 侧经 orgsync 收敛见到 B 的端点（成员自写条目）",
    );

    // A 再加一名成员：触发向已知成员推送 → B 收到更新（成员数 3）
    let root_c = "cd".repeat(32);
    kernel_a.org_add_member(&org_id, &root_c, None).unwrap();
    wait_until(
        || {
            kernel_b
                .list_orgs()
                .map(|l| l[0].member_count == 3)
                .unwrap_or(false)
        },
        20_000,
        "B 收到成员变更推送",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// DM 组织邀请：org_send_invite / org_respond_invite / org_invite_records
// ---------------------------------------------------------------------------

const ORG_ID_FIXED: &str = "org_aaaabbbbccccdddd";

fn incoming_invite_record(id: &str, inviter_root: &str, code: &str) -> OrgInviteRecord {
    let now = system_now_ms();
    OrgInviteRecord {
        id: id.to_string(),
        org_id: ORG_ID_FIXED.to_string(),
        org_name: "星火组织".to_string(),
        org_avatar: None,
        peer_root_id: inviter_root.to_string(),
        peer_nickname: "管理员".to_string(),
        direction: OrgInviteDirection::Incoming,
        status: OrgInviteStatus::Pending,
        invite_code: Some(code.to_string()),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn org_send_invite_persists_outgoing_record_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel); // 登录即在线（p2p 自动启动）
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "星火组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let target = "cd".repeat(32);

    let record = kernel
        .org_send_invite(
            &org.record.org_id,
            &target,
            Some("peer-target-123"),
            &[],
            Some("小张"),
        )
        .unwrap();
    assert_eq!(record.direction, OrgInviteDirection::Outgoing);
    assert_eq!(record.status, OrgInviteStatus::Pending);
    assert_eq!(record.org_id, org.record.org_id);
    assert_eq!(record.peer_root_id, target);
    assert_eq!(record.peer_nickname, "小张");
    assert!(record.invite_code.is_none(), "出站记录不存邀请码");
    assert!(record.id.starts_with("inv-"), "邀请 id 为 inv- 前缀");

    // 重复邀请：原地更新（新邀请 id、回 pending），同 (orgId, peer) 只留一条
    let again = kernel
        .org_send_invite(
            &org.record.org_id,
            &target,
            Some("peer-target-123"),
            &[],
            None,
        )
        .unwrap();
    assert_ne!(again.id, record.id, "重复邀请生成新邀请 id");
    assert_eq!(again.status, OrgInviteStatus::Pending);
    assert_eq!(again.peer_nickname, "小张", "未提供昵称时保留已有展示名");
    assert_eq!(again.created_at, record.created_at, "保留首次 createdAt");
    let records = kernel.org_invite_records(&org.record.org_id).unwrap();
    assert_eq!(records.len(), 1, "同 (orgId, peer) 只留一条");
    assert_eq!(records[0].id, again.id);

    kernel.shutdown().unwrap();
}

#[test]
fn org_send_invite_resolves_peer_from_preregistered_member() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "星火组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let target = "cd".repeat(32);
    // 前端 InviteMemberDialog 的预录行为：先 addMember 携带 nodeInfo
    kernel
        .org_add_member(
            &org.record.org_id,
            &target,
            Some(&OrganizationNodeInfo {
                device_uid: None,
                peer_id: Some("peer-member-123".to_string()),
                addresses: vec![],
            }),
        )
        .unwrap();
    // 不带显式寻址：从预录成员 nodeInfo 解析
    let record = kernel
        .org_send_invite(&org.record.org_id, &target, None, &[], None)
        .unwrap();
    assert_eq!(record.peer_nickname, "待加入成员", "未提供昵称用占位名");
    kernel.shutdown().unwrap();
}

#[test]
fn org_send_invite_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    // 未解锁 → Locked
    assert_eq!(
        kernel
            .org_send_invite(ORG_ID_FIXED, &"cd".repeat(32), None, &[], None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );
    let (root_id, _) = init_identity(&mut kernel);
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "星火组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    // 邀请自己 → 拒绝
    assert_eq!(
        kernel
            .org_send_invite(&org.record.org_id, &root_id, Some("peer-x"), &[], None)
            .unwrap_err()
            .to_string(),
        "不能邀请自己"
    );
    // 无任何寻址信息 → 报错（记录不落库）
    let err = kernel
        .org_send_invite(&org.record.org_id, &"cd".repeat(32), None, &[], None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("无法确定对方节点地址"), "得到 {err}");
    assert!(
        kernel
            .org_invite_records(&org.record.org_id)
            .unwrap()
            .is_empty()
    );
    // 组织不存在
    assert!(
        kernel
            .org_send_invite(
                "org_eeeeffff00001111",
                &"cd".repeat(32),
                Some("peer-x"),
                &[],
                None
            )
            .is_err()
    );
    kernel.shutdown().unwrap();
}

#[test]
fn org_respond_invite_decline_marks_declined_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);
    let inviter_root = "cd".repeat(32);
    // 合法邀请码（inviter 寻址可供回执投递解析；对端不存在，投递尽力而为）
    let code = encode_org_invite(&OrgInvitePayload::new(
        ORG_ID_FIXED.to_string(),
        "星火组织".to_string(),
        OrgInviteInviter {
            root_id: inviter_root.clone(),
            peer_id: Some("peer-inviter-123".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    // 模拟此前入站 org-invite 已落库的记录
    let mut storage = kernel.__test_storage().unwrap();
    OrganizationService::put_invite_record(
        &mut storage,
        &incoming_invite_record("inv-1", &inviter_root, &code),
    )
    .unwrap();

    // 拒绝 → declined，幂等（已终态直接返回，不再刷新）
    let updated = kernel.org_respond_invite("inv-1", false).unwrap();
    assert_eq!(updated.status, OrgInviteStatus::Declined);
    let again = kernel.org_respond_invite("inv-1", false).unwrap();
    assert_eq!(again.status, OrgInviteStatus::Declined);
    assert_eq!(again.updated_at, updated.updated_at, "终态不再刷新");
    // 终态不可逆：再 accept 也直接返回 declined（不走加入编排）
    let third = kernel.org_respond_invite("inv-1", true).unwrap();
    assert_eq!(third.status, OrgInviteStatus::Declined);

    // 不存在的邀请 → 报错
    assert_eq!(
        kernel
            .org_respond_invite("inv-x", false)
            .unwrap_err()
            .to_string(),
        "组织邀请不存在"
    );

    let records = kernel.org_invite_records(ORG_ID_FIXED).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, OrgInviteStatus::Declined);

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// F4 第一层（batch3 §1.2）：回执重试耗尽入 Org 空间 pending 队列
// ---------------------------------------------------------------------------

/// 邀请人不可达（无端点）时被邀请人应答 → 回执投递失败 + 退避耗尽 →
/// `org:dm:pending:{orgId}:` 入队（messageId = org-invite-reply-{inviteId}，
/// 同邀请幂等覆盖）；本侧状态已落库（declined），不回滚。
#[test]
fn invite_reply_enqueues_org_pending_when_inviter_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel); // 登录即在线（p2p 自动启动）

    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "回执组织".to_string(),
            ..Default::default()
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    let inviter_root = "cc".repeat(32);
    // 入站邀请记录（邀请人端点不可达：peerId 虚构、无地址）
    let payload = OrgInvitePayload::new(
        &org_id,
        "回执组织",
        OrgInviteInviter {
            root_id: inviter_root.clone(),
            peer_id: Some("peer-unreachable".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    );
    let code = encode_org_invite(&payload);
    let rec = OrgInviteRecord {
        id: "inv-unreach".to_string(),
        org_id: org_id.clone(),
        org_name: "回执组织".to_string(),
        org_avatar: None,
        peer_root_id: inviter_root.clone(),
        peer_nickname: "邀请人".to_string(),
        direction: OrgInviteDirection::Incoming,
        status: OrgInviteStatus::Pending,
        invite_code: Some(code),
        created_at: system_now_ms(),
        updated_at: system_now_ms(),
    };
    let mut s = kernel.__test_storage().unwrap();
    OrganizationService::put_invite_record(&mut s, &rec).unwrap();

    // 应答拒绝（不走 accept 编排）→ 本地标 declined + 回执投递（不可达）
    let updated = kernel.org_respond_invite("inv-unreach", false).unwrap();
    assert_eq!(
        updated.status,
        OrgInviteStatus::Declined,
        "本侧状态先行落库"
    );

    // 投递失败 + 2s/5s 退避耗尽 → 入队。轮询（不固定 sleep 卡死）
    let prefix = format!("org:dm:pending:{org_id}:");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        let scan = spark_core::storage::StorageBackend::scan(
            &kernel.__test_storage().unwrap(),
            &spark_core::storage::ScanOptions::prefix(&prefix),
        )
        .unwrap();
        if scan
            .iter()
            .any(|(_, raw)| raw.contains("org-invite-reply") && raw.contains("inv-unreach"))
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(found, "重试耗尽后回执入 Org 空间 pending 队列");
    // 本侧状态不回滚
    let rec_after = OrganizationService::get_incoming_invite(
        &kernel.__test_storage().unwrap(),
        &org_id,
        &inviter_root,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        rec_after.status,
        OrgInviteStatus::Declined,
        "终态拒绝/不可达不回滚本侧状态"
    );
    kernel.shutdown().unwrap();
}
