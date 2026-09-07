//! `/spark/affairmeta/1.0.0` indexer 查询协议帧（affair-metadata §8）：
//! 轻客户端向启用角色的节点发 `affair-meta-query` 帧，节点回
//! `affair-meta-result` 帧。帧级只做 type/queryId/体积校验与固定键序构造；
//! search 语义校验在 `crate::index::query`（p2p 层不做业务判定）。

use serde_json::{Map, Value};

use crate::index::query::{QUERY_MAX_BYTES, QUERY_TYPE, RESULT_TYPE};

/// 构造 affair-meta-query 请求帧（queryId 由调用方生成；payload 为
/// `crate::index::query` 构造的 search 语义对象）。
pub fn build_affair_meta_request(query_id: &str, payload: &Value) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(QUERY_TYPE.to_string()));
    map.insert("queryId".to_string(), Value::String(query_id.to_string()));
    map.insert("payload".to_string(), payload.clone());
    serde_json::to_string(&Value::Object(map)).expect("request frame serializable")
}

/// 解析请求帧：返回 (queryId, payload)；type/体积不符返回 None。
pub fn parse_affair_meta_request(text: &str) -> Option<(String, Value)> {
    if text.len() > QUERY_MAX_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(Value::as_str) != Some(QUERY_TYPE) {
        return None;
    }
    let query_id = value.get("queryId").and_then(Value::as_str)?;
    if query_id.is_empty() || query_id.len() > 64 {
        return None;
    }
    Some((query_id.to_string(), value.get("payload").cloned()?))
}

/// 构造 affair-meta-result 响应帧（payload = 结果对象或 {"error": reason}）。
///
/// 与 `crate::index::query::build_result` 同一函数：线形一致，且共享响应帧
/// 4KB 上限的尾部截断口径（§7，截断后 complete=false）。
pub fn build_affair_meta_response(query_id: &str, payload: &Value) -> String {
    crate::index::query::build_result(query_id, payload)
}

/// 解析响应帧（请求侧）：返回 payload；type 不符返回 None。
pub fn parse_affair_meta_response(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(Value::as_str) != Some(RESULT_TYPE) {
        return None;
    }
    value.get("payload").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_response_frames_roundtrip() {
        let payload = serde_json::json!({ "kind": "search", "query": "业委会", "limit": 20 });
        let request = build_affair_meta_request("q1", &payload);
        let (query_id, parsed_payload) = parse_affair_meta_request(&request).unwrap();
        assert_eq!(query_id, "q1");
        assert_eq!(parsed_payload, payload);

        let result = serde_json::json!({ "results": [], "complete": true });
        let response = build_affair_meta_response("q1", &result);
        assert_eq!(parse_affair_meta_response(&response).unwrap(), result);
    }

    #[test]
    fn parse_rejects_bad_frames() {
        assert!(parse_affair_meta_request("not json").is_none());
        assert!(
            parse_affair_meta_request(
                &serde_json::json!({ "type": "other", "queryId": "q" }).to_string()
            )
            .is_none()
        );
        assert!(
            parse_affair_meta_response(
                &serde_json::json!({ "type": "affair-meta-query", "queryId": "q" }).to_string()
            )
            .is_none()
        );
    }

    /// 响应帧 4KB 上限（affair-metadata §7）：p2p 应答与 loopback 共用同一
    /// 构造函数，超限尾部截断、complete=false。
    #[test]
    fn response_frame_truncates_over_cap() {
        let payload = serde_json::json!({
            "results": (0..50)
                .map(|i| serde_json::json!({
                    "affairId": format!("{:064x}", i),
                    "summary": "简介".repeat(200),
                }))
                .collect::<Vec<_>>(),
            "complete": true,
        });
        let text = build_affair_meta_response("q-cap", &payload);
        assert!(text.len() <= QUERY_MAX_BYTES);
        let parsed = parse_affair_meta_response(&text).unwrap();
        assert!(parsed["results"].as_array().unwrap().len() < 50);
        assert_eq!(parsed["complete"], false);
    }
}
