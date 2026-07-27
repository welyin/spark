//! `/spark/peer-exchange/1.0.0` 邻居交换协议帧与请求侧样本过滤。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::p2p::constants::PEER_EXCHANGE_MAX;

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
    map.insert(
        "type".to_string(),
        Value::String("peer-exchange-request".to_string()),
    );
    map.insert(
        "want".to_string(),
        Value::Number((want.min(PEER_EXCHANGE_MAX) as u64).into()),
    );
    serde_json::to_string(&Value::Object(map)).expect("exchange request is always serializable")
}

/// 构造 peer-exchange 响应帧文本。
pub fn build_exchange_response(
    ok: bool,
    peers: &[PeerExchangeSample],
    reason: Option<&str>,
) -> String {
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
