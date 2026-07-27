use sha2::{Digest, Sha256};

use spark_core::org::gateway::*;

#[test]
fn key_is_sha256_of_secret_plus_suffix() {
    let secret = "ab".repeat(32);
    let key = org_members_dht_key(&secret);
    let expect = hex::encode(Sha256::digest(format!("{secret}:members").as_bytes()));
    assert_eq!(key, expect);
    assert_eq!(key.len(), 64);
    assert!(
        key.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    // 不同 secret → 不同 key（不可枚举性的构造基础）
    assert_ne!(key, org_members_dht_key(&"cd".repeat(32)));
}

#[test]
fn hint_record_value_roundtrip() {
    let hint = OrgMemberHint {
        peer_id: "12D3KooWGateway".to_string(),
        addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
    };
    let value = hint.to_record_value();
    // 线形：紧凑 JSON，恰好两键
    assert_eq!(
        String::from_utf8(value.clone()).unwrap(),
        r#"{"peerId":"12D3KooWGateway","addresses":["/ip4/1.2.3.4/tcp/15002/ws"]}"#
    );
    let parsed = OrgMemberHint::from_record_value(&value).unwrap();
    assert_eq!(parsed, hint);
    assert_eq!(parsed.peer_id, "12D3KooWGateway");
    assert_eq!(parsed.addresses.len(), 1);
}

#[test]
fn hint_parse_rejects_foreign_shapes() {
    // node-announce 报文（含 type/signature 等键）不得被当作成员提示
    let announce = br#"{"type":"spark-node-announce","version":1,"peerId":"12D3KooW","addresses":[],"timestamp":1,"signature":"xx"}"#;
    assert!(OrgMemberHint::from_record_value(announce).is_none());
    // 缺键 / 多键 / 非 JSON / 空 peerId 一律拒绝
    assert!(OrgMemberHint::from_record_value(br#"{"peerId":"12D3KooW"}"#).is_none());
    assert!(
        OrgMemberHint::from_record_value(
            br#"{"peerId":"12D3KooW","addresses":[],"orgId":"org_x"}"#
        )
        .is_none()
    );
    assert!(OrgMemberHint::from_record_value(b"not json").is_none());
    assert!(OrgMemberHint::from_record_value(br#"{"peerId":"  ","addresses":[]}"#).is_none());
}
