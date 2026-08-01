//! 拨号目标构造与 peerId 提取单测。

use spark_core::p2p::peer_targets::*;

#[test]
fn extract_peer_id_prefers_explicit() {
    let info = PeerNodeInfo {
        peer_id: Some("  12D3KooWxxx  ".to_string()),
        addresses: vec!["/ip4/1.2.3.4/tcp/15002/ws/p2p/12D3KooWyyy".to_string()],
    };
    assert_eq!(extract_peer_id(&info).as_deref(), Some("12D3KooWxxx"));
}

#[test]
fn extract_peer_id_from_address_tail() {
    let info = PeerNodeInfo {
        peer_id: None,
        addresses: vec![
            "/ip4/1.2.3.4/tcp/15002/ws".to_string(),
            "/ip4/1.2.3.4/tcp/15002/ws/p2p/12D3KooWzzz".to_string(),
        ],
    };
    assert_eq!(extract_peer_id(&info).as_deref(), Some("12D3KooWzzz"));
    assert_eq!(extract_peer_id(&PeerNodeInfo::default()), None);
}

#[test]
fn build_dial_targets_appends_p2p_segment() {
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/1.2.3.4/tcp/15002/ws/".to_string(),
            "/ip4/5.6.7.8/tcp/15002/ws/p2p/peerA".to_string(),
        ],
    };
    let targets = build_dial_targets(&info).unwrap();
    assert_eq!(
        targets,
        vec![
            "/ip4/1.2.3.4/tcp/15002/ws/".to_string(),
            "/ip4/1.2.3.4/tcp/15002/ws/p2p/peerA".to_string(),
            "/ip4/5.6.7.8/tcp/15002/ws/p2p/peerA".to_string(),
        ]
    );
}

#[test]
fn build_dial_targets_requires_addresses() {
    assert!(build_dial_targets(&PeerNodeInfo::default()).is_err());
}

#[test]
fn build_dial_targets_filters_wildcard_addresses() {
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/0.0.0.0/tcp/15002".to_string(),
            "/ip6/::/tcp/15002".to_string(),
            // loopback 保留（同机互联）。
            "/ip4/127.0.0.1/tcp/15002".to_string(),
            "/ip4/192.168.31.134/tcp/15002/p2p/peerA".to_string(),
        ],
    };
    let targets = build_dial_targets(&info).unwrap();
    assert_eq!(
        targets,
        vec![
            "/ip4/127.0.0.1/tcp/15002".to_string(),
            "/ip4/127.0.0.1/tcp/15002/p2p/peerA".to_string(),
            "/ip4/192.168.31.134/tcp/15002/p2p/peerA".to_string(),
        ]
    );
}

#[test]
fn build_dial_targets_all_wildcard_is_error() {
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/0.0.0.0/tcp/15002".to_string(),
            "/ip6/::/tcp/15002".to_string(),
        ],
    };
    let err = build_dial_targets(&info).unwrap_err().to_string();
    assert_eq!(
        err,
        "malformed message: Member node addresses are required for p2p connect"
    );
}
