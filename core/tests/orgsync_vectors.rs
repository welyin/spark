//! orgsync golden vectors 验收测试：加载 `../spec/vectors/orgsync.json`，
//! 对 hello/need/data 三信封 body 断言「构造器输出 == 向量」+ parse 往返。
//!
//! 线形基线（org-orgsync.md §20.3/§20.4）：vv 计数语义（per-node 单调序号，
//! org-vv-fix）不改变信封线形——向量钉住的是字段与形状，不是 vv 数值来源。

use serde_json::Value;
use spark_core::sync::meta::{DocMeta, VersionVector};
use spark_core::sync::orgsync::{
    OrgsyncRecord, build_orgsync_data_batch, build_orgsync_hello, build_orgsync_need,
    parse_orgsync_data, parse_orgsync_hello, parse_orgsync_need,
};

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/orgsync.json");
    let raw = std::fs::read_to_string(path).expect("read orgsync vectors");
    serde_json::from_str(&raw).expect("parse orgsync vectors")
}

#[test]
fn orgsync_hello_vector() {
    let v = vectors();
    let section = &v["orgsyncHello"];
    let input = &section["input"];
    let collections = input["collections"].as_object().unwrap().clone();
    let roles: Vec<String> = input["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect();
    let body = build_orgsync_hello(
        input["orgId"].as_str().unwrap(),
        collections,
        &roles,
        input["deviceClass"].as_str().unwrap(),
    );
    assert_eq!(body, section["expected"], "hello 线形逐字段一致");

    // parse 往返：vv / dlogAck / roles / deviceClass / degraded 缺省 false
    let (org_id, cols, roles, device_class) = parse_orgsync_hello(&body).expect("parse hello");
    assert_eq!(org_id, input["orgId"].as_str().unwrap());
    let (vv, dlog_ack, degraded) = cols
        .get("ai-chat:finance@v1.0.0")
        .expect("collection entry");
    assert_eq!(vv.get("node-a"), Some(&3));
    assert_eq!(*dlog_ack, 2);
    assert!(!degraded, "degraded 缺省 false");
    assert_eq!(roles, vec!["data".to_string()]);
    assert_eq!(device_class, "pc");
}

#[test]
fn orgsync_need_vector() {
    let v = vectors();
    let section = &v["orgsyncNeed"];
    let input = &section["input"];
    let known_vv: VersionVector = serde_json::from_value(input["knownVv"].clone()).unwrap();
    let body = build_orgsync_need(
        input["orgId"].as_str().unwrap(),
        input["collection"].as_str().unwrap(),
        &known_vv,
        input["dlogAck"].as_u64().unwrap(),
    );
    assert_eq!(body, section["expected"], "need 线形逐字段一致");

    let (org_id, collection, known_vv, dlog_ack) = parse_orgsync_need(&body).expect("parse need");
    assert_eq!(org_id, input["orgId"].as_str().unwrap());
    assert_eq!(collection, input["collection"].as_str().unwrap());
    assert_eq!(known_vv.get("node-a"), Some(&3));
    assert_eq!(dlog_ack, 2);
}

/// data 批次：普通记录（nodeId 携带、tombstone 缺省省略、无 dseq 键）+
/// 墓碑记录（value 恒 null、tombstone:true、nodeId 省略、dseq 携带）。
#[test]
fn orgsync_data_vector() {
    let v = vectors();
    let section = &v["orgsyncData"];
    let input = &section["input"];
    let records: Vec<OrgsyncRecord> = input["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| OrgsyncRecord {
            key: r["key"].as_str().unwrap().to_string(),
            value: r["value"].clone(),
            meta: serde_json::from_value::<DocMeta>(r["meta"].clone()).unwrap(),
            dseq: r["dseq"].as_u64(),
        })
        .collect();
    let body = build_orgsync_data_batch(
        input["orgId"].as_str().unwrap(),
        input["collection"].as_str().unwrap(),
        &records,
        input["batchSeq"].as_u64().unwrap() as usize,
        input["batchTotal"].as_u64().unwrap() as usize,
    );
    assert_eq!(body, section["expected"], "data 线形逐字段一致");

    // parse 往返：墓碑记录的 dseq 与 meta 逐字段还原
    let (org_id, collection, parsed) = parse_orgsync_data(&body).expect("parse data");
    assert_eq!(org_id, input["orgId"].as_str().unwrap());
    assert_eq!(collection, input["collection"].as_str().unwrap());
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].key, records[0].key);
    assert_eq!(parsed[0].value, records[0].value);
    assert_eq!(parsed[0].meta, records[0].meta);
    assert_eq!(parsed[0].dseq, None, "普通记录无 dseq");
    assert_eq!(parsed[1].meta.tombstone, Some(true), "墓碑标记");
    assert_eq!(parsed[1].value, Value::Null, "墓碑 value 恒 null");
    assert_eq!(parsed[1].dseq, Some(1), "墓碑携带 dseq");
}
