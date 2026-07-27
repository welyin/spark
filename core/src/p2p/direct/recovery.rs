//! `/spark/org-recovery/1.0.0` 组织恢复查询协议帧、本地视图命中与转发合并。

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::org::recovery::{RecoveryViewItem, active_recovery_tokens};
use crate::p2p::constants::{RECOVERY_QUERY_WANT, RECOVERY_TTL};
use crate::p2p::peer_targets::PeerNodeInfo;

/// org-recovery 请求帧。
#[derive(Clone, Debug)]
pub struct RecoveryQuery {
    pub token: String,
    pub ttl: u32,
    pub want: usize,
}

/// 构造 org-recovery 请求帧文本。
pub fn build_recovery_request(token: &str, ttl: u32, want: usize) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("org-recovery-query".to_string()),
    );
    map.insert("token".to_string(), Value::String(token.to_string()));
    map.insert("ttl".to_string(), Value::Number(u64::from(ttl).into()));
    map.insert("want".to_string(), Value::Number((want as u64).into()));
    serde_json::to_string(&Value::Object(map)).expect("recovery request is always serializable")
}

/// 解析 org-recovery 请求：type/token 校验（token 须 64 hex）。
pub fn parse_recovery_request(text: &str) -> Option<RecoveryQuery> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "org-recovery-query" {
        return None;
    }
    let token = value.get("token")?.as_str()?;
    // 严格对齐 ^[0-9a-f]{64}$
    if !is_hex64(token) {
        return None;
    }
    let ttl = value.get("ttl").and_then(Value::as_u64).unwrap_or(0) as u32;
    let want = normalize_recovery_want(value.get("want"));
    Some(RecoveryQuery {
        token: token.to_string(),
        ttl,
        want,
    })
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// 归一化 want：缺省/非法 → 8；封顶 8。
pub fn normalize_recovery_want(raw: Option<&Value>) -> usize {
    let Some(n) = raw.and_then(Value::as_i64) else {
        return RECOVERY_QUERY_WANT;
    };
    if n <= 0 {
        return RECOVERY_QUERY_WANT;
    }
    (n as usize).min(RECOVERY_QUERY_WANT)
}

/// 构造 org-recovery 响应帧文本。
pub fn build_recovery_response(ok: bool, peers: &[PeerNodeInfo], reason: Option<&str>) -> String {
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(ok));
    map.insert(
        "type".to_string(),
        Value::String("org-recovery-response".to_string()),
    );
    map.insert(
        "peers".to_string(),
        serde_json::to_value(peers).expect("peers serialize"),
    );
    if let Some(reason) = reason {
        map.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    serde_json::to_string(&Value::Object(map)).expect("recovery response is always serializable")
}

/// 解析 org-recovery 响应（请求侧过滤：peerId 或地址须存在，地址滤空截 20）。
pub fn parse_recovery_response(text: &str) -> Option<Vec<PeerNodeInfo>> {
    let value: Value = serde_json::from_str(text).ok()?;
    if !value.get("ok")?.as_bool()? {
        return None;
    }
    let peers: Vec<PeerNodeInfo> = serde_json::from_value(value.get("peers")?.clone()).ok()?;
    Some(
        peers
            .into_iter()
            .filter(|p| p.peer_id.is_some() || !p.addresses.is_empty())
            .map(|mut p| {
                p.addresses = p
                    .addresses
                    .into_iter()
                    .filter(|a| !a.is_empty())
                    .take(20)
                    .collect();
                p
            })
            .filter(|p| !p.addresses.is_empty())
            .collect(),
    )
}

/// ttl 归一化：`min(max(0, ttl), RECOVERY_TTL)`。
pub fn normalize_recovery_ttl(ttl: u32) -> u32 {
    ttl.min(RECOVERY_TTL)
}

/// 本地恢复视图命中判定：token ∈ 任一组织的 activeRecoveryTokens → 返回成员前 want 条。
pub fn match_recovery_view(
    view: &[RecoveryViewItem],
    token: &str,
    want: usize,
    now_ms: i64,
) -> Option<Vec<PeerNodeInfo>> {
    for entry in view {
        let tokens = active_recovery_tokens(&entry.org_id, &entry.recovery_secret, now_ms);
        if !tokens.contains(&token.to_string()) {
            continue;
        }
        return Some(
            entry
                .member_node_infos
                .iter()
                .take(want)
                .map(|info| PeerNodeInfo {
                    peer_id: info.peer_id.clone(),
                    addresses: info.addresses.clone(),
                })
                .collect(),
        );
    }
    None
}

/// 转发结果去重合并：按 peerId 合并地址，匿名条目按序保留，截断到 want。
pub fn dedupe_recovery_peers(peers: Vec<PeerNodeInfo>, want: usize) -> Vec<PeerNodeInfo> {
    let mut by_peer_id: HashMap<String, PeerNodeInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut anonymous: Vec<PeerNodeInfo> = Vec::new();
    for peer in peers {
        if let Some(peer_id) = &peer.peer_id {
            let entry = by_peer_id.entry(peer_id.clone()).or_insert_with(|| {
                order.push(peer_id.clone());
                PeerNodeInfo {
                    peer_id: Some(peer_id.clone()),
                    addresses: Vec::new(),
                }
            });
            for addr in &peer.addresses {
                if !entry.addresses.contains(addr) {
                    entry.addresses.push(addr.clone());
                }
            }
        } else {
            anonymous.push(peer);
        }
    }
    let mut out: Vec<PeerNodeInfo> = order
        .into_iter()
        .filter_map(|pid| by_peer_id.remove(&pid))
        .collect();
    out.extend(anonymous);
    out.truncate(want);
    out
}
