//! orgsync 三信封 body 构造-解析（hello/need/data）+ 记录类型 + 分批切分。
//!
//! 纯逻辑（不触碰 p2p/签名），信封装配与投递由 kernel 层完成。

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::sync::meta::VersionVector;

/// orgsync 单条记录（同 PdsyncRecord 线形）。
#[derive(Clone, Debug)]
pub struct OrgsyncRecord {
    pub key: String,
    pub value: Value,
    pub meta: crate::sync::meta::DocMeta,
    /// 删除日志序号（仅墓碑记录携带）。
    pub dseq: Option<u64>,
}

/// 构造 `orgsync-hello` body：
/// `{ orgId, collections: { name@version: { vv, dlogAck } }, roles, deviceClass }`
pub fn build_orgsync_hello(
    org_id: &str,
    collections: Map<String, Value>,
    roles: &[String],
    device_class: &str,
) -> Value {
    json!({
        "orgId": org_id,
        "collections": collections,
        "roles": roles,
        "deviceClass": device_class,
    })
}

/// 解析 orgsync-hello body → (orgId, collections map, roles, deviceClass)。
///
/// collections 值 = `(vv, dlogAck, degraded)`——F4 保留对端 hello 摘要中的
/// `degraded` 标注（数据账号对某 filtered 集合插件未运行 → 只存不服务降级），
/// 成员侧据此避让该数据账号对降级集合的路由。
///
/// F6：单集合 vv 畸形（缺失/非法）时**逐条跳过**该集合，不整体失败——
/// 一个坏集合不应拖垮其余正常集合的 diff（对齐 pdsync `parse_hello_categories`
/// 的容错口径）。
pub fn parse_orgsync_hello(
    body: &Value,
) -> Option<(String, BTreeMap<String, (VersionVector, u64, bool)>, Vec<String>, String)> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let collections_obj = body.get("collections")?.as_object()?;
    let mut collections = BTreeMap::new();
    for (col_name, col_val) in collections_obj {
        let Ok(vv) = serde_json::from_value::<VersionVector>(col_val.get("vv")?.clone()) else {
            log::info!("[ORGSYNC] hello skip malformed vv for {col_name}");
            continue;
        };
        let dlog_ack = col_val.get("dlogAck").and_then(Value::as_u64).unwrap_or(0);
        // F4：保留 degraded 标注（缺省 false = 可服务）
        let degraded = col_val.get("degraded").and_then(Value::as_bool).unwrap_or(false);
        collections.insert(col_name.clone(), (vv, dlog_ack, degraded));
    }
    let roles: Vec<String> = body
        .get("roles")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let device_class = body
        .get("deviceClass")
        .and_then(Value::as_str)
        .unwrap_or("pc")
        .to_string();
    Some((org_id, collections, roles, device_class))
}

/// 构造 `orgsync-need` body：`{ orgId, collection, knownVv, dlogAck }`。
pub fn build_orgsync_need(
    org_id: &str,
    collection: &str,
    known_vv: &VersionVector,
    dlog_ack: u64,
) -> Value {
    json!({
        "orgId": org_id,
        "collection": collection,
        "knownVv": known_vv,
        "dlogAck": dlog_ack,
    })
}

/// 解析 orgsync-need body → (orgId, collection, knownVv, dlogAck)。
pub fn parse_orgsync_need(body: &Value) -> Option<(String, String, VersionVector, u64)> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let collection = body.get("collection")?.as_str()?.to_string();
    let known_vv: VersionVector = serde_json::from_value(body.get("knownVv")?.clone()).ok()?;
    let dlog_ack = body.get("dlogAck").and_then(Value::as_u64).unwrap_or(0);
    Some((org_id, collection, known_vv, dlog_ack))
}

/// 构造单批 `orgsync-data` body：
/// `{ orgId, collection, records, batchSeq, batchTotal }`。
pub fn build_orgsync_data_batch(
    org_id: &str,
    collection: &str,
    records: &[OrgsyncRecord],
    batch_seq: usize,
    batch_total: usize,
) -> Value {
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            let mut item = json!({
                "key": r.key,
                "value": r.value,
                "meta": serde_json::to_value(&r.meta).unwrap_or(Value::Null),
            });
            if let Some(dseq) = r.dseq {
                item["dseq"] = json!(dseq);
            }
            item
        })
        .collect();
    json!({
        "orgId": org_id,
        "collection": collection,
        "records": items,
        "batchSeq": batch_seq,
        "batchTotal": batch_total,
    })
}

/// 解析 orgsync-data body → (orgId, collection, records)。
pub fn parse_orgsync_data(body: &Value) -> Option<(String, String, Vec<OrgsyncRecord>)> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let collection = body.get("collection")?.as_str()?.to_string();
    let items = body.get("records")?.as_array()?;
    let mut records = Vec::with_capacity(items.len());
    for item in items {
        let key = item.get("key")?.as_str()?.to_string();
        let value = item.get("value").cloned().unwrap_or(Value::Null);
        let meta: crate::sync::meta::DocMeta = serde_json::from_value(item.get("meta")?.clone()).ok()?;
        let dseq = item.get("dseq").and_then(Value::as_u64);
        records.push(OrgsyncRecord { key, value, meta, dseq });
    }
    Some((org_id, collection, records))
}

/// 按单批字节上限切分记录列表。
pub fn split_orgsync_batches(
    records: Vec<OrgsyncRecord>,
    max_batch_bytes: usize,
) -> Vec<Vec<OrgsyncRecord>> {
    let mut batches: Vec<Vec<OrgsyncRecord>> = Vec::new();
    let mut current: Vec<OrgsyncRecord> = Vec::new();
    let mut current_bytes = 0usize;
    for record in records {
        let bytes = serde_json::to_string(&record.value)
            .map(|s| s.len() + record.key.len() + 64)
            .unwrap_or(record.key.len() + 128);
        if !current.is_empty() && current_bytes + bytes > max_batch_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(record);
        current_bytes += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}
