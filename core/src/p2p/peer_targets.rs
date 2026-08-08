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

/// 构建拨号地址候选：原始地址保留；缺 `/p2p` 段且已知 peerId 时自动补全候选。
///
/// 无可用地址时返回 `Err`（TS 抛 'Member node addresses are required for p2p connect'）。
pub fn build_dial_targets(node_info: &PeerNodeInfo) -> crate::p2p::Result<Vec<String>> {
    let addresses: Vec<String> = node_info
        .addresses
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        // 通配地址（0.0.0.0/::）不可路由，对端拿到也回拨不通，候选中剔除；
        // loopback（127.0.0.1/::1）保留供同机互联。
        .filter(|item| !is_unroutable(item))
        .map(ToString::to_string)
        .collect();
    if addresses.is_empty() {
        return Err(crate::p2p::P2pError::Malformed(
            "Member node addresses are required for p2p connect".to_string(),
        ));
    }

    // IPv6 直连 → IPv4 直连 → 电路中继 排序（peer-rediscovery §4.6.3）
    let addresses = sort_addresses(addresses);
    let target_peer_id = extract_peer_id(node_info);
    let mut targets = Vec::with_capacity(addresses.len() * 2);
    for address in addresses {
        targets.push(address.clone());
        if let Some(peer_id) = &target_peer_id
            && !address.contains("/p2p/")
        {
            targets.push(format!("{}/p2p/{}", address.trim_end_matches('/'), peer_id));
        }
    }
    Ok(targets)
}

/// 地址是否不可路由：首段 IP 为通配（0.0.0.0/::）即不可回拨。
/// 解析失败的地址不算不可路由——原样保留，由后续拨号环节报错。
fn is_unroutable(address: &str) -> bool {
    let Ok(addr) = address.parse::<libp2p::Multiaddr>() else {
        return false;
    };
    match addr.iter().next() {
        Some(Protocol::Ip4(ip)) => ip.is_unspecified(),
        Some(Protocol::Ip6(ip)) => ip.is_unspecified(),
        _ => false,
    }
}

/// 拨号地址排序：IPv6 直连 > IPv4 直连 > 电路中继（peer-rediscovery §4.6.3）。
///
/// 国内移动网络 IPv6 是移动端之间唯一直连可能（IPv4 双 CGNAT 入站不可达），
/// 且 IPv6 打洞无需猜端口；因此拨号时 IPv6 直连排最前，电路中继垫底。
/// Happy Eyeballs 式并发拨号由 libp2p 自带的并发因子承担，本函数只决定
/// 尝试顺序。
pub fn sort_addresses(addrs: Vec<String>) -> Vec<String> {
    fn rank(a: &str) -> u8 {
        let Ok(ma) = a.parse::<libp2p::Multiaddr>() else {
            return 1;
        };
        if ma.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
            2 // 电路中继兜底
        } else if ma.iter().any(|p| matches!(p, Protocol::Ip6(_))) {
            0 // IPv6 直连优先
        } else {
            1 // IPv4 直连次之
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
    fn address_sort_ipv6_first() {
        let v4 = "/ip4/192.168.1.5/tcp/15002".to_string();
        let v6 = "/ip6/2408:8207:1::1/tcp/15002".to_string();
        // 电路中继：含 /p2p-circuit 段（不含 peerId，纯中继形态）
        let circuit = "/ip4/1.2.3.4/tcp/15002/p2p-circuit".to_string();
        // IPv6 直连 > IPv4 直连 > 电路中继
        let sorted = sort_addresses(vec![circuit.clone(), v4.clone(), v6.clone()]);
        assert_eq!(sorted, vec![v6, v4, circuit]);
    }

    #[test]
    fn sort_addresses_preserves_relative_order_within_rank() {
        let a1 = "/ip4/1.1.1.1/tcp/15002".to_string();
        let a2 = "/ip4/2.2.2.2/tcp/15002".to_string();
        // 同为 IPv4，保持原顺序
        let sorted = sort_addresses(vec![a1.clone(), a2.clone()]);
        assert_eq!(sorted, vec![a1, a2]);
    }
}
