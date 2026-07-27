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
        .map(ToString::to_string)
        .collect();
    if addresses.is_empty() {
        return Err(crate::p2p::P2pError::Malformed(
            "Member node addresses are required for p2p connect".to_string(),
        ));
    }

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
