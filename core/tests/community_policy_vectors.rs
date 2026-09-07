//! golden vectors 验收测试（community-affairs C5：策略引擎 B1 纯逻辑模块）。
//!
//! 加载 `../spec/vectors/community.json`，逐 case 断言 `readGate.policyRef` 组：
//! - 求值矩阵（policy §4，fail-closed）：名册三档 × 请求者关系、字段级掩码、
//!   向上开放矩阵（命中/未覆盖/域不符）、engine 不匹配、policyRef 篡改；
//! - 静态分析（policy §5）：规则冲突 Error 与暴露面扩大 Warning 的稳定 code。
//!
//! 生成器：`core/examples/gen_policy_vectors.rs`（C5 自产回填）。
//! 与 C2 消费测试（community_credential_vectors.rs）分文件避让并行冲突。

use serde_json::Value;
use spark_core::policy::{
    PolicyDoc, PresentedCredential, ReadRequest, RequesterContext, analyze, evaluate_read,
    policy_doc_hash,
};

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

fn requester_from(value: &Value) -> RequesterContext {
    RequesterContext {
        is_org_member: value["isOrgMember"].as_bool().unwrap(),
        is_representative: value["isRepresentative"].as_bool().unwrap(),
        credentials: value["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| PresentedCredential {
                cred_type: c["credType"].as_str().unwrap().to_string(),
                subject_domain: c["subjectDomain"].as_str().unwrap().to_string(),
            })
            .collect(),
    }
}

fn request_from<'a>(value: &'a Value) -> ReadRequest<'a> {
    match value["kind"].as_str().unwrap() {
        "roster-row" => ReadRequest::RosterRow {
            row_is_representative: value["rowIsRepresentative"].as_bool().unwrap(),
        },
        "roster-field" => ReadRequest::RosterField {
            field: value["field"].as_str().unwrap(),
        },
        "collection" => ReadRequest::Collection {
            collection: value["collection"].as_str().unwrap(),
        },
        other => panic!("unknown request kind {other}"),
    }
}

/// 按 case 覆盖项重建策略文档与 policyRef（与生成器同口径）。
fn case_policy<'a>(group: &'a Value, case: &'a Value) -> (PolicyDoc, String) {
    let entry = &group["policies"][case["policy"].as_str().unwrap()];
    let mut doc: PolicyDoc =
        serde_json::from_value(entry["doc"].clone()).expect("deserialize policy doc");
    if let Some(engine) = case["engineOverride"].as_str() {
        doc.engine = engine.to_string();
    }
    let policy_ref = case["policyRefOverride"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| policy_doc_hash(&doc).expect("case policy hash"));
    (doc, policy_ref)
}

fn outcome_name(result: spark_core::policy::Result<spark_core::policy::ReadVerdict>) -> String {
    match result {
        Ok(verdict) => verdict.kind().to_string(),
        Err(err) => err.kind().to_string(),
    }
}

#[test]
fn policy_doc_hashes_match_registration() {
    let group = &vectors()["readGate.policyRef"];
    for (key, entry) in group["policies"].as_object().unwrap() {
        let doc: PolicyDoc = serde_json::from_value(entry["doc"].clone()).unwrap();
        assert_eq!(
            policy_doc_hash(&doc).unwrap(),
            entry["policyDocHash"].as_str().unwrap(),
            "policy {key} hash drift"
        );
    }
}

#[test]
fn readgate_policy_ref_eval_matrix() {
    let group = &vectors()["readGate.policyRef"];
    for case in group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (doc, policy_ref) = case_policy(group, case);
        let requester = requester_from(&case["requester"]);
        let request = request_from(&case["request"]);
        let actual = outcome_name(evaluate_read(&policy_ref, &doc, &requester, &request));
        assert_eq!(actual, case["expect"].as_str().unwrap(), "case {name}");
    }
}

#[test]
fn readgate_policy_ref_static_analysis() {
    let group = &vectors()["readGate.policyRef"];
    for case in group["analysisCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let doc: PolicyDoc = serde_json::from_value(case["doc"].clone()).expect("analysis doc");
        let prev: Option<PolicyDoc> = match &case["prev"] {
            Value::Null => None,
            value => Some(serde_json::from_value(value.clone()).expect("analysis prev")),
        };
        let actual: Vec<&str> = analyze(&doc, prev.as_ref())
            .iter()
            .map(|f| f.code)
            .collect();
        let expect: Vec<&str> = case["expect"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(actual, expect, "analysis case {name}");
    }
}
