use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use spark_core::org::invite::*;

const NOW: i64 = 1_720_000_000_000;

fn rid() -> String {
    "a".repeat(64)
}

fn sample_payload() -> OrgInvitePayload {
    OrgInvitePayload::new(
        "org_0123456789abcdef",
        "星火  组织",
        OrgInviteInviter {
            root_id: rid(),
            peer_id: Some("12D3KooWPeer".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
        },
        NOW - 1000,
    )
}

#[test]
fn encode_decode_roundtrip() {
    let payload = sample_payload();
    let code = encode_org_invite(&payload);
    // base64url 无 padding、无 +/
    assert!(!code.contains(['+', '/', '=']));
    let decoded = decode_org_invite_at(&code, NOW).unwrap();
    assert_eq!(decoded, payload);
}

#[test]
fn encode_matches_fixed_layout() {
    // 固定 payload → 固定编码（JSON 键序 = 结构体声明序 = TS 插入序）
    let payload = OrgInvitePayload::new(
        "org_0123456789abcdef",
        "测试",
        OrgInviteInviter {
            root_id: rid(),
            peer_id: None,
            addresses: vec!["/ip4/1.2.3.4/tcp/1/ws".to_string()],
        },
        1_720_000_000_000,
    );
    let code = encode_org_invite(&payload);
    let expect_json = format!(
        "{{\"type\":\"spark-org-invite\",\"version\":1,\"orgId\":\"org_0123456789abcdef\",\"orgName\":\"测试\",\"inviter\":{{\"rootId\":\"{}\",\"addresses\":[\"/ip4/1.2.3.4/tcp/1/ws\"]}},\"createdAt\":1720000000000}}",
        rid()
    );
    assert_eq!(
        code,
        URL_SAFE_NO_PAD.encode(expect_json.as_bytes()),
        "编码必须等于固定键序紧凑 JSON 的 base64url"
    );
}

#[test]
fn decode_rejects_empty() {
    assert_eq!(decode_org_invite_at("", NOW), Err(OrgInviteError::Empty));
    assert_eq!(decode_org_invite_at("   ", NOW), Err(OrgInviteError::Empty));
}

#[test]
fn decode_rejects_malformed() {
    assert_eq!(
        decode_org_invite_at("!!!not-base64!!!", NOW),
        Err(OrgInviteError::Malformed)
    );
    // 合法 base64url 但非 JSON
    let not_json = URL_SAFE_NO_PAD.encode(b"hello");
    assert_eq!(
        decode_org_invite_at(&not_json, NOW),
        Err(OrgInviteError::Malformed)
    );
}

#[test]
fn decode_rejects_wrong_type_or_version() {
    for raw in [
        r#"{"type":"other","version":1,"orgId":"org_x","inviter":{},"createdAt":1}"#,
        r#"{"type":"spark-org-invite","version":2,"orgId":"org_x","inviter":{},"createdAt":1}"#,
    ] {
        let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
        assert_eq!(
            decode_org_invite_at(&code, NOW),
            Err(OrgInviteError::NotSparkOrgInvite)
        );
    }
}

#[test]
fn decode_rejects_missing_org_id() {
    let raw = format!(
        r#"{{"type":"spark-org-invite","version":1,"orgId":"  ","inviter":{{"rootId":"{}"}},"createdAt":{}}}"#,
        rid(),
        NOW
    );
    let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::MissingOrgId)
    );
}

#[test]
fn decode_rejects_invalid_inviter_root() {
    // 大写 hex 合法（trim+lowercase 后校验）；空串与非 hex 拒绝
    for root in ["", "xyz", &"g".repeat(64), &rid()[..63]] {
        let raw = format!(
            r#"{{"type":"spark-org-invite","version":1,"orgId":"org_x","inviter":{{"rootId":"{root}","peerId":"12D3KooWPeer"}},"createdAt":{NOW}}}"#
        );
        let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
        assert_eq!(
            decode_org_invite_at(&code, NOW),
            Err(OrgInviteError::InvalidInviter)
        );
    }
    // inviter 整个缺失
    let raw = format!(
        r#"{{"type":"spark-org-invite","version":1,"orgId":"org_x","createdAt":{NOW}}}"#
    );
    let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::InvalidInviter)
    );
}

#[test]
fn decode_rejects_missing_address_and_peer() {
    let raw = format!(
        r#"{{"type":"spark-org-invite","version":1,"orgId":"org_x","inviter":{{"rootId":"{}","peerId":"  ","addresses":["", 42]}},"createdAt":{}}}"#,
        rid(),
        NOW
    );
    let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::MissingInviterAddress)
    );
}

#[test]
fn decode_rejects_expired_and_nonpositive_created_at() {
    // 恰好 24h + 1ms → 过期
    let mut p = sample_payload();
    p.created_at = NOW - ORG_INVITE_MAX_AGE_MS - 1;
    let code = encode_org_invite(&p);
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::Expired)
    );

    // createdAt = 0 / 负数 → 过期
    p.created_at = 0;
    let code = encode_org_invite(&p);
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::Expired)
    );

    // createdAt 非 number → 按 0 处理 → 过期
    let raw = format!(
        r#"{{"type":"spark-org-invite","version":1,"orgId":"org_x","inviter":{{"rootId":"{}","peerId":"12D3KooWPeer"}},"createdAt":"yesterday"}}"#,
        rid()
    );
    let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    assert_eq!(
        decode_org_invite_at(&code, NOW),
        Err(OrgInviteError::Expired)
    );
}

#[test]
fn decode_accepts_boundary_and_future_created_at() {
    // 恰好 24h → 仍有效（TS 为 `>` 严格大于才过期）
    let mut p = sample_payload();
    p.created_at = NOW - ORG_INVITE_MAX_AGE_MS;
    let code = encode_org_invite(&p);
    assert!(decode_org_invite_at(&code, NOW).is_ok());

    // 未来 createdAt 不设上限（spec §2.3 明确复刻的无上限行为）
    p.created_at = NOW + 10 * 365 * 24 * 60 * 60 * 1000;
    let code = encode_org_invite(&p);
    let decoded = decode_org_invite_at(&code, NOW).unwrap();
    assert_eq!(decoded.created_at, NOW + 10 * 365 * 24 * 60 * 60 * 1000);
}

#[test]
fn decode_normalizes_fields() {
    let raw = format!(
        r#"{{"type":"spark-org-invite","version":1,"orgId":"  org_abc  ","inviter":{{"rootId":"  {}  ","peerId":"  12D3KooWPeer ","addresses":[" /ip4/1.2.3.4/tcp/1 ", 7, ""]}},"createdAt":{}}}"#,
        rid().to_uppercase(),
        NOW
    );
    let code = URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let decoded = decode_org_invite_at(&code, NOW).unwrap();
    assert_eq!(decoded.org_id, "org_abc");
    assert_eq!(decoded.org_name, "");
    assert_eq!(decoded.inviter.root_id, rid());
    assert_eq!(decoded.inviter.peer_id.as_deref(), Some("12D3KooWPeer"));
    // TS 只过滤非字符串与全空串，不 trim 内容
    assert_eq!(decoded.inviter.addresses, vec![" /ip4/1.2.3.4/tcp/1 "]);
}
