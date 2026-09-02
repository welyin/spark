//! 拨号目标构造与 peerId 提取（对齐 peer-targets.ts）。

/// 一个可连接的远端节点描述（TS `PeerNodeInfo`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerNodeInfo {
    /// peerId 可省（可从地址 `/p2p/<peerId>` 尾段推导）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

use std::net::IpAddr;

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

/// 提取目标 peerId：优先显式 `peer_id`，回退从地址 `/p2p/<peerId>` 尾段解析。
pub fn extract_peer_id(node_info: &PeerNodeInfo) -> Option<String> {
    if let Some(direct) = node_info.peer_id.as_deref().map(str::trim)
        && !direct.is_empty()
    {
        return Some(direct.to_string());
    }
    for address in &node_info.addresses {
        if let Some(pos) = address.rfind("/p2p/") {
            let tail = &address[pos + 5..];
            // 仅接受尾段（不再含 '/'）
            if !tail.is_empty() && !tail.contains('/') {
                return Some(tail.to_string());
            }
        }
    }
    None
}

/// 单个拨号候选的过滤判定：返回 `Some` 表示保留、`None` 表示剔除。
///
/// 规则（纯本地候选筛选，不改协议线形）：
/// - 通配地址（0.0.0.0/::）不可路由，对端拿到也回拨不通，候选中剔除；
/// - Android 平台未启用 ws 传输层（`cfg!(target_os = "android")`），含 `/ws`
///   或 `/wss` 段的地址拨了也是 `MultiaddrNotSupported` 白等，直接剔除；
/// - loopback（127.0.0.1/::1）与私网地址**保留**（同机/局域网场景还要用），
///   只由 [`sort_addresses`] 降权排后。
///
/// `is_android` 作为参数传入以便跨平台单测（测试不依赖宿主平台）。
pub(crate) fn filter_dial_candidate(address: &str, is_android: bool) -> Option<String> {
    let trimmed = address.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let Ok(ma) = trimmed.parse::<libp2p::Multiaddr>() else {
        return Some(trimmed); // 解析失败原样保留，由后续拨号环节报错
    };
    // 通配地址不可路由（loopback 保留供同机互联）。
    if ma.iter().next().is_some_and(|p| match p {
        Protocol::Ip4(ip) => ip.is_unspecified(),
        Protocol::Ip6(ip) => ip.is_unspecified(),
        _ => false,
    }) {
        return None;
    }
    if is_android
        && ma
            .iter()
            .any(|p| matches!(p, Protocol::Ws(_) | Protocol::Wss(_)))
    {
        return None;
    }
    Some(trimmed)
}

/// 电路地址拨号补全：`<relay-addr>/p2p/<relay>/p2p-circuit` 形态缺目的段时
/// 追加 `/p2p/<dst>`——libp2p relay client 拨号要求地址携带目的 peer 段
/// （否则 MissingDstPeerId，真机实测：DHT/announce 分发的电路地址在各拨号
/// 路径被原样使用，永远拨不通）；已含目的段或非电路地址原样返回。
pub(crate) fn ensure_circuit_dst_peer(address: &str, dst_peer: &str) -> String {
    let trimmed = address.trim();
    let Ok(ma) = trimmed.parse::<Multiaddr>() else {
        return trimmed.to_string();
    };
    let mut has_circuit = false;
    let mut last_is_p2p = false;
    for p in ma.iter() {
        match p {
            Protocol::P2pCircuit => {
                has_circuit = true;
                last_is_p2p = false;
            }
            Protocol::P2p(_) => last_is_p2p = true,
            _ => last_is_p2p = false,
        }
    }
    if !has_circuit || last_is_p2p {
        return trimmed.to_string();
    }
    let Ok(dst) = dst_peer.parse::<libp2p::PeerId>() else {
        return trimmed.to_string();
    };
    let mut out = ma;
    out.push(Protocol::P2p(dst.into()));
    out.to_string()
}

/// 判定一个 multiaddr 首段 IP 是否可作为**公网 external 地址**广播
/// （wrong-peer-id-address-pollution S1 helper，与 `filter_kad_addr` 同族）。
///
/// 返回 `true` = 保留（可作 external 广播）、`false` = 剔除。
///
/// 背景：`swarm.external_addresses()` 来自 identify observe / AutoNAT 确认，定位是
/// 「本机在公网上的可达地址」。私有 / 链路本地 / 回环 / 通配 / CGNAT 段跨 NAT 不可达，
/// 且 LAN 可达由 `expand_wildcard_listeners` 展开的具体监听地址覆盖，作为 external
/// 广播是错误且重复的（同 LAN 多实例会把彼此私有 IP 当 external → 广播 → 对端
/// remember 污染 → WrongPeerId）。
///
/// 判定规则：
/// - **IPv4**：剔 回环 / 私有（10/8、172.16/12、192.168/16）/ 链路本地（169.254/16）/
///   通配（0.0.0.0）/ CGNAT（100.64/10，标准库 `is_private()` 不覆盖，手写）；其余保留。
/// - **IPv6**：剔 回环（::1）/ 唯一本地（fc00::/7）/ 单播链路本地（fe80::/10）/
///   通配（::）；其余保留。
/// - **非 IP 首段**（dns、circuit 等）：无法按 IP 判定，保留不误伤（external 段
///   理论上均为 IP 开头，此分支为防御性兜底）。
pub(crate) fn is_public_external_addr(addr: &Multiaddr) -> bool {
    let Some(ip) = addr.iter().next().and_then(|p| match p {
        Protocol::Ip4(ip) => Some(IpAddr::V4(ip)),
        Protocol::Ip6(ip) => Some(IpAddr::V6(ip)),
        _ => None,
    }) else {
        return true; // 非 IP 首段无法判定，保留
    };
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return false;
            }
            // CGNAT 100.64.0.0/10：标准库 is_private() 不覆盖。首字节 100、
            // 第二字节高 2 位 01（0x40..=0x7f）。
            let octets = v4.octets();
            if octets[0] == 100 && (octets[1] & 0xC0) == 0x40 {
                return false;
            }
            true
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_unspecified())
        }
    }
}

/// kad 路由表地址过滤（kad-addr-filtering root fix，S1 helper）。
///
/// 返回 `true` 表示该地址**允许**灌进 kad 路由表；`false` 表示剔除。
///
/// 背景：libp2p-kad 行为层对路由表内「未连接 peer」用默认 `PortUse::Reuse`
/// （复用监听端口）自动拨号。若本机监听地址 / ws 地址进入路由表，kad 会拨
/// 本机监听端口 → 与自监听 socket 冲突 → `AddrInUse(10048)` 刷屏。identify
/// 路径（swarm_events）此前零过滤 `kad.add_address`，是全仓库唯一漏网口。
///
/// 三层剔除：
/// 1. **本机监听地址**（`self_addrs`，剥离 `/p2p` 段比对）：永远不可能作为
///    对端可达地址（那是本机自己的 listener），剔除不误伤任何真实 peer；
/// 2. **通配地址**（0.0.0.0/::，复用 [`filter_dial_candidate`] 语义）：不可路由；
/// 3. **ws/wss 形态**：桌面端自监听 ws（ws 与 tcp 同端口），kad `PortUse::Reuse`
///    拨任何 ws 地址都会撞自 ws listener → AddrInUse。故桌面 kad 一律不拨 ws。
///    取舍：纯 ws-only 节点（纯浏览器）不被桌面 kad 路由；但浏览器/ws 中继
///    通常也上报 TCP 地址，Spark 桌面间主链路为 TCP。
///
/// 仅影响「哪些地址进 kad 路由表」，不触协议线形/存储键/对外契约。
pub(crate) fn filter_kad_addr(
    addr: &libp2p::Multiaddr,
    self_addrs: &std::collections::HashSet<String>,
) -> bool {
    // 1) 本机监听地址：剥离 /p2p 段精确比对（identify 上报形态多样）
    let base = addr
        .to_string()
        .split("/p2p/")
        .next()
        .unwrap_or(&addr.to_string())
        .to_string();
    if self_addrs
        .iter()
        .any(|sa| sa.split("/p2p/").next().unwrap_or(sa) == base)
    {
        return false;
    }
    // 2) 通配地址不可路由
    if filter_dial_candidate(&base, false).is_none() {
        return false;
    }
    // 3) ws/wss 形态不灌 kad（桌面 kad 不拨 ws）
    if addr
        .iter()
        .any(|p| matches!(p, Protocol::Ws(_) | Protocol::Wss(_)))
    {
        return false;
    }
    true
}

/// 构建拨号地址候选（M9）：原始地址保留；缺 `/p2p` 段且已知 peerId 时自动补全
/// 候选。
///
/// M9 改造：
/// - **去重**：同一 (ip, port, transport) 只出一个目标（含原始地址与 `/p2p/<id>`
///   补全形态互认，避免同一端点 ×2）；
/// - **排序**：记分卡证据优先（success/valid），零分/同级按静态优先级
///   （IPv6 公网 tcp > IPv6 公网 ws > IPv4 tcp > IPv4 ws > loopback/link-local）；
/// - **不截断**：全部候选都参与（交由分批并发拨号）。
///
/// `addr_meta` 为该 peer 的地址记分卡（缺省零分）；`self_addrs` 为本机当前监听
/// 地址集合（自过滤：不拨自己的监听地址）。
///
/// 无可用地址时返回 `Err`（TS 抛 'Member node addresses are required for p2p connect'）。
pub fn build_dial_targets(
    node_info: &PeerNodeInfo,
    addr_meta: Option<&std::collections::HashMap<String, crate::p2p::overlay_store::AddrScore>>,
    self_addrs: &std::collections::HashSet<String>,
) -> crate::p2p::Result<Vec<String>> {
    // Android 未启用 ws 传输层，拨了也白等，剔除以避免串行死地址白试
    // （跨网建连时死地址排前会拖出 1-2 分钟延迟）。
    let is_android = cfg!(target_os = "android");
    let addresses: Vec<String> = node_info
        .addresses
        .iter()
        .filter_map(|item| filter_dial_candidate(item, is_android))
        .collect();
    if addresses.is_empty() {
        return Err(crate::p2p::P2pError::Malformed(
            "Member node addresses are required for p2p connect".to_string(),
        ));
    }

    // 去重：同一 (ip, port, transport) 只出一个目标。用剥掉尾 `/p2p/{peerId}`
    // 段的 base 地址作去重键（原始地址与补全形态互认）。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique: Vec<String> = Vec::new();
    for addr in addresses {
        let base = base_dial_addr(&addr);
        if seen.insert(base) {
            unique.push(addr);
        }
    }

    // 排序：记分卡证据优先，零分/同级按静态优先级。同一地址的补全形态紧随
    // 原始形态之后（构造时再展开）。
    let empty_meta = std::collections::HashMap::new();
    let meta = addr_meta.unwrap_or(&empty_meta);
    let mut addresses = unique;
    crate::p2p::overlay_store::sort_by_addr_rank(&mut addresses, meta);

    let target_peer_id = extract_peer_id(node_info);
    let mut targets = Vec::with_capacity(addresses.len() * 2);
    for address in addresses {
        // 自过滤：本机监听地址不拨（多实例同机开发污染源）
        if self_addrs.contains(&address) {
            continue;
        }
        targets.push(address.clone());
        if let Some(peer_id) = &target_peer_id
            && !address.contains("/p2p/")
        {
            let with_peer = format!("{}/p2p/{}", address.trim_end_matches('/'), peer_id);
            targets.push(with_peer);
        }
    }
    if targets.is_empty() {
        return Err(crate::p2p::P2pError::Malformed(
            "Member node addresses are required for p2p connect".to_string(),
        ));
    }
    Ok(targets)
}

/// 剥掉 multiaddr 尾部 `/p2p/{peerId}` 段，得到去重键（原始地址与带 peer 段
/// 形态互认）。与 org_direct::base_addr 同义，集中到拨号目标构造处。
fn base_dial_addr(addr: &str) -> String {
    addr.split("/p2p/").next().unwrap_or(addr).to_string()
}

/// 静态优先级（M9）：IPv6 公网 tcp → IPv6 公网 ws → IPv4 tcp → IPv4 ws →
/// loopback/link-local 垫底。与 [`sort_addresses`] 同族但按 tcp/ws 细分：
/// 记分卡零分/同级时以此定序。数字越大越靠后（`rank` 越低越优先）。
pub fn addr_static_rank(a: &str) -> u8 {
    let Ok(ma) = a.parse::<libp2p::Multiaddr>() else {
        return 10; // 解析失败垫底
    };
    if ma.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return 10;
    }
    let is_ws = ma
        .iter()
        .any(|p| matches!(p, Protocol::Ws(_) | Protocol::Wss(_)));
    let ipv6 = ma.iter().any(|p| matches!(p, Protocol::Ip6(_)));
    // 先判 ip 族，再判 tcp/ws；loopback/link-local 一律垫底（不管协议）
    if let Some(p) = ma.iter().next() {
        let is_loopback_or_linklocal = match p {
            Protocol::Ip4(ip) => ip.is_loopback(),
            Protocol::Ip6(ip) => {
                ip.is_loopback() || {
                    let seg = ip.segments();
                    seg[0] >= 0xfe80 && seg[0] <= 0xfebf
                }
            }
            _ => false,
        };
        if is_loopback_or_linklocal {
            return 5;
        }
    }
    match (ipv6, is_ws) {
        (true, false) => 0,  // IPv6 公网 tcp
        (true, true) => 1,   // IPv6 公网 ws
        (false, false) => 2, // IPv4 tcp
        (false, true) => 3,  // IPv4 ws
    }
}

/// 拨号地址排序：公网 IPv6 > 私网/回环 IPv6 > 公网 IPv4 > 私网 IPv4 >
/// 回环 IPv4 > 电路中继（peer-rediscovery §4.6.3，叠加死地址降权）。
///
/// 国内移动网络 IPv6 是移动端之间唯一直连可能（IPv4 双 CGNAT 入站不可达），
/// 且 IPv6 打洞无需猜端口；因此拨号时 IPv6 直连排最前，电路中继垫底。
///
/// 回环（127.0.0.1/::1）与 IPv4 私网段（10/8、172.16/12、192.168/16）不剔除
/// ——同机/局域网场景还要用——但在同协议族内降权排到公网地址之后，避免
/// 跨网建连时私网/回环死地址排在前面被串行白试（单地址 4s 超时）。
///
/// 用更细的多级 rank 实现，`sort_by_key` 为稳定排序，同档内保持插入序。
/// Happy Eyeballs 式并发拨号由 libp2p 自带的并发因子承担，本函数只决定
/// 尝试顺序。
pub fn sort_addresses(addrs: Vec<String>) -> Vec<String> {
    fn rank(a: &str) -> u8 {
        let Ok(ma) = a.parse::<libp2p::Multiaddr>() else {
            return 10; // 解析失败垫底，由拨号环节报错
        };
        if ma.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
            return 10; // 电路中继兜底
        }
        // 逐级降权：公网 IPv6 → 私网/回环 IPv6 → 公网 IPv4 → 私网 IPv4 →
        // 回环 IPv4。回环/私网只降权不剔除（同机/局域网还要用）。
        if ma.iter().any(|p| matches!(p, Protocol::Ip6(_))) {
            let loopback_or_private = ma.iter().any(|p| match p {
                Protocol::Ip6(ip) => ip.is_loopback() || ip.is_unique_local(),
                _ => false,
            });
            if loopback_or_private {
                1 // 私网/回环 IPv6
            } else {
                0 // 公网 IPv6 直连优先
            }
        } else {
            let mut is_loopback = false;
            let mut is_private = false;
            for p in ma.iter() {
                if let Protocol::Ip4(ip) = p {
                    is_loopback |= ip.is_loopback();
                    is_private |= ip.is_private();
                }
            }
            if is_loopback {
                4 // 回环 IPv4（127.0.0.1 不属于私网段）
            } else if is_private {
                3 // 私网 IPv4
            } else {
                2 // 公网 IPv4
            }
        }
    }
    let mut addrs = addrs;
    addrs.sort_by_key(|a| rank(a));
    addrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_circuit_dst_peer_appends_missing_dst() {
        let dst = "12D3KooWPDrpSKELPnUBhn29pY2K7mPYctLY5zHx2XcbcZBtLSsq";
        // 缺目的段的电路地址：补上 /p2p/<dst>
        assert_eq!(
            ensure_circuit_dst_peer(
                "/ip6/2408::1/tcp/15003/p2p/12D3KooWEKRbKxNV65Fsd7G68SPckn7GHQaXcvF3qvAQAYdxY1hj/p2p-circuit",
                dst,
            ),
            format!(
                "/ip6/2408::1/tcp/15003/p2p/12D3KooWEKRbKxNV65Fsd7G68SPckn7GHQaXcvF3qvAQAYdxY1hj/p2p-circuit/p2p/{dst}"
            )
        );
        // 已含目的段：原样
        let full = format!(
            "/ip4/1.2.3.4/tcp/15003/p2p/12D3KooWEKRbKxNV65Fsd7G68SPckn7GHQaXcvF3qvAQAYdxY1hj/p2p-circuit/p2p/{dst}"
        );
        assert_eq!(ensure_circuit_dst_peer(&full, dst), full);
        // 非电路地址：原样
        let plain = "/ip4/1.2.3.4/tcp/15002";
        assert_eq!(ensure_circuit_dst_peer(plain, dst), plain);
        // 非电路但自带 /p2p 尾段：原样
        let with_peer = format!("/ip4/1.2.3.4/tcp/15002/p2p/{dst}");
        assert_eq!(ensure_circuit_dst_peer(&with_peer, dst), with_peer);
        // dst 不可解析：原样（不panic）
        assert_eq!(
            ensure_circuit_dst_peer(
                "/ip4/1.2.3.4/tcp/15003/p2p/12D3KooWEKRbKxNV65Fsd7G68SPckn7GHQaXcvF3qvAQAYdxY1hj/p2p-circuit",
                "not-a-peer",
            ),
            "/ip4/1.2.3.4/tcp/15003/p2p/12D3KooWEKRbKxNV65Fsd7G68SPckn7GHQaXcvF3qvAQAYdxY1hj/p2p-circuit"
        );
    }

    #[test]
    fn address_sort_ipv6_first() {
        let v4 = "/ip4/192.168.1.5/tcp/15002".to_string();
        let v6 = "/ip6/2408:8207:1::1/tcp/15002".to_string();
        // 电路中继：含 /p2p-circuit 段（不含 peerId，纯中继形态）
        let circuit = "/ip4/1.2.3.4/tcp/15002/p2p-circuit".to_string();
        // 公网 IPv6 > 私网 IPv4 > 电路中继
        let sorted = sort_addresses(vec![circuit.clone(), v4.clone(), v6.clone()]);
        assert_eq!(sorted, vec![v6, v4, circuit]);
    }

    #[test]
    fn sort_addresses_preserves_relative_order_within_rank() {
        let a1 = "/ip4/1.1.1.1/tcp/15002".to_string();
        let a2 = "/ip4/2.2.2.2/tcp/15002".to_string();
        // 同为公网 IPv4，保持原顺序
        let sorted = sort_addresses(vec![a1.clone(), a2.clone()]);
        assert_eq!(sorted, vec![a1, a2]);
    }

    #[test]
    fn sort_public_v6_before_private_v4_before_loopback_before_circuit() {
        let public_v6 = "/ip6/2408:8207:1::1/tcp/15002".to_string();
        let private_v4 = "/ip4/192.168.1.5/tcp/15002".to_string();
        let loopback_v4 = "/ip4/127.0.0.1/tcp/15002".to_string();
        let circuit = "/ip4/1.2.3.4/tcp/15002/p2p-circuit".to_string();
        let sorted = sort_addresses(vec![
            circuit.clone(),
            loopback_v4.clone(),
            private_v4.clone(),
            public_v6.clone(),
        ]);
        assert_eq!(sorted, vec![public_v6, private_v4, loopback_v4, circuit]);
    }

    #[test]
    fn sort_private_and_loopback_ipv6_after_public_ipv6() {
        let public_v6 = "/ip6/2408:8207:1::1/tcp/15002".to_string();
        let private_v6 = "/ip6/fd00::1/tcp/15002".to_string();
        let loopback_v6 = "/ip6/::1/tcp/15002".to_string();
        let sorted = sort_addresses(vec![
            loopback_v6.clone(),
            private_v6.clone(),
            public_v6.clone(),
        ]);
        // 公网 IPv6 排最前；私网与回环 IPv6 同档（rank 1），保持插入序
        assert_eq!(sorted[0], public_v6);
        let mut tail = sorted[1..].to_vec();
        tail.sort();
        assert_eq!(tail, vec![loopback_v6, private_v6]);
    }

    #[test]
    fn filter_ws_dropped_only_on_android() {
        let ws = "/ip4/1.2.3.4/tcp/15002/ws".to_string();
        let plain = "/ip4/1.2.3.4/tcp/15002".to_string();
        // Android：ws 剔除，普通地址保留
        assert_eq!(filter_dial_candidate(&ws, true), None);
        assert_eq!(
            filter_dial_candidate(&plain, true).as_deref(),
            Some("/ip4/1.2.3.4/tcp/15002")
        );
        // 非 Android（PC 桌面端）：ws 保留
        assert_eq!(
            filter_dial_candidate(&ws, false).as_deref(),
            Some("/ip4/1.2.3.4/tcp/15002/ws")
        );
    }

    #[test]
    fn filter_wss_dropped_on_android() {
        let wss = "/ip4/1.2.3.4/tcp/443/wss".to_string();
        assert_eq!(filter_dial_candidate(&wss, true), None);
        assert_eq!(
            filter_dial_candidate(&wss, false).as_deref(),
            Some("/ip4/1.2.3.4/tcp/443/wss")
        );
    }

    #[test]
    fn filter_keeps_loopback_and_private() {
        // 回环/私网保留（同机/局域网场景仍可用），只由排序降权
        let loopback = "/ip4/127.0.0.1/tcp/15002".to_string();
        let private = "/ip4/10.0.0.5/tcp/15002".to_string();
        assert_eq!(
            filter_dial_candidate(&loopback, true).as_deref(),
            Some("/ip4/127.0.0.1/tcp/15002")
        );
        assert_eq!(
            filter_dial_candidate(&private, true).as_deref(),
            Some("/ip4/10.0.0.5/tcp/15002")
        );
    }

    #[test]
    fn filter_drops_wildcard() {
        assert_eq!(filter_dial_candidate("/ip4/0.0.0.0/tcp/15002", true), None);
        assert_eq!(filter_dial_candidate("/ip6/::/tcp/15002", true), None);
    }

    #[test]
    fn filter_kad_drops_self_listen_addr() {
        use std::collections::HashSet;
        // 本机监听地址（identify 被对端回灌本机地址）→ 剔除，防 kad 自拨
        let self_addrs: HashSet<String> =
            ["/ip4/127.0.0.1/tcp/15002", "/ip4/192.168.31.134/tcp/15002"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert!(!filter_kad_addr(
            &"/ip4/127.0.0.1/tcp/15002".parse().unwrap(),
            &self_addrs
        ));
        assert!(!filter_kad_addr(
            &"/ip4/192.168.31.134/tcp/15002".parse().unwrap(),
            &self_addrs
        ));
        // 本机 ws 监听形态同样剔除（桌面自监听 ws 必自撞）
        assert!(!filter_kad_addr(
            &"/ip6/::1/tcp/15002/ws".parse().unwrap(),
            &self_addrs
        ));
    }

    #[test]
    fn filter_kad_drops_wildcard_and_ws() {
        use std::collections::HashSet;
        let empty: HashSet<String> = HashSet::new();
        // 通配不可路由
        assert!(!filter_kad_addr(
            &"/ip4/0.0.0.0/tcp/15002".parse().unwrap(),
            &empty
        ));
        // ws/wss 形态：桌面 kad 不拨 ws（即使非本机地址）
        assert!(!filter_kad_addr(
            &"/ip4/1.2.3.4/tcp/15002/ws".parse().unwrap(),
            &empty
        ));
        assert!(!filter_kad_addr(
            &"/ip4/1.2.3.4/tcp/443/wss".parse().unwrap(),
            &empty
        ));
    }

    #[test]
    fn public_external_keeps_public_v4_and_v6() {
        // 公网 IPv4 保留
        assert!(is_public_external_addr(
            &"/ip4/203.0.113.9/tcp/15002".parse().unwrap()
        ));
        assert!(is_public_external_addr(
            &"/ip4/1.2.3.4/tcp/15002".parse().unwrap()
        ));
        // 公网 IPv6 保留（移动网络直连主要靠它）
        assert!(is_public_external_addr(
            &"/ip6/2408:8207:1::1/tcp/15002".parse().unwrap()
        ));
        // 非 IP 首段（如 DNS / circuit）不误伤保留
        assert!(is_public_external_addr(
            &"/dns/example.com/tcp/15002".parse().unwrap()
        ));
    }

    #[test]
    fn public_external_drops_private_loopback_linklocal_wildcard_cgnat() {
        // 私网 IPv4（10/8、172.16/12、192.168/16）剔
        assert!(!is_public_external_addr(
            &"/ip4/10.1.2.3/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip4/172.16.5.5/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip4/192.168.31.134/tcp/15002".parse().unwrap()
        ));
        // loopback 剔
        assert!(!is_public_external_addr(
            &"/ip4/127.0.0.1/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip6/::1/tcp/15002".parse().unwrap()
        ));
        // 链路本地（169.254/16）剔
        assert!(!is_public_external_addr(
            &"/ip4/169.254.1.1/tcp/15002".parse().unwrap()
        ));
        // 通配剔
        assert!(!is_public_external_addr(
            &"/ip4/0.0.0.0/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip6/::/tcp/15002".parse().unwrap()
        ));
        // CGNAT 100.64/10 剔（标准库 is_private 不覆盖，手写判定）
        assert!(!is_public_external_addr(
            &"/ip4/100.64.0.1/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip4/100.127.255.254/tcp/15002".parse().unwrap()
        ));
        // 唯一本地 / 链路本地 IPv6 剔
        assert!(!is_public_external_addr(
            &"/ip6/fd00::1/tcp/15002".parse().unwrap()
        ));
        assert!(!is_public_external_addr(
            &"/ip6/fe80::1/tcp/15002".parse().unwrap()
        ));
    }

    #[test]
    fn filter_kad_keeps_legit_peer_tcp_addr() {
        use std::collections::HashSet;
        let self_addrs: HashSet<String> = ["/ip4/127.0.0.1/tcp/15002"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // 合法对端 TCP 地址（非本机、非通配、非 ws）→ 保留
        assert!(filter_kad_addr(
            &"/ip4/203.0.113.9/tcp/15002".parse().unwrap(),
            &self_addrs
        ));
        // 对端 loopback（同机场景，本机未监听该地址）→ 保留
        assert!(filter_kad_addr(
            &"/ip6/::1/tcp/15002".parse().unwrap(),
            &HashSet::new()
        ));
    }

    #[test]
    fn build_targets_dedups_raw_and_p2p_variant() {
        use std::collections::{HashMap, HashSet};
        let info = PeerNodeInfo {
            peer_id: Some("12D3KooWExamplePeer".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/15002".to_string()],
        };
        // 同一地址只出一个目标：raw + /p2p 变体互认去重
        let targets = build_dial_targets(&info, Some(&HashMap::new()), &HashSet::new()).unwrap();
        assert_eq!(targets.len(), 2, "raw + /p2p 变体各一个");
        assert_eq!(targets[0], "/ip4/1.2.3.4/tcp/15002");
        assert_eq!(targets[1], "/ip4/1.2.3.4/tcp/15002/p2p/12D3KooWExamplePeer");
    }

    #[test]
    fn build_targets_excludes_self_listen_addr() {
        use std::collections::{HashMap, HashSet};
        let info = PeerNodeInfo {
            peer_id: Some("12D3KooWExamplePeer".to_string()),
            addresses: vec![
                "/ip4/192.168.1.5/tcp/15002".to_string(), // 本机监听地址
                "/ip4/8.8.8.8/tcp/15002".to_string(),
            ],
        };
        let self_addrs: HashSet<String> = ["/ip4/192.168.1.5/tcp/15002".to_string()]
            .into_iter()
            .collect();
        let targets = build_dial_targets(&info, Some(&HashMap::new()), &self_addrs).unwrap();
        assert!(
            !targets.iter().any(|t| t.contains("192.168.1.5")),
            "本机监听地址被自过滤排除，实际 {targets:?}"
        );
        assert!(targets.iter().any(|t| t.contains("8.8.8.8")));
    }

    #[test]
    fn static_rank_loopback_last() {
        assert_eq!(
            addr_static_rank("/ip4/192.168.31.134/tcp/15002"),
            2,
            "私网 IPv4 tcp"
        );
        assert_eq!(
            addr_static_rank("/ip4/127.0.0.1/tcp/15002"),
            5,
            "loopback 垫底"
        );
        assert_eq!(
            addr_static_rank("/ip6/2408::1/tcp/15002"),
            0,
            "IPv6 公网 tcp"
        );
        assert_eq!(
            addr_static_rank("/ip6/2408::1/tcp/15002/ws"),
            1,
            "IPv6 公网 ws"
        );
        assert_eq!(addr_static_rank("/ip4/1.2.3.4/tcp/15002/ws"), 3, "IPv4 ws");
    }

    #[test]
    fn build_targets_loopback_after_private() {
        use std::collections::{HashMap, HashSet};
        let info = PeerNodeInfo {
            peer_id: Some("peerA".to_string()),
            addresses: vec![
                "/ip4/127.0.0.1/tcp/15002".to_string(),
                "/ip4/192.168.31.134/tcp/15002".to_string(),
            ],
        };
        let targets = build_dial_targets(&info, Some(&HashMap::new()), &HashSet::new()).unwrap();
        assert_eq!(
            targets[0], "/ip4/192.168.31.134/tcp/15002",
            "私网 IPv4 tcp 在 loopback 之前"
        );
    }

    #[test]
    fn build_targets_sorted_by_scorecard() {
        use crate::p2p::overlay_store::AddrScore;
        use std::collections::{HashMap, HashSet};
        // 记分卡优先：高证据地址排最前（即便 IPv4 tcp 证据更强也优先于零分 IPv6）
        let info = PeerNodeInfo {
            peer_id: None,
            addresses: vec![
                "/ip6/2408:8207:1::1/tcp/15002".to_string(), // 零分但静态高
                "/ip4/1.2.3.4/tcp/15002".to_string(),        // success 证据
            ],
        };
        let mut meta = HashMap::new();
        meta.insert(
            "/ip4/1.2.3.4/tcp/15002".to_string(),
            AddrScore {
                success_count: 3,
                last_success_at: 900,
                ..Default::default()
            },
        );
        let targets = build_dial_targets(&info, Some(&meta), &HashSet::new()).unwrap();
        assert_eq!(
            targets[0], "/ip4/1.2.3.4/tcp/15002",
            "success 证据优先于零分 IPv6"
        );
    }
}
