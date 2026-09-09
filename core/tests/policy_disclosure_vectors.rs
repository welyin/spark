//! `policy.disclosure` golden vectors 消费测试（A15 名册开放声明；生成器：
//! `core/examples/gen_disclosure_vectors.rs` 自产回填，规格登记 policy §8）。
//! 逐字节/逐 case 复算比对：线形哈希自认证、暴露面扩大静态分析真值表、
//! 开放声明求值（生效门控 / 版本裁决 / 默认档）。

use serde_json::Value;
use spark_core::policy::{
    DisclosureRecord, FieldRule, disclosure_hash, disclosure_widening, eval_disclosure,
    validate_disclosure,
};

fn vectors() -> Value {
    let raw = include_str!("../../spec/vectors/community.json");
    let doc: Value = serde_json::from_str(raw).expect("parse community.json");
    doc["policy.disclosure"].clone()
}

fn case_record(group: &Value, key: &str) -> DisclosureRecord {
    serde_json::from_value(group["records"][key].clone()).expect("deserialize case record")
}

#[test]
fn disclosure_records_valid_and_hash_stable() {
    let group = vectors();
    for (key, expect) in group["expect"]["disclosureHash"]
        .as_object()
        .expect("hash map")
    {
        let record = case_record(&group, key);
        validate_disclosure(&record).unwrap_or_else(|e| panic!("{key} invalid: {e}"));
        assert_eq!(
            disclosure_hash(&record).expect("rehash"),
            expect.as_str().unwrap(),
            "disclosureHash drift {key}（线形/canonical 口径回归）"
        );
    }
}

#[test]
fn disclosure_widening_truth_table() {
    let group = vectors();
    for case in group["wideningCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let prev = case["prev"].as_str().map(|k| case_record(&group, k));
        let next = case_record(&group, case["next"].as_str().unwrap());
        assert_eq!(
            disclosure_widening(prev.as_ref(), &next),
            case["expect"].as_bool().unwrap(),
            "widening case {name}"
        );
    }
}

#[test]
fn disclosure_eval_cases() {
    let group = vectors();
    for case in group["evalCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let records: Vec<DisclosureRecord> = case["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| case_record(&group, k.as_str().unwrap()))
            .collect();
        let refs: Vec<&DisclosureRecord> = records.iter().collect();
        let view = eval_disclosure(
            &refs,
            case["targetDomain"].as_str().unwrap(),
            case["nowMs"].as_i64().unwrap(),
        );
        let expect = &case["expect"];
        assert_eq!(
            view.tier.to_string(),
            expect["tier"].as_str().unwrap(),
            "eval tier {name}"
        );
        let expect_fields: Vec<FieldRule> = expect["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| serde_json::from_value(f.clone()).unwrap())
            .collect();
        assert_eq!(view.fields, expect_fields, "eval fields {name}");
        let expect_collections: Vec<String> = expect["collections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(view.collections, expect_collections, "eval collections {name}");
    }
}
