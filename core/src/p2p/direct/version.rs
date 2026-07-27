//! `/spark/version/1.0.0` 直连协议帧：连接打开后立即写入的版本响应。

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
pub fn build_peer_version_response(
    app_version: &str,
    node_id: &str,
    now_ms: i64,
) -> PeerVersionResponse {
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
