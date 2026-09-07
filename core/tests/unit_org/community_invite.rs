//! 共同体邀请码载荷（community-org-invite）编解码与 join-notice 体纲单测：
//! 线形键序/缺省丢键/校验顺序与新鲜度口径（对齐 unit_org/invite.rs 的个人
//! 邀请码用例）。

use spark_core::org::community_invite::{
    COMMUNITY_ORG_INVITE_TYPE, COMMUNITY_ORG_JOIN_NOTICE_TYPE, CommunityInviteError,
    CommunityOrgInvitePayload, build_community_invite_mail_body, build_community_join_notice,
    community_domain, decode_community_org_invite_at, encode_community_org_invite,
};
use spark_core::org::invite::OrgInviteInviter;
use spark_core::org::types::OrgBinding;

const NOW: i64 = 1_720_000_000_000;

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn genesis_org_id(ch: char) -> String {
    format!("org_{}", ch.to_string().repeat(64))
}

fn payload() -> CommunityOrgInvitePayload {
    CommunityOrgInvitePayload::new(
        genesis_org_id('a'),
        "阳光共同体",
        OrgInviteInviter {
            root_id: rid('b'),
            peer_id: Some("12D3KooWAdmin".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/15002".to_string()],
        },
        NOW,
    )
}

#[test]
fn codec_roundtrip_and_optional_keys() {
    let mut p = payload();
    // 可省字段缺省丢键（线形不含 communityOrgAddress/replyDomainId）
    let code = encode_community_org_invite(&p);
    let raw = String::from_utf8(
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &code).unwrap(),
    )
    .unwrap();
    assert!(!raw.contains("communityOrgAddress"));
    assert!(!raw.contains("replyDomainId"));

    let decoded = decode_community_org_invite_at(&code, NOW).unwrap();
    assert_eq!(decoded, p);

    // 附带回信寻址后往返一致
    p.community_org_address = Some("{\"orgAddress\":\"...\"}".to_string());
    p.reply_domain_id = Some("dGVzdA==".to_string());
    let decoded = decode_community_org_invite_at(&encode_community_org_invite(&p), NOW).unwrap();
    assert_eq!(decoded, p);
}

#[test]
fn decode_validation_order() {
    let p = payload();
    // 空输入 / 非法 base64 / 非本类型
    assert_eq!(
        decode_community_org_invite_at("  ", NOW).unwrap_err(),
        CommunityInviteError::Empty
    );
    assert_eq!(
        decode_community_org_invite_at("!!!", NOW).unwrap_err(),
        CommunityInviteError::Malformed
    );
    let mut wrong_type = p.clone();
    wrong_type.type_ = "spark-org-invite".to_string();
    assert_eq!(
        decode_community_org_invite_at(&encode_community_org_invite(&wrong_type), NOW).unwrap_err(),
        CommunityInviteError::NotCommunityInvite
    );

    // 缺 communityOrgId / 非法 orgId（非双形态）
    let raw = r#"{"type":"community-org-invite","version":1,"inviter":{"rootId":""#.to_string()
        + &rid('b')
        + r#""},"createdAt":1720000000000}"#;
    let code = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
    assert_eq!(
        decode_community_org_invite_at(&code, NOW).unwrap_err(),
        CommunityInviteError::MissingOrgId
    );
    let mut bad_id = p.clone();
    bad_id.community_org_id = "org_xyz".to_string();
    assert_eq!(
        decode_community_org_invite_at(&encode_community_org_invite(&bad_id), NOW).unwrap_err(),
        CommunityInviteError::InvalidOrgId
    );

    // 邀请人 rootId 非法
    let mut bad_inviter = p.clone();
    bad_inviter.inviter.root_id = "nope".to_string();
    assert_eq!(
        decode_community_org_invite_at(&encode_community_org_invite(&bad_inviter), NOW)
            .unwrap_err(),
        CommunityInviteError::InvalidInviter
    );

    // 过期（>24h）与 createdAt 缺失（按 TS 口径归一为 0 → 必过期）
    let mut stale = p.clone();
    stale.created_at = NOW - spark_core::org::ORG_INVITE_MAX_AGE_MS - 1;
    assert_eq!(
        decode_community_org_invite_at(&encode_community_org_invite(&stale), NOW).unwrap_err(),
        CommunityInviteError::Expired
    );
    let raw = format!(
        r#"{{"type":"community-org-invite","version":1,"communityOrgId":"{}","inviter":{{"rootId":"{}"}}}}"#,
        genesis_org_id('a'),
        rid('b')
    );
    let code = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
    assert_eq!(
        decode_community_org_invite_at(&code, NOW).unwrap_err(),
        CommunityInviteError::Expired
    );

    // 未来的 createdAt 不设上限（同个人邀请口径）；inviter 无地址线索也放行
    // （传输走 org-mail，不直连）
    let mut future = p.clone();
    future.created_at = NOW + 3_600_000;
    future.inviter.peer_id = None;
    future.inviter.addresses = vec![];
    let decoded =
        decode_community_org_invite_at(&encode_community_org_invite(&future), NOW + 3_600_000)
            .unwrap();
    assert_eq!(decoded.created_at, NOW + 3_600_000);
}

#[test]
fn mail_body_and_join_notice_shape() {
    let p = payload();
    let code = encode_community_org_invite(&p);
    let body = build_community_invite_mail_body(&p, &code);
    assert_eq!(body["type"], COMMUNITY_ORG_INVITE_TYPE);
    assert_eq!(body["code"], code);
    assert_eq!(body["communityOrgId"], genesis_org_id('a'));
    assert_eq!(body["inviterRootId"], rid('b'));

    // join notice：无公开绑定时丢 orgBinding 键；公开时携带
    let notice = build_community_join_notice(&genesis_org_id('a'), &rid('c'), None, NOW);
    assert_eq!(notice["type"], COMMUNITY_ORG_JOIN_NOTICE_TYPE);
    assert_eq!(notice["memberIdentity"], rid('c'));
    assert!(notice.get("orgBinding").is_none());

    let binding = OrgBinding {
        org_id: Some(genesis_org_id('d')),
        org_address: Some("a".repeat(55)),
    };
    let notice = build_community_join_notice(&genesis_org_id('a'), &rid('c'), Some(&binding), NOW);
    assert_eq!(notice["orgBinding"]["orgId"], genesis_org_id('d'));
    assert_eq!(notice["orgBinding"]["orgAddress"], "a".repeat(55));
}

#[test]
fn community_domain_string() {
    // org-genesis §4：共同体成员身份域串 = community:{communityOrgId}
    assert_eq!(community_domain("org_abc"), "community:org_abc");
}
