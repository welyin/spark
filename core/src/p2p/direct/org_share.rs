//! `/spark/org-share/1.0.0` 组织分享直连协议帧
//! （org-share / org-pull-list / org-pull-org 三类请求帧）。

use serde_json::{Map, Value};

use crate::p2p::{P2pError, Result};

/// org-share 直连请求类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrgShareRequestKind {
    OrgShare,
    OrgPullList,
    OrgPullOrg,
}

/// 解析 org-share 直连请求帧：返回 (类别, payload)。
/// 空/非法 JSON 返回 Err（Malformed）；未知 type 返回 Ok(None)（由调用方决定响应文案）。
pub fn parse_org_share_request(text: &str) -> Result<Option<(OrgShareRequestKind, Value)>> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_| P2pError::Malformed("empty or invalid json".to_string()))?;
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
pub fn build_org_share_ack_response(
    sync_id: Option<&str>,
    org_id: &str,
    receiver_root_id: &str,
) -> String {
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
    map.insert(
        "type".to_string(),
        Value::String("org-pull-list".to_string()),
    );
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
    map.insert(
        "type".to_string(),
        Value::String("org-pull-org".to_string()),
    );
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
