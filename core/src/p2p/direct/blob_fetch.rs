//! `/spark/blob-fetch/1.0.0` 内容面 blob 拉取协议帧（public-topics §七
//! 「持有即做种」的传输协议）：请求按 CID 向 provider 拉取本体，响应携带
//! base64 内容；接收侧经 CID 重算校验后落 content store（校验与落库在
//! `node/blob_fetch.rs`，本文件只管线形）。
//!
//! 帧约定与 direct 模块其余协议一致：单帧 UTF-8 JSON，解析失败返回 None
//! 不抛异常。响应帧体积上限由 codec 层把控（`BLOB_FETCH_FRAME_MAX_LEN`）。

use serde_json::{Map, Value};

use crate::content::Cid;

/// 构造 blob-fetch 请求帧文本（CID 形状由调用方在 API 边界校验）。
pub fn build_blob_fetch_request(cid: &str) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("blob-fetch-request".to_string()),
    );
    map.insert("cid".to_string(), Value::String(cid.to_string()));
    serde_json::to_string(&Value::Object(map)).expect("blob-fetch request is always serializable")
}

/// 解析 blob-fetch 请求帧：类型匹配且 CID 形状合法返回 `Cid`，否则 None。
pub fn parse_blob_fetch_request(text: &str) -> Option<Cid> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "blob-fetch-request" {
        return None;
    }
    Cid::parse(value.get("cid")?.as_str()?).ok()
}

/// 构造成功响应帧：`{"type":"blob-fetch-response","ok":true,"cid":...,"data":base64}`。
pub fn build_blob_fetch_response_ok(cid: &str, data_base64: &str) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("blob-fetch-response".to_string()),
    );
    map.insert("ok".to_string(), Value::Bool(true));
    map.insert("cid".to_string(), Value::String(cid.to_string()));
    map.insert(
        "data".to_string(),
        Value::String(data_base64.to_string()),
    );
    serde_json::to_string(&Value::Object(map)).expect("blob-fetch response is always serializable")
}

/// 构造失败响应帧：`reason` ∈ not-found / invalid-request / rate-limited /
/// leaf / integrity-error（错误文本为机器可读 token，对齐 org-mail 口径）。
pub fn build_blob_fetch_response_err(reason: &str) -> String {
    let mut map = Map::new();
    map.insert(
        "type".to_string(),
        Value::String("blob-fetch-response".to_string()),
    );
    map.insert("ok".to_string(), Value::Bool(false));
    map.insert("reason".to_string(), Value::String(reason.to_string()));
    serde_json::to_string(&Value::Object(map)).expect("blob-fetch response is always serializable")
}

/// blob-fetch 响应解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobFetchResponse {
    /// 成功：声明的 CID 与 base64 本体（哈希校验在接收侧做，解析只管形状）。
    Ok { cid: String, data_base64: String },
    /// 失败：机器可读原因 token。
    Err(String),
}

/// 解析 blob-fetch 响应帧：类型非法/形状缺失返回 None。
pub fn parse_blob_fetch_response(text: &str) -> Option<BlobFetchResponse> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "blob-fetch-response" {
        return None;
    }
    if value.get("ok")?.as_bool()? {
        let cid = value.get("cid")?.as_str()?.to_string();
        let data_base64 = value.get("data")?.as_str()?.to_string();
        Some(BlobFetchResponse::Ok { cid, data_base64 })
    } else {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        Some(BlobFetchResponse::Err(reason))
    }
}
