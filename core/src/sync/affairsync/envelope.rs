//! affairsync 三信封 body 构造-解析（hello/need/data）+ 记录类型 + 分批切分。
//!
//! 线形权威：wiki/protocol/community/affair-sync.md §3。纯逻辑（不触碰 p2p/
//! 签名），dm 包装（顶层 kind 字段）与投递由调用方完成——与 orgsync 同口径
//! （build/parse 只针对 body）。
//!
//! 与 orgsync-data 的差异：无 `dseq`（affair 域纯 append-only，无墓碑面，
//! affair-sync §1）；hello 额外携带 DAG 头集合（`heads`）。

use serde_json::{Value, json};

use crate::sync::meta::{DocMeta, VersionVector};

/// 单批字节上限（与 orgsync 同口径）。
pub const AFFAIRSYNC_BATCH_BYTES: usize = 256 * 1024;

/// dm kind 字符串（affair-sync §2；p2p 层限流豁免清单同族）。
pub const KIND_AFFAIRSYNC_HELLO: &str = "affairsync-hello";
pub const KIND_AFFAIRSYNC_NEED: &str = "affairsync-need";
pub const KIND_AFFAIRSYNC_DATA: &str = "affairsync-data";

/// affairsync 单条记录（线形同 orgsync-data 记录，无 dseq）。
#[derive(Clone, Debug)]
pub struct AffairsyncRecord {
    /// 存储键（白名单：affair:rec:{affairId} 或 affair:op:{affairId}: 前缀）。
    pub key: String,
    /// 记录值（创世记录或操作条目 JSON）。
    pub value: Value,
    /// 同步 meta（远端 pmeta 持久化形态）。
    pub meta: DocMeta,
}

/// 构造 `affairsync-hello` body：`{ affairId, vv, heads, deviceClass }`。
pub fn build_affairsync_hello(
    affair_id: &str,
    vv: &VersionVector,
    heads: &[String],
    device_class: &str,
) -> Value {
    json!({
        "affairId": affair_id,
        "vv": vv,
        "heads": heads,
        "deviceClass": device_class,
    })
}

/// 解析 affairsync-hello body → (affairId, vv, heads, deviceClass)。
/// 单字段畸形即整体无效（与 orgsync 逐集合容错不同：affair 单集合，无降级粒度）。
pub fn parse_affairsync_hello(
    body: &Value,
) -> Option<(String, VersionVector, Vec<String>, String)> {
    let affair_id = body.get("affairId")?.as_str()?.to_string();
    let vv: VersionVector = serde_json::from_value(body.get("vv")?.clone()).ok()?;
    let heads: Vec<String> = body
        .get("heads")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let device_class = body
        .get("deviceClass")
        .and_then(Value::as_str)
        .unwrap_or("pc")
        .to_string();
    Some((affair_id, vv, heads, device_class))
}

/// 构造 `affairsync-need` body：`{ affairId, knownVv }`。
pub fn build_affairsync_need(affair_id: &str, known_vv: &VersionVector) -> Value {
    json!({
        "affairId": affair_id,
        "knownVv": known_vv,
    })
}

/// 解析 affairsync-need body → (affairId, knownVv)。
pub fn parse_affairsync_need(body: &Value) -> Option<(String, VersionVector)> {
    let affair_id = body.get("affairId")?.as_str()?.to_string();
    let known_vv: VersionVector = serde_json::from_value(body.get("knownVv")?.clone()).ok()?;
    Some((affair_id, known_vv))
}

/// 构造单批 `affairsync-data` body：
/// `{ affairId, records, batchSeq, batchTotal }`。
pub fn build_affairsync_data_batch(
    affair_id: &str,
    records: &[AffairsyncRecord],
    batch_seq: usize,
    batch_total: usize,
) -> Value {
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "key": r.key,
                "value": r.value,
                "meta": serde_json::to_value(&r.meta).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "affairId": affair_id,
        "records": items,
        "batchSeq": batch_seq,
        "batchTotal": batch_total,
    })
}

/// 解析 affairsync-data body → (affairId, records)。
pub fn parse_affairsync_data(body: &Value) -> Option<(String, Vec<AffairsyncRecord>)> {
    let affair_id = body.get("affairId")?.as_str()?.to_string();
    let items = body.get("records")?.as_array()?;
    let mut records = Vec::with_capacity(items.len());
    for item in items {
        let key = item.get("key")?.as_str()?.to_string();
        let value = item.get("value").cloned().unwrap_or(Value::Null);
        let meta: DocMeta = serde_json::from_value(item.get("meta")?.clone()).ok()?;
        records.push(AffairsyncRecord { key, value, meta });
    }
    Some((affair_id, records))
}

/// 按单批字节上限切分记录列表（口径同 split_orgsync_batches）。
pub fn split_affairsync_batches(
    records: Vec<AffairsyncRecord>,
    max_batch_bytes: usize,
) -> Vec<Vec<AffairsyncRecord>> {
    let mut batches: Vec<Vec<AffairsyncRecord>> = Vec::new();
    let mut current: Vec<AffairsyncRecord> = Vec::new();
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

/// 记录 key 白名单校验（affair-sync §3.3 红线）：必须等于创世键或以
/// `affair:op:{affairId}:` 为前缀；`org:` 等异域键恒拒收。
pub fn record_key_in_scope(affair_id: &str, key: &str) -> bool {
    key == crate::affair::affair_record_key(affair_id)
        || key.starts_with(&format!(
            "{}{}:",
            crate::affair::AFFAIR_OP_PREFIX,
            affair_id
        ))
}

/// 出向信封集合：hello 处理结果（need 至多一条 + data 分批）。
#[derive(Clone, Debug, Default)]
pub struct AffairsyncOut {
    /// 本机落后/并发时回给对端的 diff 请求。
    pub need: Option<Value>,
    /// 本机领先/并发时推给对端的数据批。
    pub data: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let mut vv = VersionVector::new();
        vv.insert("node-a".to_string(), 3);
        let heads = vec!["cd".repeat(32)];
        let body = build_affairsync_hello("ab".repeat(32).as_str(), &vv, &heads, "mobile");
        let parsed = parse_affairsync_hello(&body).expect("parse hello");
        assert_eq!(parsed.0, "ab".repeat(32));
        assert_eq!(parsed.1.get("node-a"), Some(&3));
        assert_eq!(parsed.2, heads);
        assert_eq!(parsed.3, "mobile");
    }

    #[test]
    fn need_roundtrip() {
        let mut vv = VersionVector::new();
        vv.insert("n".to_string(), 1);
        let body = build_affairsync_need("ab".repeat(32).as_str(), &vv);
        let parsed = parse_affairsync_need(&body).expect("parse need");
        assert_eq!(parsed.0, "ab".repeat(32));
        assert_eq!(parsed.1, vv);
    }

    #[test]
    fn data_roundtrip_and_split() {
        let affair_id = "ab".repeat(32);
        let records: Vec<AffairsyncRecord> = (0..3)
            .map(|i| AffairsyncRecord {
                key: crate::affair::affair_op_key(&affair_id, &format!("{:02x}", i).repeat(32)),
                value: json!({ "opV": 1, "seq": i }),
                meta: DocMeta {
                    vv: VersionVector::from([("n".to_string(), i as i64 + 1)]),
                    ts: 1000 + i,
                    ..DocMeta::default()
                },
            })
            .collect();
        let bodies: Vec<Value> = split_affairsync_batches(records.clone(), AFFAIRSYNC_BATCH_BYTES)
            .iter()
            .enumerate()
            .map(|(i, batch)| build_affairsync_data_batch(&affair_id, batch, i, 1))
            .collect();
        assert_eq!(bodies.len(), 1);
        let (parsed_affair, parsed_records) =
            parse_affairsync_data(&bodies[0]).expect("parse data");
        assert_eq!(parsed_affair, affair_id);
        assert_eq!(parsed_records.len(), 3);
        assert_eq!(parsed_records[1].value, json!({ "opV": 1, "seq": 1 }));
        assert_eq!(parsed_records[1].meta.ts, 1001);
    }

    #[test]
    fn record_key_whitelist() {
        let affair_id = "ab".repeat(32);
        let op_hash = "cd".repeat(32);
        assert!(record_key_in_scope(
            &affair_id,
            &crate::affair::affair_record_key(&affair_id)
        ));
        assert!(record_key_in_scope(
            &affair_id,
            &crate::affair::affair_op_key(&affair_id, &op_hash)
        ));
        // 红线：org 键 / 异 affair 键 / 其他 affair 数据键域拒收
        assert!(!record_key_in_scope(&affair_id, "org:meta:xxx"));
        assert!(!record_key_in_scope(
            &affair_id,
            &crate::affair::affair_record_key(&"ef".repeat(32))
        ));
        assert!(!record_key_in_scope(
            &affair_id,
            &crate::affair::affair_head_key(&affair_id)
        ));
        assert!(!record_key_in_scope(
            &affair_id,
            &crate::affair::affair_follow_key(&affair_id)
        ));
    }
}
