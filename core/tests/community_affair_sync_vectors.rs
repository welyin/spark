//! C4 `affairSync` golden vectors 消费测试（wiki/protocol/community/affair-sync.md §9）。
//!
//! 字节级纪律：body 字符串逐字节比对（builder 输出 == 向量钉死值），
//! parse 回读字段级断言，白名单用例逐条执行。

use serde_json::{Value, json};
use spark_core::sync::affairsync::{
    AffairsyncRecord, build_affairsync_data_batch, build_affairsync_hello, build_affairsync_need,
    parse_affairsync_data, parse_affairsync_hello, parse_affairsync_need, record_key_in_scope,
};
use spark_core::sync::meta::{DocMeta, VersionVector};

fn vectors() -> Value {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    ))
    .expect("read community.json");
    serde_json::from_str(&raw).expect("parse community.json")
}

fn fixed_vv() -> VersionVector {
    VersionVector::from([("node-a".to_string(), 3), ("node-b".to_string(), 1)])
}

fn fixed_heads() -> Vec<String> {
    vec!["cd".repeat(32), "ef".repeat(32)]
}

fn fixed_record_meta() -> DocMeta {
    DocMeta {
        vv: VersionVector::from([("node-a".to_string(), 2)]),
        ts: 1_720_000_000_000,
        node_id: Some("node-a".to_string()),
        tombstone: None,
    }
}

#[test]
fn hello_body_byte_exact_and_parse_roundtrip() {
    let expect = &vectors()["affairSync"]["expect"];
    let affair_id = expect["affairId"].as_str().unwrap();
    let body = build_affairsync_hello(affair_id, &fixed_vv(), &fixed_heads(), "pc");
    // 逐字节：builder 输出与向量钉死值一致（键序/字段集变更即红）
    assert_eq!(body.to_string(), expect["hello"]["body"].as_str().unwrap());
    // parse 回读
    let (parsed_id, vv, heads, device_class) = parse_affairsync_hello(&body).expect("parse hello");
    assert_eq!(parsed_id, affair_id);
    assert_eq!(vv, fixed_vv());
    assert_eq!(heads, fixed_heads());
    assert_eq!(device_class, "pc");
    // 向量内嵌字段与 builder 输入一致（生成器自洽约束）
    let pinned: Value = serde_json::from_str(expect["hello"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(pinned["vv"], json!({ "node-a": 3, "node-b": 1 }));
    assert_eq!(pinned["deviceClass"], json!("pc"));
}

#[test]
fn need_body_byte_exact_and_parse_roundtrip() {
    let expect = &vectors()["affairSync"]["expect"];
    let affair_id = expect["affairId"].as_str().unwrap();
    let body = build_affairsync_need(affair_id, &fixed_vv());
    assert_eq!(body.to_string(), expect["need"]["body"].as_str().unwrap());
    let (parsed_id, known_vv) = parse_affairsync_need(&body).expect("parse need");
    assert_eq!(parsed_id, affair_id);
    assert_eq!(known_vv, fixed_vv());
}

#[test]
fn data_body_byte_exact_and_parse_roundtrip() {
    let expect = &vectors()["affairSync"]["expect"];
    let affair_id = expect["affairId"].as_str().unwrap();
    let record = AffairsyncRecord {
        key: expect["data"]["recordKey"].as_str().unwrap().to_string(),
        value: json!({
            "opV": 1, "affairId": affair_id, "prevOpHash": affair_id,
            "opType": "content", "payload": { "kind": "post", "text": "向量" },
            "actor": { "kind": "person", "identity": "11".repeat(32), "publicKey": "AAAA" },
            "declaredAt": 1_720_000_000_000i64,
            "sig": "BBBB"
        }),
        meta: fixed_record_meta(),
    };
    let body = build_affairsync_data_batch(affair_id, &[record], 0, 1);
    assert_eq!(body.to_string(), expect["data"]["body"].as_str().unwrap());
    let (parsed_id, records) = parse_affairsync_data(&body).expect("parse data");
    assert_eq!(parsed_id, affair_id);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, expect["data"]["recordKey"]);
    assert_eq!(records[0].meta, fixed_record_meta());
    // 线形约束：无 dseq（affair 域无墓碑面）
    let pinned: Value = serde_json::from_str(expect["data"]["body"].as_str().unwrap()).unwrap();
    assert!(pinned["records"][0].get("dseq").is_none());
    assert_eq!(pinned["batchTotal"], json!(1));
}

#[test]
fn key_whitelist_cases() {
    let all = vectors();
    let affair_id = all["affairSync"]["expect"]["affairId"]
        .as_str()
        .unwrap()
        .to_string();
    let cases = all["affairSync"]["expect"]["whitelist"]
        .as_array()
        .expect("whitelist cases");
    assert!(!cases.is_empty());
    for case in cases {
        let key = case["key"].as_str().unwrap();
        let expect = case["inScope"].as_bool().unwrap();
        assert_eq!(
            record_key_in_scope(&affair_id, key),
            expect,
            "whitelist case {} failed",
            case["name"].as_str().unwrap()
        );
    }
}
