//! 直连协议帧与响应侧纯逻辑（core/spec/p2p-messages.md §4/§6/§7/§8/§9）。
//!
//! 通用约定：四个协议均为"写一帧 JSON → 读一帧 JSON"的 request-response，
//! 应用层无长度前缀、无分隔符，帧边界由流承载保证；解析失败返回 null 不抛异常。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::org::recovery::{RecoveryViewItem, active_recovery_tokens};

use super::constants::{
    PEER_EXCHANGE_MAX, RECOVERY_QUERY_WANT, RECOVERY_TTL,
};
use super::peer_targets::PeerNodeInfo;

// ---------------------------------------------------------------------------
// /spark/version/1.0.0
// ---------------------------------------------------------------------------

/// version 响应帧：连接打开后立即写入；请求方**不写任何请求体**。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerVersionResponse {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub app_version: String,
    pub node_id: String,
    pub timestamp: i64,
}

/// 构造 version 响应帧。
pub fn build_peer_version_response(app_version: &str, node_id: &str, now_ms: i64) -> PeerVersionResponse {
    PeerVersionResponse {
        msg_type: "peer-version".to_string(),
        app_version: app_version.to_string(),
        node_id: node_id.to_string(),
        timestamp: now_ms,
    }
}

/// 解析 version 响应并取 appVersion（trim 后为空视为无版本）。
pub fn parse_peer_version_response(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    let version = value.get("appVersion")?.as_str()?.trim();
    if version.is_empty() {
        return None;
    }
    Some(version.to_string())
}

// ---------------------------------------------------------------------------
// /spark/peer-exchange/1.0.0
// ---------------------------------------------------------------------------

/// 邻居样本条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerExchangeSample {
    pub peer_id: String,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub last_seen_at: i64,
}

/// 归一化 want：缺省/非法 → 上限值；封顶 16。
pub fn normalize_exchange_want(raw: Option<&Value>) -> usize {
    let Some(n) = raw.and_then(Value::as_i64) else {
        return PEER_EXCHANGE_MAX;
    };
    if n <= 0 {
        return PEER_EXCHANGE_MAX;
    }
    (n as usize).min(PEER_EXCHANGE_MAX)
}

/// 构造 peer-exchange 请求帧文本。
pub fn build_exchange_request(want: usize) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("peer-exchange-request".to_string()));
    map.insert("want".to_string(), Value::Number((want.min(PEER_EXCHANGE_MAX) as u64).into()));
    serde_json::to_string(&Value::Object(map)).expect("exchange request is always serializable")
}

/// 构造 peer-exchange 响应帧文本。
pub fn build_exchange_response(ok: bool, peers: &[PeerExchangeSample], reason: Option<&str>) -> String {
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(ok));
    map.insert(
        "type".to_string(),
        Value::String("peer-exchange-response".to_string()),
    );
    map.insert(
        "peers".to_string(),
        serde_json::to_value(peers).expect("samples serialize"),
    );
    if let Some(reason) = reason {
        map.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    serde_json::to_string(&Value::Object(map)).expect("exchange response is always serializable")
}

/// 解析 peer-exchange 响应：ok 且 peers 为数组时返回条目（非法返回 None）。
pub fn parse_exchange_response(text: &str) -> Option<Vec<PeerExchangeSample>> {
    let value: Value = serde_json::from_str(text).ok()?;
    if !value.get("ok")?.as_bool()? {
        return None;
    }
    serde_json::from_value(value.get("peers")?.clone()).ok()
}

/// 请求侧样本过滤：跳过自 peerId 与应答方 peerId、地址滤空截 20。
pub fn filter_incoming_sample(
    sample: &PeerExchangeSample,
    self_peer_id: &str,
    responder_peer_id: &str,
) -> Option<(String, Vec<String>)> {
    if sample.peer_id.is_empty() {
        return None;
    }
    if sample.peer_id == self_peer_id || sample.peer_id == responder_peer_id {
        return None;
    }
    let addresses: Vec<String> = sample
        .addresses
        .iter()
        .filter(|a| !a.is_empty())
        .take(20)
        .cloned()
        .collect();
    if addresses.is_empty() {
        return None;
    }
    Some((sample.peer_id.clone(), addresses))
}

// ---------------------------------------------------------------------------
// /spark/org-recovery/1.0.0
// ---------------------------------------------------------------------------

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
    map.insert("type".to_string(), Value::String("org-recovery-query".to_string()));
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
                PeerNodeInfo { peer_id: Some(peer_id.clone()), addresses: Vec::new() }
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

// ---------------------------------------------------------------------------
// /spark/org-share/1.0.0（org-share / org-pull-list / org-pull-org 三类请求帧）
// ---------------------------------------------------------------------------

/// org-share 直连请求类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrgShareRequestKind {
    OrgShare,
    OrgPullList,
    OrgPullOrg,
}

/// 解析 org-share 直连请求帧：返回 (类别, payload)。
/// 空/非法 JSON 返回 Err（Malformed）；未知 type 返回 Ok(None)（由调用方决定响应文案）。
pub fn parse_org_share_request(text: &str) -> super::Result<Option<(OrgShareRequestKind, Value)>> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| super::P2pError::Malformed("empty or invalid json".to_string()))?;
    let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let kind = match msg_type {
        "org-share" => OrgShareRequestKind::OrgShare,
        "org-pull-list" => OrgShareRequestKind::OrgPullList,
        "org-pull-org" => OrgShareRequestKind::OrgPullOrg,
        _ => return Ok(None),
    };
    Ok(Some((kind, payload)))
}

/// org-share 成功响应帧。
pub fn build_org_share_ack_response(sync_id: Option<&str>, org_id: &str, receiver_root_id: &str) -> String {
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(true));
    if let Some(sync_id) = sync_id {
        map.insert("syncId".to_string(), Value::String(sync_id.to_string()));
    }
    map.insert("orgId".to_string(), Value::String(org_id.to_string()));
    map.insert(
        "receiverRootId".to_string(),
        Value::String(receiver_root_id.to_string()),
    );
    serde_json::to_string(&Value::Object(map)).expect("ack response is always serializable")
}

/// org-share 失败响应帧。
pub fn build_org_share_error_response(reason: &str) -> String {
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(false));
    map.insert("reason".to_string(), Value::String(reason.to_string()));
    serde_json::to_string(&Value::Object(map)).expect("error response is always serializable")
}

/// 解析 org-share 直连响应：ok && syncId 匹配即送达。
pub fn parse_org_share_direct_response(text: &str, expected_sync_id: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    value.get("ok").and_then(Value::as_bool) == Some(true)
        && value.get("syncId").and_then(Value::as_str) == Some(expected_sync_id)
}

/// 构造 org-pull-list 请求帧。
pub fn build_pull_list_request(
    requester_root_id: &str,
    requester_peer_id: Option<&str>,
    node_info_claim: Option<Value>,
) -> String {
    let mut payload = Map::new();
    payload.insert(
        "requesterRootId".to_string(),
        Value::String(requester_root_id.to_string()),
    );
    if let Some(peer_id) = requester_peer_id {
        payload.insert(
            "requesterPeerId".to_string(),
            Value::String(peer_id.to_string()),
        );
    }
    if let Some(claim) = node_info_claim {
        payload.insert("nodeInfoClaim".to_string(), claim);
    }
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("org-pull-list".to_string()));
    map.insert("payload".to_string(), Value::Object(payload));
    serde_json::to_string(&Value::Object(map)).expect("pull-list request is always serializable")
}

/// 构造 org-pull-org 请求帧。
pub fn build_pull_org_request(
    requester_root_id: &str,
    requester_peer_id: Option<&str>,
    org_id: &str,
) -> String {
    let mut payload = Map::new();
    payload.insert(
        "requesterRootId".to_string(),
        Value::String(requester_root_id.to_string()),
    );
    if let Some(peer_id) = requester_peer_id {
        payload.insert(
            "requesterPeerId".to_string(),
            Value::String(peer_id.to_string()),
        );
    }
    payload.insert("orgId".to_string(), Value::String(org_id.to_string()));
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("org-pull-org".to_string()));
    map.insert("payload".to_string(), Value::Object(payload));
    serde_json::to_string(&Value::Object(map)).expect("pull-org request is always serializable")
}

/// 构造 org-share 直连请求帧（payload 为 §3.5 的 org-share payload）。
pub fn build_org_share_request(payload: Value) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("org-share".to_string()));
    map.insert("payload".to_string(), payload);
    serde_json::to_string(&Value::Object(map)).expect("org-share request is always serializable")
}

// ---------------------------------------------------------------------------
// 通用限流器（peer-exchange 60s / org-recovery 30s / node-announce 见 announce.rs）
// ---------------------------------------------------------------------------

/// 同一请求方两次服务的最小间隔限流器。
pub struct MinIntervalRateLimiter {
    min_interval_ms: i64,
    last_served_at: HashMap<String, i64>,
}

impl MinIntervalRateLimiter {
    pub fn new(min_interval_ms: i64) -> Self {
        Self {
            min_interval_ms,
            last_served_at: HashMap::new(),
        }
    }

    /// 命中限流返回 true；未命中则记录本次服务时间。
    pub fn is_rate_limited(&mut self, requester: &str, now_ms: i64) -> bool {
        if let Some(last) = self.last_served_at.get(requester)
            && now_ms - last < self.min_interval_ms
        {
            return true;
        }
        self.last_served_at.insert(requester.to_string(), now_ms);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            PeerNodeInfo { peer_id: Some("p1".into()), addresses: vec!["/a".into()] },
            PeerNodeInfo { peer_id: Some("p1".into()), addresses: vec!["/b".into(), "/a".into()] },
            PeerNodeInfo { peer_id: None, addresses: vec!["/c".into()] },
        ];
        let merged = dedupe_recovery_peers(peers, 8);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].addresses, vec!["/a".to_string(), "/b".to_string()]);
    }

    #[test]
    fn recovery_view_match() {
        let view = vec![RecoveryViewItem {
            org_id: "org_0123456789abcdef".to_string(),
            recovery_secret: "ef".repeat(32),
            member_node_infos: vec![crate::org::types::OrganizationNodeInfo {
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
        assert!(parse_org_share_request("{\"type\":\"bogus\"}").unwrap().is_none());
    }

    #[test]
    fn org_share_direct_response_matching() {
        let ok = build_org_share_ack_response(Some("sync-1"), "org_x", "receiver");
        assert!(parse_org_share_direct_response(&ok, "sync-1"));
        assert!(!parse_org_share_direct_response(&ok, "sync-2"));
        assert!(!parse_org_share_direct_response(&build_org_share_error_response("not accepted"), "sync-1"));
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
}
