//! org::roles 单测：履职集计分推导（在线 > 最近活跃 > rotate 轮换 > 字典序
//! tie-break，叶子不履职）、数据账号推导/显式覆盖、设备类判定、
//! gateways 存量字段忽略读取（A9 / network §4.2 / §六验收）。

use super::*;
use crate::org::types::{
    OrganizationDeviceSet, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
    OrganizationRole,
};

fn cand(root_id: &str, online: bool, last_active_ms: i64, leaf: bool) -> GatewayCandidateScore {
    GatewayCandidateScore {
        root_id: root_id.to_string(),
        online,
        last_active_ms,
        leaf,
    }
}

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
        access_key: None,
        kind: None,
        org_binding: None,
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
        domain_type: None,
        extra: Default::default(),
    }
}

const ADMIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const M1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const M2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const M3: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const M4: &str = "4444444444444444444444444444444444444444444444444444444444444444";

/// §六验收向量：固定名册 + 固定在线视图 → 固定履职集（确定性限流 3 个）；
/// 同分（rotate 注入恒等）tie-break 按 rootId 字典序。
#[test]
fn gateway_active_deterministic_and_tie_break_lexicographic() {
    let scores = vec![
        cand(M4, true, 100, false),
        cand(M1, true, 100, false),
        cand(M3, true, 100, false),
        cand(M2, true, 100, false),
        cand(ADMIN, true, 100, false),
    ];
    // rotate 恒等 → 全因子同分：纯字典序取前 3（确定性 + tie-break 向量）
    let fixed = order_gateway_candidates(&scores, &|_| 0);
    assert_eq!(
        fixed,
        vec![M1.to_string(), M2.to_string(), M3.to_string()],
        "同分 tie-break 按 identity 字典序"
    );
    // 真实 rotate 下同一视图重复计算结果一致（确定性）
    let a = select_gateway_active(&scores, "org_test", 1_700_000_000_000);
    let b = select_gateway_active(&scores, "org_test", 1_700_000_000_000);
    assert_eq!(a, b, "固定名册 + 固定在线视图 → 固定履职集");
    assert_eq!(a.len(), GATEWAY_ACTIVE_LIMIT);
    // 换 bucket（小时后）履职集可能轮换，但仍恰好 3 个
    let next = select_gateway_active(&scores, "org_test", 1_700_000_000_000 + GATEWAY_ACTIVE_ROTATE_MS);
    assert_eq!(next.len(), GATEWAY_ACTIVE_LIMIT);
}

/// 计分因子：在线 > 离线（离线者最近活跃更高也排后）；同在线按最近活跃降序；
/// 叶子（纯移动设备成员）即便在线也不履职。
#[test]
fn gateway_active_scoring_factors() {
    let scores = vec![
        cand(M1, false, 9999, false), // 离线但最近活跃最高
        cand(M2, true, 100, false),   // 在线
        cand(M3, true, 200, false),   // 在线且更活跃
        cand(M4, true, 500, true),    // 叶子：在线活跃最高也不履职
    ];
    let out = order_gateway_candidates(&scores, &|_| 0);
    assert_eq!(
        out,
        vec![M3.to_string(), M2.to_string(), M1.to_string()],
        "在线优先 → 最近活跃降序 → 离线殿后；叶子过滤"
    );
    // 全员叶子：履职集为空（组织无 PC 节点 = Q04 部署前置不满足，如实退化）
    let all_leaf = vec![cand(M1, true, 100, true), cand(M2, true, 100, true)];
    assert!(order_gateway_candidates(&all_leaf, &|_| 0).is_empty());
}

/// 履职集随在线变化自动更替（§六单测）：同一名册，在线视图翻转 → 集合更替。
#[test]
fn gateway_active_set_follows_online_changes() {
    let base = vec![
        cand(M1, true, 100, false),
        cand(M2, false, 100, false),
        cand(M3, false, 100, false),
        cand(M4, false, 100, false),
    ];
    let before = select_gateway_active(&base, "org_test", 1_700_000_000_000);
    assert!(before.contains(&M1.to_string()));
    // M1 下线、其余上线 → M1 掉出履职集
    let flipped = vec![
        cand(M1, false, 100, false),
        cand(M2, true, 100, false),
        cand(M3, true, 100, false),
        cand(M4, true, 100, false),
    ];
    let after = select_gateway_active(&flipped, "org_test", 1_700_000_000_000);
    assert!(!after.contains(&M1.to_string()), "离线即掉出履职集");
    assert_eq!(after.len(), GATEWAY_ACTIVE_LIMIT);
}

/// 存储装配（gateway_candidate_scores）：peer_activity 有进行中会话 = 在线、
/// last_seen 最大 = 最近活跃；self 恒在线（peer_activity 不记本机）；
/// 移动设备成员 = 叶子。is_gateway_active 与活跃集一致；非成员永不活跃。
#[test]
fn gateway_candidate_scores_from_storage() {
    use crate::device::{DeviceRecord, DeviceService};
    use crate::p2p::peer_activity::PeerActivityStore;
    use crate::storage::MemoryStorage;

    let mut storage = MemoryStorage::new();
    // M1：PC 端点，peer_activity 有进行中会话 → 在线
    let mut m1 = member(M1, OrganizationRole::Member);
    m1.node_info = Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("peer-pc-1".to_string()),
        addresses: vec![],
    }));
    DeviceService::upsert_pdsync(
        &mut storage,
        &DeviceRecord {
            peer_id: "peer-pc-1".to_string(),
            device_uid: Some("uid-pc".to_string()),
            device_name: "PC".to_string(),
            os: "Windows".to_string(),
            arch: "x86_64".to_string(),
            macs: vec![],
            app_version: "0.2.1".to_string(),
            os_version: "10".to_string(),
            updated_at: 1000,
            last_seen_at: 1000,
            revoked_at: None,
            device_pub_key: None,
        },
        1000,
        "node-a",
    )
    .unwrap();
    {
        let mut store = PeerActivityStore::new(&mut storage);
        store.mark_connected("peer-pc-1", 5000).unwrap();
    }
    // M2：移动端点 → 叶子
    let mut m2 = member(M2, OrganizationRole::Member);
    m2.node_info = Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("peer-mobile-1".to_string()),
        addresses: vec![],
    }));
    DeviceService::upsert_pdsync(
        &mut storage,
        &DeviceRecord {
            peer_id: "peer-mobile-1".to_string(),
            device_uid: Some("uid-m".to_string()),
            device_name: "手机".to_string(),
            os: "Android".to_string(),
            arch: "aarch64".to_string(),
            macs: vec![],
            app_version: "0.2.1".to_string(),
            os_version: "14".to_string(),
            updated_at: 1000,
            last_seen_at: 1000,
            revoked_at: None,
            device_pub_key: None,
        },
        1000,
        "node-a",
    )
    .unwrap();
    let record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        m1,
        m2,
        member(M3, OrganizationRole::Member),
    ]);
    let scores = gateway_candidate_scores(&storage, &record, Some(ADMIN), 10_000);
    let by_id = |rid: &str| scores.iter().find(|s| s.root_id == rid).unwrap();
    assert!(by_id(ADMIN).online, "self 恒在线");
    assert!(by_id(M1).online, "进行中会话 = 在线");
    assert_eq!(by_id(M1).last_active_ms, 5000);
    assert!(!by_id(M3).online && by_id(M3).last_active_ms == 0, "无记录 = 离线/0");
    assert!(by_id(M2).leaf, "移动端点成员 = 叶子");
    assert!(!by_id(M1).leaf);

    // is_gateway_active 与活跃集一致；非成员永不活跃
    let active = gateway_active_set(&storage, &record, Some(ADMIN), 10_000);
    for rid in [ADMIN, M1, M2, M3] {
        assert_eq!(
            is_gateway_active(&storage, &record, rid, 10_000),
            active.iter().any(|a| a == rid)
        );
    }
    assert!(!active.iter().any(|a| a == M2), "叶子不入履职集");
    assert!(!is_gateway_active(&storage, &record, &"f".repeat(64), 10_000));
}

/// A9：gateways 存量字段忽略读取——含 `gateways` 键的旧记录 JSON 正常解析
/// （字段惰性、任何消费者不读）；重新序列化即丢键（随记录保存自然老化，
/// 且不经 extra flatten 成僵尸键随快照流动）。
#[test]
fn legacy_gateways_field_ignored_on_read_and_dropped_on_save() {
    let json = r#"{
        "orgId": "org_test", "name": "测试组织", "createdAt": 1000,
        "createdBy": "creator", "updatedAt": 1000,
        "members": [],
        "gateways": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
    }"#;
    let record: OrganizationRecord = serde_json::from_str(json).expect("旧记录可解析");
    // 忽略读取：活跃集推导不受存量 gateways 影响（空名册 → 空集）
    let storage = crate::storage::MemoryStorage::new();
    assert!(gateway_active_set(&storage, &record, None, 0).is_empty());
    // 保存即老化：序列化不含 gateways 键，也不落入 extra
    let out = serde_json::to_string(&record).unwrap();
    assert!(!out.contains("gateways"), "序列化丢键，实际: {out}");
    assert!(!record.extra.contains_key("gateways"), "不经 extra 僵尸流动");
}

/// A14 全员数据节点（membership §4.1）：data_node_set = 全体成员（与角色
/// 无关——数据面与治理面解耦）；is_data_node = 成员判定。
#[test]
fn data_node_set_is_all_members() {
    let record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
        member(M2, OrganizationRole::Admin),
    ]);
    let mut set = data_node_set(&record);
    set.sort();
    let mut expect = vec![ADMIN.to_string(), M1.to_string(), M2.to_string()];
    expect.sort();
    assert_eq!(set, expect, "副本池 = 全体成员账号（普通成员也在内）");
    for rid in [ADMIN, M1, M2] {
        assert!(is_data_node(&record, rid), "成员即数据节点");
    }
    assert!(!is_data_node(&record, &"f".repeat(64)), "非成员不是数据节点");
}

/// A14：dataAccounts 存量字段忽略读取——存量记录里残留的指定列表不影响
/// 全员推导（读取即忽略、保存即老化同 A9 手法）。
#[test]
fn legacy_data_accounts_field_ignored() {
    let mut record = org(vec![
        member(ADMIN, OrganizationRole::Admin),
        member(M1, OrganizationRole::Member),
    ]);
    // 存量显式指定（含非成员）：不再影响推导
    record.data_accounts = vec![M1.to_string(), "9".repeat(64)];
    let mut set = data_node_set(&record);
    set.sort();
    let mut expect = vec![ADMIN.to_string(), M1.to_string()];
    expect.sort();
    assert_eq!(set, expect, "存量 dataAccounts 不影响全员推导");
    // 保存即老化：序列化不含 dataAccounts 键
    let out = serde_json::to_string(&record).unwrap();
    assert!(!out.contains("dataAccounts"), "序列化丢键，实际: {out}");
    // 存量 JSON 解析兼容（键可入、字段惰性、不落 extra）
    let legacy = r#"{"orgId":"org_test","name":"t","createdAt":1,"createdBy":"c","updatedAt":1,"members":[],"dataAccounts":["aa"]}"#;
    let parsed: OrganizationRecord = serde_json::from_str(legacy).unwrap();
    assert_eq!(parsed.data_accounts, vec!["aa".to_string()], "解析兼容存量");
    assert!(!parsed.extra.contains_key("dataAccounts"), "不落 extra");
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
        revoked_at: None,
        device_pub_key: None,
    };
    DeviceService::upsert_pdsync(&mut storage, &device, 1000, "node-a").unwrap();
    let mut m = member(M1, OrganizationRole::Member);
    m.node_info = Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("peer-mobile".to_string()),
        addresses: vec![],
    }));
    assert_eq!(member_device_class(&storage, &m), "mobile");
    // 无 nodeInfo / 无设备记录 → 兜底 pc（宁可多算不漏算）
    let m2 = member(M2, OrganizationRole::Member);
    assert_eq!(member_device_class(&storage, &m2), "pc");
}

/// F1：端点集存在但全部端点仅地址（无 peerId，无法查设备记录）→ 兜底 pc，
/// 与注释「无记录按 pc 计入」一致（不得落穿返回 mobile）。
#[test]
fn member_device_class_falls_back_pc_when_endpoints_have_no_peer_id() {
    use crate::org::types::{OrganizationDeviceSet, OrganizationNodeInfo};
    use crate::storage::MemoryStorage;
    let storage = MemoryStorage::new();
    // 端点集仅地址（peer_id 为 None）→ 无法查表 → pc
    let mut m = member(M1, OrganizationRole::Member);
    m.node_info = Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
        device_uid: None,
        peer_id: None,
        addresses: vec!["/ip4/127.0.0.1/tcp/9001".to_string()],
    }));
    assert_eq!(member_device_class(&storage, &m), "pc", "仅地址端点兜底 pc");
    // 混合：一个仅地址端点 + 一个 mobile peerId 端点 → 进入查表 → mobile
    m.node_info = Some(OrganizationDeviceSet {
        endpoints: vec![
            OrganizationNodeInfo {
                device_uid: None,
                peer_id: None,
                addresses: vec!["/ip4/127.0.0.1/tcp/9001".to_string()],
            },
            OrganizationNodeInfo {
                device_uid: None,
                peer_id: Some("peer-mobile".to_string()),
                addresses: vec![],
            },
        ],
    });
    let mut s2 = MemoryStorage::new();
    let mobile = crate::device::DeviceRecord {
        peer_id: "peer-mobile".to_string(),
        device_uid: Some("uid-m".to_string()),
        device_name: "手机".to_string(),
        os: "iOS".to_string(),
        arch: "arm64".to_string(),
        macs: vec![],
        app_version: "0.2.1".to_string(),
        os_version: "17".to_string(),
        updated_at: 1000,
        last_seen_at: 1000,
        revoked_at: None,
        device_pub_key: None,
    };
    crate::device::DeviceService::upsert_pdsync(&mut s2, &mobile, 1000, "node-a").unwrap();
    assert_eq!(
        member_device_class(&s2, &m),
        "mobile",
        "任一可查端点 mobile → mobile"
    );
}

/// 成员换设备（同 deviceUid 新 peerId）记账不漂：peerId 漂移后设备类判定不变，
/// K 副本 PC 计入稳定（角色绑账号、设备类绑物理设备 UID，均与 peerId 无关）。
#[test]
fn device_class_stable_across_peer_id_drift_same_device_uid() {
    use crate::device::{DeviceRecord, DeviceService};
    use crate::storage::MemoryStorage;

    let mut storage = MemoryStorage::new();
    let device = DeviceRecord {
        peer_id: "peer-old".to_string(),
        device_uid: Some("uid-pc".to_string()),
        device_name: "PC".to_string(),
        os: "Windows".to_string(),
        arch: "x86_64".to_string(),
        macs: vec![],
        app_version: "0.2.1".to_string(),
        os_version: "10.0.22631".to_string(),
        updated_at: 1000,
        last_seen_at: 1000,
        revoked_at: None,
        device_pub_key: None,
    };
    DeviceService::upsert_pdsync(&mut storage, &device, 1000, "node-a").unwrap();

    // 旧设备（uid-pc, peer-old）：pc。
    let mut m_old = member(M1, OrganizationRole::Member);
    m_old.node_info = Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
        device_uid: Some("uid-pc".to_string()),
        peer_id: Some("peer-old".to_string()),
        addresses: vec![],
    }));
    assert_eq!(member_device_class(&storage, &m_old), "pc");

    // 同 deviceUid 新 peerId（重装/漂移，DeviceRecord 也随 pdsync 换新）：
    // 端点集墓碑化替换旧 peerId → 记账仍 pc。
    let mut m_new = m_old.clone();
    let set = m_new.node_info.as_mut().unwrap();
    set.upsert(&OrganizationNodeInfo {
        device_uid: Some("uid-pc".to_string()),
        peer_id: Some("peer-new".to_string()),
        addresses: vec![],
    });
    assert_eq!(set.len(), 1, "同 deviceUid 旧 peerId 已墓碑化替换");
    let drifted = DeviceRecord {
        peer_id: "peer-new".to_string(),
        updated_at: 2000,
        ..device
    };
    DeviceService::upsert_pdsync(&mut storage, &drifted, 2000, "node-a").unwrap();
    assert_eq!(
        member_device_class(&storage, &m_new),
        "pc",
        "换设备后 PC 计入不漂"
    );

    // 角色绑账号（rootId），与设备/peerId 无关——换设备角色不漂。
    m_new.role = OrganizationRole::Admin;
    assert_eq!(m_new.role, OrganizationRole::Admin);
    // A14：成员即数据节点（与角色无关）
    assert!(crate::org::roles::is_data_node(
        &OrganizationRecord {
            members: vec![m_new.clone()],
            ..Default::default()
        },
        M1,
    ));
}
