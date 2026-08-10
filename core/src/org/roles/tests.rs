//! org::roles 单测：缺省推导（网关=全员候选/数据=全体管理员）、显式覆盖、
//! 活跃集确定性轮换、设备类判定。

use super::*;
use crate::org::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
};

fn member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role,
        joined_at: 1000,
        added_by: "creator".to_string(),
        node_info: None,
        nickname: None,
        avatar: None,
        signature: None,
        gender: None,
        region: None,
        use_personal_identity: None,
        extra: Default::default(),
    }
}

fn org(members: Vec<OrganizationMember>) -> OrganizationRecord {
    OrganizationRecord {
        org_id: "org_test".to_string(),
        name: "测试组织".to_string(),
        description: String::new(),
        avatar: String::new(),
        base_plugin_domain: None,
        created_at: 1000,
        created_by: "creator".to_string(),
        updated_at: 1000,
        members,
        sync: None,
        gateways: vec![],
        data_accounts: vec![],
        org_address: None,
        is_public: false,
        extra: Default::default(),
    }
}

const ADMIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const M1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const M2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const M3: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const M4: &str = "4444444444444444444444444444444444444444444444444444444444444444";

#[test]
fn gateway_default_all_members_active_limited() {
    // 未指定网关：全员候选，活跃集确定性限流 3 个
    let record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
        member(M2, OrganizationRole::Member),
        member(M3, OrganizationRole::Member),
        member(M4, OrganizationRole::Member),
    ]);
    let active = gateway_active_set(&record, 1_700_000_000_000);
    assert_eq!(active.len(), GATEWAY_ACTIVE_LIMIT);
    for rid in &active {
        assert!(record.find_member(rid).is_some(), "活跃集必须都是成员");
    }
    // 同一时刻重复计算结果一致（确定性）
    assert_eq!(active, gateway_active_set(&record, 1_700_000_000_000));
    // 活跃成员 is_gateway_active 为真，非活跃为假
    for rid in [ADMIN, M1, M2, M3, M4] {
        assert_eq!(
            is_gateway_active(&record, rid, 1_700_000_000_000),
            active.iter().any(|a| a == rid)
        );
    }
    // 换 bucket（小时后）活跃集可能轮换，但仍恰好 3 个
    let next = gateway_active_set(&record, 1_700_000_000_000 + GATEWAY_ACTIVE_ROTATE_MS);
    assert_eq!(next.len(), GATEWAY_ACTIVE_LIMIT);
    // 非成员永不活跃
    assert!(!is_gateway_active(&record, &"f".repeat(64), 1_700_000_000_000));
}

#[test]
fn gateway_explicit_override() {
    let mut record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
        member(M2, OrganizationRole::Member),
    ]);
    record.gateways = vec![M1.to_string(), M2.to_string()];
    assert!(is_gateway_active(&record, M1, 0));
    assert!(is_gateway_active(&record, M2, 0));
    assert!(!is_gateway_active(&record, ADMIN, 0), "显式指定后缺省推导不生效");
    // 指定了已退出成员：过滤掉
    record.gateways = vec![M1.to_string(), "9".repeat(64)];
    assert_eq!(gateway_active_set(&record, 0), vec![M1.to_string()]);
}

#[test]
fn data_account_default_all_admins() {
    let record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
        member(M2, OrganizationRole::Admin),
    ]);
    let set = data_account_set(&record);
    assert_eq!(set, vec![ADMIN.to_string(), M2.to_string()]);
    assert!(is_data_account(&record, ADMIN));
    assert!(is_data_account(&record, M2));
    assert!(!is_data_account(&record, M1));
    assert!(!has_explicit_data_accounts(&record));
}

#[test]
fn data_account_explicit_override() {
    let mut record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
        member(M2, OrganizationRole::Admin),
    ]);
    // 显式收窄：只留一个数据账号（可以是普通成员——指定权在管理员）
    record.data_accounts = vec![M1.to_string()];
    assert_eq!(data_account_set(&record), vec![M1.to_string()]);
    assert!(is_data_account(&record, M1));
    assert!(!is_data_account(&record, ADMIN), "显式指定后管理员不自动担责");
    assert!(has_explicit_data_accounts(&record));
    // 指定非成员：过滤
    record.data_accounts = vec!["9".repeat(64)];
    assert!(data_account_set(&record).is_empty());
}

#[test]
fn member_device_class_lookup() {
    use crate::device::{DeviceRecord, DeviceService};
    use crate::storage::MemoryStorage;

    let mut storage = MemoryStorage::new();
    let device = DeviceRecord {
        peer_id: "peer-mobile".to_string(),
        device_uid: Some("uid-1".to_string()),
        device_name: "手机".to_string(),
        os: "Android".to_string(),
        arch: "aarch64".to_string(),
        macs: vec![],
        app_version: "0.2.1".to_string(),
        os_version: "14".to_string(),
        updated_at: 1000,
        last_seen_at: 1000,
    };
    DeviceService::upsert_pdsync(&mut storage, &device, 1000, "node-a").unwrap();
    let mut m = member(M1, OrganizationRole::Member);
    m.node_info = Some(OrganizationNodeInfo {
        peer_id: Some("peer-mobile".to_string()),
        addresses: vec![],
    });
    assert_eq!(member_device_class(&storage, &m), "mobile");
    // 无 nodeInfo / 无设备记录 → 兜底 pc（宁可多算不漏算）
    let m2 = member(M2, OrganizationRole::Member);
    assert_eq!(member_device_class(&storage, &m2), "pc");
}
