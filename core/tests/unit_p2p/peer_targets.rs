//! 拨号目标构造与 peerId 提取单测。

use std::collections::{HashMap, HashSet};

use spark_core::p2p::overlay_store::AddrScore;
use spark_core::p2p::peer_targets::*;

fn empty() -> (Option<HashMap<String, AddrScore>>, HashSet<String>) {
    (None, HashSet::new())
}

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
    let (meta, self_addrs) = empty();
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/1.2.3.4/tcp/15002/ws/".to_string(),
            "/ip4/5.6.7.8/tcp/15002/ws/p2p/peerA".to_string(),
        ],
    };
    let targets = build_dial_targets(&info, meta.as_ref(), &self_addrs).unwrap();
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
    let (meta, self_addrs) = empty();
    assert!(build_dial_targets(&PeerNodeInfo::default(), meta.as_ref(), &self_addrs).is_err());
}

#[test]
fn build_dial_targets_filters_wildcard_addresses() {
    // M9 静态优先级：私网 IPv4 tcp（rank 2）在 loopback（rank 5）之前
    let (meta, self_addrs) = empty();
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/0.0.0.0/tcp/15002".to_string(),
            "/ip6/::/tcp/15002".to_string(),
            // loopback 保留（同机互联）。
            "/ip4/127.0.0.1/tcp/15002".to_string(),
            "/ip4/192.168.31.134/tcp/15002".to_string(),
        ],
    };
    let targets = build_dial_targets(&info, meta.as_ref(), &self_addrs).unwrap();
    assert_eq!(
        targets,
        vec![
            "/ip4/192.168.31.134/tcp/15002".to_string(),
            "/ip4/192.168.31.134/tcp/15002/p2p/peerA".to_string(),
            "/ip4/127.0.0.1/tcp/15002".to_string(),
            "/ip4/127.0.0.1/tcp/15002/p2p/peerA".to_string(),
        ]
    );
}

#[test]
fn build_dial_targets_all_wildcard_is_error() {
    let (meta, self_addrs) = empty();
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/0.0.0.0/tcp/15002".to_string(),
            "/ip6/::/tcp/15002".to_string(),
        ],
    };
    let err = build_dial_targets(&info, meta.as_ref(), &self_addrs)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "malformed message: Member node addresses are required for p2p connect"
    );
}

#[test]
fn build_dial_targets_dedups_and_sorts_by_score() {
    // 同一地址的 raw 与 /p2p 变体去重为两个目标；高证据地址优先
    let info = PeerNodeInfo {
        peer_id: Some("peerA".to_string()),
        addresses: vec![
            "/ip4/9.9.9.9/tcp/15002".to_string(),       // 高证据
            "/ip4/9.9.9.9/tcp/15002/p2p/peerA".to_string(), // 同 base，去重
            "/ip4/8.8.8.8/tcp/15002".to_string(),       // 零分
        ],
    };
    let mut meta = HashMap::new();
    meta.insert(
        "/ip4/9.9.9.9/tcp/15002".to_string(),
        AddrScore { success_count: 2, last_success_at: 500, ..Default::default() },
    );
    let targets = build_dial_targets(&info, Some(&meta), &HashSet::new()).unwrap();
    assert_eq!(
        targets[0],
        "/ip4/9.9.9.9/tcp/15002",
        "高证据地址优先"
    );
    assert_eq!(targets[1], "/ip4/9.9.9.9/tcp/15002/p2p/peerA");
    assert_eq!(targets[2], "/ip4/8.8.8.8/tcp/15002");
}
