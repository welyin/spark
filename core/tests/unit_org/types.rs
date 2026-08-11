//! 组织记录类型与归一化规则单测。

use spark_core::org::OrgError;
use spark_core::org::types::*;

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

#[test]
fn root_id_validation() {
    assert!(is_valid_root_id(&rid('a')));
    assert!(is_valid_root_id(&format!("  {} ", rid('F')))); // trim + lowercase
    assert!(!is_valid_root_id(&rid('g')));
    assert!(!is_valid_root_id(&rid('a')[..63]));
    assert!(!is_valid_root_id(""));
    assert_eq!(
        normalize_root_id(&format!(" {} ", rid('B'))).unwrap(),
        rid('b')
    );
    assert!(normalize_root_id("xyz").is_err());
}

#[test]
fn text_normalization() {
    assert_eq!(
        normalize_text("  hello   world \n ", "Name").unwrap(),
        "hello world"
    );
    assert!(normalize_text("   ", "Name").is_err());
}

#[test]
fn node_info_normalization() {
    // 全空 → required 错误
    let empty = OrganizationNodeInfo::default();
    assert!(matches!(
        normalize_node_info(&empty),
        Err(OrgError::NodeInfoRequired)
    ));
    // peerId 过短
    let short = OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("abc".to_string()),
        addresses: vec![],
    };
    assert!(matches!(
        normalize_node_info(&short),
        Err(OrgError::InvalidPeerId)
    ));
    // trim + 滤空
    let ok = OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("  peer-12345  ".to_string()),
        addresses: vec![" /ip4/1.2.3.4/tcp/1 ".to_string(), "  ".to_string()],
    };
    let n = normalize_node_info(&ok).unwrap();
    assert_eq!(n.peer_id.as_deref(), Some("peer-12345"));
    assert_eq!(n.addresses, vec!["/ip4/1.2.3.4/tcp/1"]);
}

#[test]
fn optional_node_info() {
    assert_eq!(normalize_optional_node_info(None).unwrap(), None);
    let all_blank = OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("   ".to_string()),
        addresses: vec![" ".to_string()],
    };
    assert_eq!(
        normalize_optional_node_info(Some(&all_blank)).unwrap(),
        None
    );
    let valid = OrganizationNodeInfo {
        device_uid: None,
        peer_id: Some("peer-12345".to_string()),
        addresses: vec![],
    };
    assert!(
        normalize_optional_node_info(Some(&valid))
            .unwrap()
            .is_some()
    );
}

#[test]
fn sort_members_admin_first_then_joined_at() {
    let m = |root: char, role: OrganizationRole, joined: i64| OrganizationMember {
        root_id: rid(root),
        role,
        joined_at: joined,
        added_by: rid('f'),
        node_info: None,
        ..Default::default()
    };
    let members = vec![
        m('a', OrganizationRole::Member, 300),
        m('b', OrganizationRole::Member, 100),
        m('c', OrganizationRole::Admin, 500),
        m('d', OrganizationRole::Admin, 200),
    ];
    let sorted = sort_members(&members);
    let order: Vec<char> = sorted
        .iter()
        .map(|m| m.root_id.chars().next().unwrap())
        .collect();
    assert_eq!(order, vec!['d', 'c', 'b', 'a']);
}

#[test]
fn org_id_and_secret_shapes() {
    let id = generate_organization_id();
    assert!(id.starts_with("org_") && id.len() == 4 + 16);
    assert!(
        id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let secret = generate_recovery_secret();
    assert_eq!(secret.len(), 64);
    assert!(
        secret
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}

#[test]
fn recovery_secret_via_dynamic_extra() {
    let mut record = OrganizationRecord::default();
    assert_eq!(record.recovery_secret(), None);
    record.set_recovery_secret("ab".repeat(32));
    assert_eq!(record.recovery_secret(), Some("ab".repeat(32).as_str()));
    // 动态键序列化为顶层键（与 TS 记录形状一致）
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["recoverySecret"], serde_json::json!("ab".repeat(32)));
    // 反序列化后仍在 extra 中（不会丢）
    let back: OrganizationRecord = serde_json::from_value(json).unwrap();
    assert_eq!(back.recovery_secret(), Some("ab".repeat(32).as_str()));
}

#[test]
fn org_secret_via_dynamic_extra() {
    let mut record = OrganizationRecord::default();
    assert_eq!(record.org_secret(), None);
    let secret = generate_org_secret();
    assert_eq!(secret.len(), 64);
    assert!(
        secret
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    record.set_org_secret(&secret);
    assert_eq!(record.org_secret(), Some(secret.as_str()));
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["orgSecret"], serde_json::json!(secret));
    let back: OrganizationRecord = serde_json::from_value(json).unwrap();
    assert_eq!(back.org_secret(), Some(secret.as_str()));
}

#[test]
fn gateways_serde_roundtrip() {
    let mut record = OrganizationRecord::default();
    // 缺省：序列化丢键、反序列化为空
    let json = serde_json::to_value(&record).unwrap();
    assert!(json.get("gateways").is_none());
    record.gateways = vec![rid('a'), rid('b')];
    assert!(record.is_gateway(&rid('a')));
    assert!(!record.is_gateway(&rid('c')));
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["gateways"], serde_json::json!([rid('a'), rid('b')]));
    let back: OrganizationRecord = serde_json::from_value(json).unwrap();
    assert_eq!(back.gateways, vec![rid('a'), rid('b')]);
}

// ---------------------------------------------------------------------------
// 成员表端点化（O1 工作项 1）：deviceUid 聚合 / 同设备墓碑化替换 / wire 兼容
// ---------------------------------------------------------------------------

fn ep(device_uid: Option<&str>, peer_id: Option<&str>, addresses: Vec<&str>) -> OrganizationNodeInfo {
    OrganizationNodeInfo {
        device_uid: device_uid.map(str::to_string),
        peer_id: peer_id.map(str::to_string),
        addresses: addresses.into_iter().map(str::to_string).collect(),
    }
}

/// deviceUid 聚合：同 deviceUid 新 peerId 到达 → 旧 peerId 墓碑化替换（成员
/// 换设备/重装后 peerId 漂移，凭稳定 deviceUid 识别同一台物理设备）。
#[test]
fn device_set_upsert_tombstones_stale_peer_same_device() {
    let mut set = OrganizationDeviceSet::from_single(ep(Some("uid-A"), Some("peer-old"), vec!["/a"]));
    assert_eq!(set.len(), 1);
    // 同 deviceUid 新 peerId：墓碑化旧端点，保留新。
    let changed = set.upsert(&ep(Some("uid-A"), Some("peer-new"), vec!["/a", "/b"]));
    assert!(changed);
    assert_eq!(set.len(), 1);
    assert_eq!(set.iter().next().unwrap().peer_id.as_deref(), Some("peer-new"));
    assert_eq!(set.iter().next().unwrap().addresses, vec!["/a", "/b"]);
    // 再次同 peerId 同地址：无变更。
    assert!(!set.upsert(&ep(Some("uid-A"), Some("peer-new"), vec!["/a", "/b"])));
    assert_eq!(set.len(), 1);
}

/// 多设备聚合：不同 deviceUid 各自独立端点，互不墓碑化。
#[test]
fn device_set_aggregates_distinct_devices() {
    let mut set = OrganizationDeviceSet::default();
    assert!(set.upsert(&ep(Some("uid-A"), Some("peer-A"), vec![])));
    assert!(set.upsert(&ep(Some("uid-B"), Some("peer-B"), vec![])));
    assert_eq!(set.len(), 2);
    let peers: Vec<_> = set.iter().filter_map(|e| e.peer_id.clone()).collect();
    assert_eq!(peers, vec!["peer-A".to_string(), "peer-B".to_string()]);
}

/// 无 deviceUid（旧声明）：按 peerId 兜底聚合。
#[test]
fn device_set_upsert_without_device_uid_keys_by_peer() {
    let mut set = OrganizationDeviceSet::default();
    assert!(set.upsert(&ep(None, Some("peer-x"), vec![])));
    // 同 peerId 更新地址，不新增端点。
    assert!(set.upsert(&ep(None, Some("peer-x"), vec!["/x"])));
    assert_eq!(set.len(), 1);
    assert_eq!(set.iter().next().unwrap().addresses, vec!["/x"]);
}

/// 存量单端点记录 wire 兼容：`{peerId, addresses}` 对象线形可反序列化为端点集，
/// 且重新序列化仍为同一对象线形（golden 向量逐字段一致）。
#[test]
fn legacy_single_endpoint_node_info_deserialize_and_reserialize() {
    let legacy = r#"{"rootId":"aaa","role":"member","joinedAt":1,"addedBy":"bbb","nodeInfo":{"peerId":"12D3KooWLegacy","addresses":["/ip4/1.2.3.4/tcp/1"]}}"#;
    let member: OrganizationMember = serde_json::from_str(legacy).unwrap();
    let set = member.node_info.as_ref().unwrap();
    assert_eq!(set.len(), 1);
    assert_eq!(set.iter().next().unwrap().peer_id.as_deref(), Some("12D3KooWLegacy"));
    // 重新序列化：单端点无 deviceUid → 仍为对象线形。
    let re = serde_json::to_value(&member).unwrap();
    assert!(re["nodeInfo"].is_object());
    assert_eq!(re["nodeInfo"]["peerId"], serde_json::json!("12D3KooWLegacy"));
}

/// 新端点集（多设备）序列化为数组线形。
#[test]
fn multi_device_node_info_serializes_as_array() {
    let mut set = OrganizationDeviceSet::default();
    set.upsert(&ep(Some("uid-A"), Some("peer-A"), vec!["/a"]));
    set.upsert(&ep(Some("uid-B"), Some("peer-B"), vec!["/b"]));
    let member = OrganizationMember {
        root_id: rid('m'),
        role: OrganizationRole::Member,
        joined_at: 1,
        added_by: rid('f'),
        node_info: Some(set),
        ..Default::default()
    };
    let json = serde_json::to_value(&member).unwrap();
    assert!(json["nodeInfo"].is_array());
    assert_eq!(json["nodeInfo"].as_array().unwrap().len(), 2);
}

/// Z2：单端点**含 deviceUid** 也恒序列化为单对象线形（数组仅用于多端点）。
/// 旧端无 deny_unknown_fields 可忽略 deviceUid 键，线形兼容。
#[test]
fn single_endpoint_with_device_uid_serializes_as_object() {
    let set = OrganizationDeviceSet::from_single(ep(Some("uid-A"), Some("peer-A"), vec!["/a"]));
    assert_eq!(set.len(), 1);
    let member = OrganizationMember {
        root_id: rid('m'),
        role: OrganizationRole::Member,
        joined_at: 1,
        added_by: rid('f'),
        node_info: Some(set.clone()),
        ..Default::default()
    };
    let json = serde_json::to_value(&member).unwrap();
    assert!(
        json["nodeInfo"].is_object(),
        "单端点含 deviceUid 应序列化为对象线形"
    );
    assert_eq!(json["nodeInfo"]["deviceUid"], serde_json::json!("uid-A"));
    assert_eq!(json["nodeInfo"]["peerId"], serde_json::json!("peer-A"));
    // 反序列化回读：对象线形 → 单端点集（deviceUid 保留）。
    let back: OrganizationMember = serde_json::from_value(json).unwrap();
    let set_back = back.node_info.as_ref().unwrap();
    assert_eq!(set_back.len(), 1);
    assert_eq!(set_back.iter().next().unwrap().device_uid.as_deref(), Some("uid-A"));
}

/// F2：升级路径——incoming 携带 deviceUid 而既有端点是同 peerId 无 uid 的
/// 旧版条目时，应吸收（移除旧无 uid 端点），不得同 peerId 双端点并存。
#[test]
fn device_set_upsert_adopts_same_peer_legacy_without_uid() {
    // 存量：旧版端点（无 deviceUid，仅 peerId）。
    let mut set = OrganizationDeviceSet::from_single(ep(None, Some("peer-X"), vec!["/old"]));
    // 升级声明：同 peerId，携带 deviceUid。
    let changed = set.upsert(&ep(Some("uid-1"), Some("peer-X"), vec!["/new"]));
    assert!(changed);
    assert_eq!(set.len(), 1, "同 peerId 旧无 uid 端点应被吸收，不得双端点并存");
    let only = set.iter().next().unwrap();
    assert_eq!(only.device_uid.as_deref(), Some("uid-1"), "uid 收养进端点");
    assert_eq!(only.peer_id.as_deref(), Some("peer-X"));
    // 再次 upsert 同 uid 同 peerId 同地址 → 无变更。
    assert!(!set.upsert(&ep(Some("uid-1"), Some("peer-X"), vec!["/new"])));
    assert_eq!(set.len(), 1);
}

/// F2 变体：多端点场景——同 peerId 旧无 uid 端点 + 另一个独立设备端点，
/// 升级时只吸收同 peerId 那个，独立设备端点保留。
#[test]
fn device_set_upsert_adopts_only_same_peer_legacy() {
    let mut set = OrganizationDeviceSet::from_single(ep(None, Some("peer-X"), vec!["/old"]));
    set.upsert(&ep(Some("uid-B"), Some("peer-B"), vec!["/b"]));
    assert_eq!(set.len(), 2);
    // 升级 peer-X：携带 uid-1。
    set.upsert(&ep(Some("uid-1"), Some("peer-X"), vec!["/new"]));
    assert_eq!(set.len(), 2, "只吸收同 peerId 的旧端点，独立设备保留");
    let peers: Vec<_> = set.iter().map(|e| e.peer_id.clone()).collect();
    assert!(peers.contains(&Some("peer-X".to_string())));
    assert!(peers.contains(&Some("peer-B".to_string())));
    assert_eq!(set.iter().find(|e| e.peer_id == Some("peer-X".to_string())).unwrap().device_uid.as_deref(), Some("uid-1"));
}
