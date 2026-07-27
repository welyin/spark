//! 监听端口解析、归一与挑选单测。

use spark_core::p2p::listen_port::*;

#[test]
fn parse_ws_port() {
    assert_eq!(
        parse_ws_listen_port(&["/ip4/0.0.0.0/tcp/15002/ws".to_string()]),
        Some(15002)
    );
    assert_eq!(
        parse_ws_listen_port(&["/ip4/0.0.0.0/tcp/15002/ws/p2p/abc".to_string()]),
        Some(15002)
    );
    assert_eq!(
        parse_ws_listen_port(&["/ip4/0.0.0.0/tcp/15002".to_string()]),
        None
    );
    assert_eq!(
        parse_ws_listen_port(&["/ip4/0.0.0.0/tcp/80/ws".to_string()]),
        None
    );
    assert_eq!(parse_ws_listen_port(&[]), None);
}

#[test]
fn normalize_port() {
    assert_eq!(normalize_preferred_port(Some("15002"), 1), 15002);
    assert_eq!(normalize_preferred_port(Some("80"), 15002), 15002);
    assert_eq!(normalize_preferred_port(Some("abc"), 15002), 15002);
    assert_eq!(normalize_preferred_port(None, 15002), 15002);
}

#[test]
fn pick_port_scans_and_binds() {
    let port = pick_listen_port(0, Some(0), false);
    assert_eq!(port, 0); // 0 非法 → 直接退化
    let picked = pick_listen_port(25000, Some(10), false);
    assert!(picked >= 25000 || picked == 0);
}

#[test]
fn listen_addrs_dual_stack() {
    let addrs = build_listen_addrs(15002, true);
    assert_eq!(
        addrs,
        vec![
            "/ip4/0.0.0.0/tcp/15002",
            "/ip4/0.0.0.0/tcp/15002/ws",
            "/ip6/::/tcp/15002",
            "/ip6/::/tcp/15002/ws",
        ]
    );
    let v4 = build_listen_addrs(15002, false);
    assert_eq!(v4.len(), 2);
}
