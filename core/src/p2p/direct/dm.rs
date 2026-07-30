//! `/spark/dm/1.0.0` dm（direct message）直连协议帧。
//!
//! dm 信封（chat/read/recall/friend-request/friend-accept）由 kernel 层构造与验签，
//! p2p 层只透明搬运 JSON：请求帧 = 信封本体序列化，响应帧 = 宿主应用层应答序列化，
//! 均不解析字段。

use serde_json::Value;

/// 构造 dm 直连请求帧（payload 为 dm 信封 JSON，透明搬运）。
pub fn build_dm_request(payload: &Value) -> String {
    serde_json::to_string(payload).expect("JSON Value serialization is infallible")
}

/// 解析 dm 直连请求帧：合法 JSON 即放行（字段校验在 kernel 层）。
pub fn parse_dm_request(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok().filter(|v| v.is_object())
}

/// 构造 dm 应答侧错误响应帧（宿主拒绝/未实现时）。
pub fn build_dm_error_response(reason: &str) -> String {
    serde_json::json!({"ok": false, "reason": reason}).to_string()
}

/// 解析 dm 直连响应：合法 JSON 对象即视为有应答。
pub fn parse_dm_response(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok().filter(|v| v.is_object())
}

/// 应答侧限流的豁免判定：控制类 kind（read/recall/friend-accept）豁免，
/// 内容型 kind（chat/friend-request/friend-reply）与未知 kind 保持最小
/// 间隔限流。
///
/// 豁免原因：控制类信封由「发消息」动作派生连发（如 chat 紧跟 read 回执），
/// 共享同一限流桶会让第二条必吃 rate-limited 被对端误标 failed。
pub fn dm_kind_is_rate_limit_exempt(kind: Option<&str>) -> bool {
    matches!(kind, Some("read" | "recall" | "friend-accept"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_kinds_are_exempt_chat_and_unknown_are_not() {
        for kind in ["read", "recall", "friend-accept"] {
            assert!(dm_kind_is_rate_limit_exempt(Some(kind)), "{kind} 应豁免");
        }
        for kind in ["chat", "friend-request", "friend-reply", "unknown-kind"] {
            assert!(!dm_kind_is_rate_limit_exempt(Some(kind)), "{kind} 不应豁免");
        }
        assert!(!dm_kind_is_rate_limit_exempt(None), "缺失 kind 不应豁免");
    }
}
