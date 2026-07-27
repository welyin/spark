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
fn text_and_domain_normalization() {
    assert_eq!(
        normalize_text("  hello   world \n ", "Name").unwrap(),
        "hello world"
    );
    assert!(normalize_text("   ", "Name").is_err());
    assert_eq!(
        normalize_plugin_domain(" plugin:chat ").unwrap(),
        "plugin:chat"
    );
    assert!(normalize_plugin_domain("chat").is_err());
    assert!(normalize_plugin_domain("plugin:").is_err());
    assert!(normalize_plugin_domain("  ").is_err());
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
        peer_id: Some("abc".to_string()),
        addresses: vec![],
    };
    assert!(matches!(
        normalize_node_info(&short),
        Err(OrgError::InvalidPeerId)
    ));
    // trim + 滤空
    let ok = OrganizationNodeInfo {
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
        peer_id: Some("   ".to_string()),
        addresses: vec![" ".to_string()],
    };
    assert_eq!(
        normalize_optional_node_info(Some(&all_blank)).unwrap(),
        None
    );
    let valid = OrganizationNodeInfo {
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
        extra: Default::default(),
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
