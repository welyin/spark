//! 直连协议帧与限流器单测。

use serde_json::json;
use spark_core::org::recovery::RecoveryViewItem;
use spark_core::p2p::direct::*;
use spark_core::p2p::peer_targets::PeerNodeInfo;

#[test]
fn version_response_roundtrip() {
    let frame = build_peer_version_response("1.2.3", "12D3KooWNode", 1_720_000_000_000);
    let text = serde_json::to_string(&frame).unwrap();
    assert_eq!(
        text,
        "{\"type\":\"peer-version\",\"appVersion\":\"1.2.3\",\"nodeId\":\"12D3KooWNode\",\"timestamp\":1720000000000}"
    );
    assert_eq!(parse_peer_version_response(&text).as_deref(), Some("1.2.3"));
    assert_eq!(parse_peer_version_response("{\"appVersion\":\"  \"}"), None);
    assert_eq!(parse_peer_version_response("garbage"), None);
}

#[test]
fn exchange_want_normalization() {
    assert_eq!(normalize_exchange_want(None), 16);
    assert_eq!(normalize_exchange_want(Some(&json!(0))), 16);
    assert_eq!(normalize_exchange_want(Some(&json!(-5))), 16);
    assert_eq!(normalize_exchange_want(Some(&json!(3))), 3);
    assert_eq!(normalize_exchange_want(Some(&json!(100))), 16);
    assert_eq!(normalize_exchange_want(Some(&json!("x"))), 16);
}

#[test]
fn exchange_frames() {
    let req = build_exchange_request(16);
    assert_eq!(req, "{\"type\":\"peer-exchange-request\",\"want\":16}");
    let resp = build_exchange_response(
        true,
        &[PeerExchangeSample {
            peer_id: "p1".to_string(),
            addresses: vec!["/ip4/1.2.3.4/tcp/1/ws".to_string()],
            last_seen_at: 5,
        }],
        None,
    );
    let parsed = parse_exchange_response(&resp).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].peer_id, "p1");
    let limited = build_exchange_response(false, &[], Some("rate-limited"));
    assert!(parse_exchange_response(&limited).is_none());
}

#[test]
fn exchange_sample_filter() {
    let sample = PeerExchangeSample {
        peer_id: "p1".to_string(),
        addresses: vec!["/a".to_string(), String::new()],
        last_seen_at: 0,
    };
    assert!(filter_incoming_sample(&sample, "p1", "responder").is_none());
    assert!(filter_incoming_sample(&sample, "self", "p1").is_none());
    let (pid, addrs) = filter_incoming_sample(&sample, "self", "responder").unwrap();
    assert_eq!(pid, "p1");
    assert_eq!(addrs, vec!["/a".to_string()]);
}

#[test]
fn recovery_request_parse_strict_token() {
    let good = build_recovery_request(&"ab".repeat(32), 2, 8);
    let query = parse_recovery_request(&good).unwrap();
    assert_eq!(query.token, "ab".repeat(32));
    assert_eq!(query.ttl, 2);
    assert_eq!(query.want, 8);
    // 非 64 hex 拒绝
    assert!(parse_recovery_request(&build_recovery_request("xyz", 2, 8)).is_none());
    assert!(parse_recovery_request(&build_recovery_request(&"AB".repeat(32), 2, 8)).is_none());
    // want 归一
    let q = parse_recovery_request(&build_recovery_request(&"cd".repeat(32), 0, 100)).unwrap();
    assert_eq!(q.want, 8);
    assert_eq!(normalize_recovery_ttl(q.ttl), 0);
    assert_eq!(normalize_recovery_ttl(5), 2);
}

#[test]
fn recovery_response_filter_and_dedupe() {
    let peers = vec![
        PeerNodeInfo {
            peer_id: Some("p1".into()),
            addresses: vec!["/a".into()],
        },
        PeerNodeInfo {
            peer_id: Some("p1".into()),
            addresses: vec!["/b".into(), "/a".into()],
        },
        PeerNodeInfo {
            peer_id: None,
            addresses: vec!["/c".into()],
        },
    ];
    let merged = dedupe_recovery_peers(peers, 8);
    assert_eq!(merged.len(), 2);
    assert_eq!(
        merged[0].addresses,
        vec!["/a".to_string(), "/b".to_string()]
    );
}

#[test]
fn recovery_view_match() {
    let view = vec![RecoveryViewItem {
        org_id: "org_0123456789abcdef".to_string(),
        recovery_secret: "ef".repeat(32),
        member_node_infos: vec![spark_core::org::types::OrganizationNodeInfo {
            peer_id: Some("peerA".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws".to_string()],
        }],
    }];
    let now = 1_720_000_000_000i64;
    let [current, _] = view[0].active_tokens(now);
    let hit = match_recovery_view(&view, &current, 8, now).unwrap();
    assert_eq!(hit[0].peer_id.as_deref(), Some("peerA"));
    assert!(match_recovery_view(&view, &"00".repeat(32), 8, now).is_none());
}

#[test]
fn org_share_request_dispatch() {
    let share = build_org_share_request(json!({"syncId":"s1"}));
    let (kind, payload) = parse_org_share_request(&share).unwrap().unwrap();
    assert_eq!(kind, OrgShareRequestKind::OrgShare);
    assert_eq!(payload["syncId"], "s1");

    let list = build_pull_list_request(&"aa".repeat(32), Some("peerX"), None);
    let (kind, _) = parse_org_share_request(&list).unwrap().unwrap();
    assert_eq!(kind, OrgShareRequestKind::OrgPullList);

    let org = build_pull_org_request(&"aa".repeat(32), None, "org_0123456789abcdef");
    let (kind, payload) = parse_org_share_request(&org).unwrap().unwrap();
    assert_eq!(kind, OrgShareRequestKind::OrgPullOrg);
    assert_eq!(payload["orgId"], "org_0123456789abcdef");

    assert!(parse_org_share_request("not json").is_err());
    assert!(
        parse_org_share_request("{\"type\":\"bogus\"}")
            .unwrap()
            .is_none()
    );
}

#[test]
fn org_share_direct_response_matching() {
    let ok = build_org_share_ack_response(Some("sync-1"), "org_x", "receiver");
    assert!(parse_org_share_direct_response(&ok, "sync-1"));
    assert!(!parse_org_share_direct_response(&ok, "sync-2"));
    assert!(!parse_org_share_direct_response(
        &build_org_share_error_response("not accepted"),
        "sync-1"
    ));
    assert!(!parse_org_share_direct_response("garbage", "sync-1"));
}

#[test]
fn rate_limiter() {
    let mut limiter = MinIntervalRateLimiter::new(60_000);
    assert!(!limiter.is_rate_limited("p1", 1000));
    assert!(limiter.is_rate_limited("p1", 60_999));
    assert!(!limiter.is_rate_limited("p1", 61_000));
    assert!(!limiter.is_rate_limited("p2", 61_000));
}
