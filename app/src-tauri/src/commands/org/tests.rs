//! 组织命令单测：直调 *_inner，不依赖 WebView。

use super::*;
use spark_core::kernel::KernelConfig;

const PASSWORD: &str = "correct-horse-battery";

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

fn input() -> CreateOrgInputDto {
    serde_json::from_value(serde_json::json!({
        "name": "测试组织",
        "description": "demo",
        "basePluginDomain": "plugin:base"
    }))
    .unwrap()
}

#[test]
fn create_and_list_roundtrip() {
    let (_dir, mut kernel) = unlocked_kernel();
    assert!(list_mine_inner(&kernel).unwrap().is_empty());

    let view = create_inner(&mut kernel, input()).unwrap();
    assert_eq!(view.record.name, "测试组织");
    assert!(view.is_current_user_admin);
    assert_eq!(view.member_count, 1);

    let mine = list_mine_inner(&kernel).unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].record.org_id, view.record.org_id);

    // 副本概览：本机恒已同步
    let overview = sync_overview_inner(&kernel, &view.record.org_id).unwrap();
    assert_eq!(overview.total_members, 1);
    assert!(overview.synced_peers >= 1);
    assert!(overview.members[0].is_self);
}

#[test]
fn invite_requires_p2p_and_bad_code_errors() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = create_inner(&mut kernel, input()).unwrap();

    // P2P 未启动 → 生成邀请码报专用文案（内核语义：邀请码须携带本机节点信息）
    assert_eq!(
        create_invite_inner(&kernel, &view.record.org_id).unwrap_err(),
        "本机 P2P 节点尚未启动，请先启动网络后再生成邀请码"
    );

    // 坏邀请码 → 解析失败
    assert!(join_by_invite_inner(&kernel, "not-a-code").is_err());

    // 未知组织 → check_join 失败（本地无成员记录）
    assert!(check_join_inner(&kernel, "org_0000000000000000").is_err());
}

#[test]
fn overview_unknown_org_errors() {
    let (_dir, kernel) = unlocked_kernel();
    assert!(sync_overview_inner(&kernel, "org_0000000000000000").is_err());
}

#[test]
fn member_management_and_delete() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = create_inner(&mut kernel, input()).unwrap();
    let org_id = view.record.org_id.clone();
    let member_root = "ab".repeat(32);

    // 添加成员（无 nodeInfo）
    let input: AddOrgMemberInputDto =
        serde_json::from_value(serde_json::json!({ "rootId": member_root })).unwrap();
    let view = add_member_inner(&mut kernel, &org_id, input).unwrap();
    assert_eq!(view.member_count, 2);
    assert!(!view.members.iter().all(|m| m.root_id != member_root));

    // 非法 rootId / 未知组织
    let bad: AddOrgMemberInputDto =
        serde_json::from_value(serde_json::json!({ "rootId": "zz" })).unwrap();
    assert_eq!(
        add_member_inner(&mut kernel, &org_id, bad).unwrap_err(),
        "Invalid member rootId"
    );
    let input: AddOrgMemberInputDto =
        serde_json::from_value(serde_json::json!({ "rootId": member_root })).unwrap();
    assert_eq!(
        add_member_inner(&mut kernel, "org_nope", input).unwrap_err(),
        "Organization not found"
    );

    // 移除成员；移除唯一 admin 被拒
    let view = remove_member_inner(&mut kernel, &org_id, &member_root).unwrap();
    assert_eq!(view.member_count, 1);
    let self_root = kernel.current_root_id().unwrap().unwrap();
    assert_eq!(
        remove_member_inner(&mut kernel, &org_id, &self_root).unwrap_err(),
        "Organization must keep at least one admin"
    );

    // 删除组织
    delete_inner(&mut kernel, &org_id).unwrap();
    assert!(list_mine_inner(&kernel).unwrap().is_empty());
    assert_eq!(
        delete_inner(&mut kernel, &org_id).unwrap_err(),
        "Organization not found"
    );
}

#[test]
fn set_gateways_flow() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = create_inner(&mut kernel, input()).unwrap();
    let org_id = view.record.org_id.clone();
    let self_root = kernel.current_root_id().unwrap().unwrap();
    let member_root = "ab".repeat(32);
    let input: AddOrgMemberInputDto =
        serde_json::from_value(serde_json::json!({ "rootId": member_root })).unwrap();
    add_member_inner(&mut kernel, &org_id, input).unwrap();

    // 组织创建时已生成 orgSecret（随视图 extra 下发，UI 不渲染）
    assert_eq!(
        view.record
            .extra
            .get("orgSecret")
            .and_then(|v| v.as_str())
            .map(str::len),
        Some(64)
    );

    // 正常设置 2 个网关（含自己），视图携带 gateways
    let view = set_gateways_inner(
        &mut kernel,
        &org_id,
        vec![self_root.clone(), member_root.clone()],
    )
    .unwrap();
    assert_eq!(view.record.gateways, vec![self_root.clone(), member_root.clone()]);

    // 数量不足 / 非成员 / 非 admin 组织不存在等错误透传
    assert_eq!(
        set_gateways_inner(&mut kernel, &org_id, vec![self_root.clone()]).unwrap_err(),
        "Gateways must be 2 to 3 member rootIds of the organization"
    );
    assert_eq!(
        set_gateways_inner(&mut kernel, &org_id, vec![self_root.clone(), "cd".repeat(32)])
            .unwrap_err(),
        "Gateways must be 2 to 3 member rootIds of the organization"
    );
    assert_eq!(
        set_gateways_inner(&mut kernel, "org_nope", vec![self_root.clone(), member_root.clone()])
            .unwrap_err(),
        "Organization not found"
    );
}

#[test]
fn set_public_and_search_known_flow() {
    let (_dir, mut kernel) = unlocked_kernel();
    let view = create_inner(&mut kernel, input()).unwrap();
    let org_id = view.record.org_id.clone();

    // 创建时生成 orgAddress（保留键随视图下发），默认不公开；
    // 根私钥密文随视图 extra 下发（密文形态，UI 不渲染）
    let org_address = view
        .record
        .org_address
        .clone()
        .expect("orgAddress generated at create");
    assert_eq!(org_address.len(), 55);
    assert!(!view.record.is_public);
    assert!(view.record.extra.get("orgRootSecret").is_some());

    // 开启公开 + 展示名
    let view = set_public_inner(&mut kernel, &org_id, true, Some("星火 公开组织".to_string()))
        .unwrap();
    assert!(view.record.is_public);
    assert_eq!(
        view.record
            .extra
            .get("orgDisplayName")
            .and_then(|v| v.as_str()),
        Some("星火 公开组织")
    );
    // 非成员/未知组织错误透传
    assert_eq!(
        set_public_inner(&mut kernel, "org_nope", true, None).unwrap_err(),
        "Organization not found"
    );

    // 解析：p2p 未启动且缓存为空 → None；非法地址 → 报错
    assert!(resolve_address_inner(&kernel, &org_address).unwrap().is_none());
    assert_eq!(
        resolve_address_inner(&kernel, "not-an-address").unwrap_err(),
        "Invalid org address"
    );

    // 本地搜索：写入一条缓存记录（模拟 gossip/DHT 命中沉淀）后命中
    let signing = spark_core::org::org_root_signing_key(&kernel_record(&kernel, &org_id))
        .expect("root key opens");
    let now = spark_core::p2p::node::system_now_ms();
    let record = spark_core::org::sign_org_address_record(
        &signing,
        &org_id,
        Some("星火 公开组织".to_string()),
        vec![],
        1,
        now,
        spark_core::org::ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    kernel_cache_record(&mut kernel, &record);

    // 缓存命中后 resolve 直接返回（无需 p2p）
    let resolved = resolve_address_inner(&kernel, &org_address).unwrap().unwrap();
    assert_eq!(resolved.org_id, org_id);
    assert_eq!(resolved.display_name.as_deref(), Some("星火 公开组织"));
    assert_eq!(resolved.seq, 1);

    // 搜索：displayName 子串 / orgAddress 子串 / 未命中
    let hits = search_known_inner(&kernel, "公开").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].org_address, org_address);
    let hits = search_known_inner(&kernel, &org_address[..16]).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(search_known_inner(&kernel, "不存在的关键字").unwrap().is_empty());
}

/// 读取内核中的组织记录（测试辅助）。
fn kernel_record(kernel: &Kernel, org_id: &str) -> spark_core::org::OrganizationRecord {
    let storage = kernel.__test_storage().unwrap();
    spark_core::org::OrganizationService::get_record(&storage, org_id)
        .unwrap()
        .unwrap()
}

/// 直接向内核缓存写入组织地址记录（测试辅助）。
fn kernel_cache_record(kernel: &mut Kernel, record: &spark_core::org::OrgAddressRecord) {
    let mut storage = kernel.__test_storage().unwrap();
    spark_core::org::cache_org_address_record(&mut storage, record).unwrap();
}

#[test]
fn accept_invite_error_paths() {        let (_dir, mut kernel) = unlocked_kernel();
    // 坏邀请码
    assert!(accept_invite_inner(&mut kernel, "not-a-code").is_err());
    // 合法邀请码但 p2p 未启动
    let code = spark_core::org::encode_org_invite(&spark_core::org::OrgInvitePayload::new(
        "org_abc".to_string(),
        "组织".to_string(),
        spark_core::org::OrgInviteInviter {
            root_id: "cd".repeat(32),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        spark_core::p2p::node::system_now_ms(),
    ));
    assert_eq!(
        accept_invite_inner(&mut kernel, &code).unwrap_err(),
        "P2P 网络未启动，无法通过邀请码加入"
    );
}
