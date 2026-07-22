//! 监听端口选择与双栈判定（对齐 listen-port.ts；修复 TS "IPv4 空闲但 IPv6 被占"
//! 误判可用的探测口径——Rust 直接对两栈都验证，见 core/spec/p2p-messages.md §1.2）。

use std::net::{TcpListener, ToSocketAddrs};

use super::constants::{LISTEN_PORT_SCAN_RANGE, P2P_DEFAULT_LISTEN_WS_PORT};

const MIN_PORT: u32 = 1024;
const MAX_PORT: u32 = 65535;

/// 从监听地址列表解析实际 ws 端口（正则口径 `/\/tcp\/(\d+)\/ws(?:\/|$)/`）。
pub fn parse_ws_listen_port(addresses: &[String]) -> Option<u16> {
    for address in addresses {
        let segments: Vec<&str> = address.split('/').collect();
        for (i, seg) in segments.iter().enumerate() {
            if *seg == "tcp"
                && let Some(port_str) = segments.get(i + 1)
                && let Ok(port) = port_str.parse::<u16>()
                && segments.get(i + 2).is_some_and(|s| *s == "ws")
                && (MIN_PORT..=MAX_PORT).contains(&(port as u32))
            {
                return Some(port);
            }
        }
    }
    None
}

/// 归一化持久化的首选端口（非法值回退默认）。
pub fn normalize_preferred_port(value: Option<&str>, fallback: u16) -> u16 {
    if let Some(text) = value
        && let Ok(port) = text.trim().parse::<u16>()
        && (MIN_PORT..=MAX_PORT).contains(&(port as u32))
    {
        return port;
    }
    fallback
}

/// 探测某个地址能否绑定。
fn probe_bind(addr: &str) -> bool {
    TcpListener::bind(addr).is_ok()
}

/// 端口可用性：IPv4 通配可绑；`ipv6` 为 true 时同时验证 IPv6 通配（ipv6Only 语义）。
///
/// 注意：macOS/Linux 上 `::` 默认双栈（v6only=0）会同时占住 v4，与 libp2p 的绑定
/// 行为一致——TS 修复后的口径即"两栈都验证"。
pub fn is_tcp_port_available(port: u16, ipv6: bool) -> bool {
    if !(MIN_PORT..=MAX_PORT).contains(&(port as u32)) {
        return false;
    }
    if !probe_bind(&format!("0.0.0.0:{port}")) {
        return false;
    }
    if ipv6 {
        // ipv6Only 语义：只验证 v6 通配
        let v6 = format!("[::]:{port}");
        let Ok(mut addrs) = v6.to_socket_addrs() else {
            return false;
        };
        if addrs.next().is_none() {
            return false;
        }
        // std 无直接 v6only 控制；绑定 [::]:port 在双栈系统上会连带验证 v4，
        // 比目标口径更严格但更保守（不会误判可用）
        if !probe_bind(&v6) {
            return false;
        }
    }
    true
}

/// 从首选端口起向后扫描最多 `scan_range` 个，全部占用退化为 0（OS 分配）。
pub fn pick_listen_port(preferred: u16, scan_range: Option<u16>, ipv6: bool) -> u16 {
    let scan_range = scan_range.unwrap_or(LISTEN_PORT_SCAN_RANGE);
    let start = normalize_preferred_port(Some(&preferred.to_string()), preferred);
    for offset in 0..=scan_range {
        let candidate = start as u32 + offset as u32;
        if candidate > MAX_PORT {
            break;
        }
        let port = candidate as u16;
        if is_tcp_port_available(port, ipv6) {
            return port;
        }
    }
    0
}

/// 探测 OS 是否可绑定 IPv6 通配地址。
pub fn supports_ipv6() -> bool {
    probe_bind("[::]:0")
}

/// 构造监听 multiaddr：IPv4 + IPv6 双栈同端口；port 为 0 时两栈都由 OS 分配。
///
/// Rust 内核同时监听裸 TCP 与 WebSocket（双协议栈同端口）。
pub fn build_listen_addrs(port: u16, ipv6_enabled: bool) -> Vec<String> {
    let mut addresses = vec![
        format!("/ip4/0.0.0.0/tcp/{port}"),
        format!("/ip4/0.0.0.0/tcp/{port}/ws"),
    ];
    if ipv6_enabled {
        addresses.push(format!("/ip6/::/tcp/{port}"));
        addresses.push(format!("/ip6/::/tcp/{port}/ws"));
    }
    addresses
}

/// 首选端口默认值。
pub fn default_listen_port() -> u16 {
    P2P_DEFAULT_LISTEN_WS_PORT
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
