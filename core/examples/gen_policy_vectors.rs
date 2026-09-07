//! 回填 `code/spec/vectors/community.json` 中 C5（策略引擎 B1）落地的占位
//! case 组（规格登记：policy §7 / read-gate §6）：
//!
//! - `readGate.policyRef`：policyRef 求值矩阵（名册三档 × 请求者关系、字段
//!   掩码、向上开放矩阵命中/未覆盖/域不符、engine 不匹配、policyRef 篡改）
//!   与静态分析冲突/扩大告警码（policy §5）。
//!
//! 本生成器只对该组做 read-modify-write upsert，其他组一字节不动。
//! 属主 orgId 复用 `trustDecl` 组的固定值（跨组一致性）；上级域 orgId 为
//! 生成器内固定 hex 值。
//!
//! 用法：`cargo run --example gen_policy_vectors -- [community.json 路径]`
//! 自检：全部 case 以 core 实现实跑求值/分析比对 expect，任一失败 panic
//! （非零退出）。

use serde_json::{Value, json};
use spark_core::policy::{
    Audience, FieldRule, PolicyDoc, PresentedCredential, ReadRequest, RequesterContext,
    RosterRules, RosterTier, UpwardEntry, analyze, evaluate_read, policy_doc_hash,
};

const UPDATED_AT: i64 = 1_720_000_000_000;
/// 上级共同体域 orgId（固定 hex，与 eval 单测同源口径）。
const UPSTREAM: &str = "org_b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";

fn doc(
    org_id: &str,
    tier: RosterTier,
    fields: Vec<(&str, Audience)>,
    upward: Vec<(&str, &str)>,
) -> PolicyDoc {
    PolicyDoc {
        policy_v: 1,
        engine: "b1".to_string(),
        org_id: org_id.to_string(),
        roster: RosterRules {
            tier,
            fields: fields
                .into_iter()
                .map(|(field, audience)| FieldRule {
                    field: field.to_string(),
                    audience,
                })
                .collect(),
        },
        upward: upward
            .into_iter()
            .map(|(collection, to)| UpwardEntry {
                collection: collection.to_string(),
                to: to.to_string(),
            })
            .collect(),
        updated_at: UPDATED_AT,
        sig_set: None,
    }
}

fn requester(member: bool, rep: bool, cred_domains: &[&str]) -> Value {
    json!({
        "isOrgMember": member,
        "isRepresentative": rep,
        "credentials": cred_domains.iter().map(|d| json!({
            "credType": "household-owner",
            "subjectDomain": d,
        })).collect::<Vec<_>>(),
    })
}

fn roster_row(row_is_representative: bool) -> Value {
    json!({ "kind": "roster-row", "rowIsRepresentative": row_is_representative })
}

fn roster_field(field: &str) -> Value {
    json!({ "kind": "roster-field", "field": field })
}

fn collection(name: &str) -> Value {
    json!({ "kind": "collection", "collection": name })
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../spec/vectors/community.json"
        )
        .to_string()
    });
    let raw = std::fs::read_to_string(&path).expect("read community.json");
    let mut doc_json: Value = serde_json::from_str(&raw).expect("parse community.json");

    // 属主 orgId 与既有组一致（trustDecl 记录）
    let org_id = doc_json["trustDecl"]["expect"]["record"]["orgId"]
        .as_str()
        .expect("orgId")
        .to_string();

    // ── 三档策略文档（求值矩阵的三档基底）──
    let representatives = doc(
        &org_id,
        RosterTier::Representatives,
        vec![
            ("nickname", Audience::Public),
            ("phone", Audience::Representatives),
        ],
        vec![("finance:monthly@v1", UPSTREAM)],
    );
    let public = doc(
        &org_id,
        RosterTier::Public,
        vec![("nickname", Audience::Public)],
        vec![],
    );
    let org_only = doc(&org_id, RosterTier::OrgOnly, vec![], vec![]);
    let policies = json!({
        "representatives": policy_entry(&representatives),
        "public": policy_entry(&public),
        "orgOnly": policy_entry(&org_only),
    });

    // ── 求值 case（evaluate_read 全路径：policyRef 复算 → 结构/引擎 → 规则）──
    let cases = json!([
        { "name": "member-row-baseline", "policy": "representatives",
          "requester": requester(true, false, &[]), "request": roster_row(false), "expect": "allow" },
        { "name": "member-field-undeclared-baseline", "policy": "representatives",
          "requester": requester(true, false, &[]), "request": roster_field("address"), "expect": "allow" },
        { "name": "rep-row-visible-to-rep", "policy": "representatives",
          "requester": requester(false, true, &[]), "request": roster_row(true), "expect": "allow" },
        { "name": "plain-row-hidden-from-rep", "policy": "representatives",
          "requester": requester(false, true, &[]), "request": roster_row(false), "expect": "row-hidden" },
        { "name": "rep-row-visible-to-outsider", "policy": "representatives",
          "requester": requester(false, false, &[]), "request": roster_row(true), "expect": "allow" },
        { "name": "public-tier-row-to-outsider", "policy": "public",
          "requester": requester(false, false, &[]), "request": roster_row(false), "expect": "allow" },
        { "name": "org-only-tier-row-hidden", "policy": "orgOnly",
          "requester": requester(false, true, &[]), "request": roster_row(true), "expect": "row-hidden" },
        { "name": "field-public-to-outsider", "policy": "representatives",
          "requester": requester(false, false, &[]), "request": roster_field("nickname"), "expect": "allow" },
        { "name": "field-rep-audience-hidden-from-outsider", "policy": "representatives",
          "requester": requester(false, false, &[]), "request": roster_field("phone"), "expect": "field-hidden" },
        { "name": "field-rep-audience-to-rep", "policy": "representatives",
          "requester": requester(false, true, &[]), "request": roster_field("phone"), "expect": "allow" },
        { "name": "field-undeclared-hidden", "policy": "representatives",
          "requester": requester(false, false, &[]), "request": roster_field("address"), "expect": "field-hidden" },
        { "name": "collection-hit", "policy": "representatives",
          "requester": requester(false, false, &[UPSTREAM]),
          "request": collection("finance:monthly@v1"), "expect": "allow" },
        { "name": "collection-wrong-domain", "policy": "representatives",
          "requester": requester(false, false, &[&org_id]),
          "request": collection("finance:monthly@v1"), "expect": "not-covered" },
        { "name": "collection-no-credential", "policy": "representatives",
          "requester": requester(false, false, &[]),
          "request": collection("finance:monthly@v1"), "expect": "not-covered" },
        { "name": "collection-uncovered", "policy": "representatives",
          "requester": requester(false, false, &[UPSTREAM]),
          "request": collection("finance:annual@v1"), "expect": "not-covered" },
        { "name": "engine-mismatch", "policy": "representatives", "engineOverride": "cedar",
          "requester": requester(true, false, &[]), "request": roster_row(false),
          "expect": "unsupported-engine" },
        { "name": "policyref-tampered", "policy": "representatives", "policyRefOverride": "f".repeat(64),
          "requester": requester(true, false, &[]), "request": roster_row(false),
          "expect": "policy-ref-mismatch" },
    ]);

    // ── 静态分析 case（analyze(doc, prev)，expect 为稳定 code 有序列表）──
    let analysis_cases = json!([
        { "name": "clean-first-version",
          "doc": serde_json::to_value(&representatives).unwrap(), "prev": Value::Null, "expect": [] },
        { "name": "duplicate-field-rule",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::Public,
              vec![("nickname", Audience::Public), ("nickname", Audience::Representatives)], vec![])).unwrap(),
          "prev": Value::Null, "expect": ["duplicate-field-rule"] },
        { "name": "duplicate-upward-entry",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::Public, vec![],
              vec![("finance:monthly@v1", UPSTREAM), ("finance:monthly@v1", UPSTREAM)])).unwrap(),
          "prev": Value::Null, "expect": ["duplicate-upward-entry"] },
        { "name": "field-rule-redundant",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::Public,
              vec![("phone", Audience::OrgMembers)], vec![])).unwrap(),
          "prev": Value::Null, "expect": ["field-rule-redundant"] },
        { "name": "field-rule-shadowed",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::OrgOnly,
              vec![("nickname", Audience::Public)], vec![])).unwrap(),
          "prev": Value::Null, "expect": ["field-rule-shadowed"] },
        { "name": "invalid-structure",
          "doc": serde_json::to_value(&doc("bad-org-id", RosterTier::Public, vec![], vec![])).unwrap(),
          "prev": Value::Null, "expect": ["invalid-structure"] },
        { "name": "unsupported-engine",
          "doc": unsupported_engine_doc(&org_id), "prev": Value::Null, "expect": ["unsupported-engine"] },
        { "name": "roster-tier-raised",
          "doc": serde_json::to_value(&representatives).unwrap(),
          "prev": serde_json::to_value(&doc(&org_id, RosterTier::OrgOnly,
              vec![("nickname", Audience::Public), ("phone", Audience::Representatives)],
              vec![("finance:monthly@v1", UPSTREAM)])).unwrap(),
          "expect": ["roster-tier-raised"] },
        { "name": "field-audience-widened",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::Representatives,
              vec![("nickname", Audience::Public)], vec![])).unwrap(),
          "prev": serde_json::to_value(&doc(&org_id, RosterTier::Representatives,
              vec![("nickname", Audience::Representatives)], vec![])).unwrap(),
          "expect": ["field-audience-widened"] },
        { "name": "field-exposed",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::Representatives,
              vec![("nickname", Audience::Public)], vec![])).unwrap(),
          "prev": serde_json::to_value(&doc(&org_id, RosterTier::Representatives, vec![], vec![])).unwrap(),
          "expect": ["field-exposed"] },
        { "name": "new-upward-entry",
          "doc": serde_json::to_value(&representatives).unwrap(),
          "prev": serde_json::to_value(&doc(&org_id, RosterTier::Representatives,
              vec![("nickname", Audience::Public), ("phone", Audience::Representatives)], vec![])).unwrap(),
          "expect": ["new-upward-entry"] },
        { "name": "narrowing-silent",
          "doc": serde_json::to_value(&org_only).unwrap(),
          "prev": serde_json::to_value(&public).unwrap(), "expect": [] },
        { "name": "multi-error-shadowed-and-duplicate",
          "doc": serde_json::to_value(&doc(&org_id, RosterTier::OrgOnly,
              vec![("nickname", Audience::Public), ("nickname", Audience::Representatives)], vec![])).unwrap(),
          "prev": Value::Null, "expect": ["duplicate-field-rule", "field-rule-shadowed"] },
    ]);

    let group = json!({
        "desc": "policyRef B1 求值矩阵（policy §4，fail-closed）：名册三档 × 请求者关系、字段级掩码、向上开放矩阵（命中/未覆盖/域不符）、engine 不匹配、policyRef 篡改；附静态分析（policy §5）冲突/暴露面扩大告警码。生成器：core/examples/gen_policy_vectors.rs（C5）",
        "policies": policies,
        "cases": cases,
        "analysisCases": analysis_cases,
    });

    // ── 自检：全部 case 以 core 实现实跑比对 expect ──
    self_check(&group);

    // ── upsert 回填（只动 C5 一组）──
    doc_json["readGate.policyRef"] = group;

    let text = serde_json::to_string_pretty(&doc_json).expect("serialize");
    std::fs::write(&path, format!("{text}\n")).expect("write community.json");
    println!("written {path}: +readGate.policyRef");
}

/// 策略文档 + 复算哈希入向量（消费侧断言复算一致）。
fn policy_entry(doc: &PolicyDoc) -> Value {
    json!({
        "doc": serde_json::to_value(doc).expect("serialize policy doc"),
        "policyDocHash": policy_doc_hash(doc).expect("policy doc hash"),
    })
}

/// engine 被覆盖为 "cedar" 的合法结构文档（B2 升级路径的 fail-closed 用例）。
fn unsupported_engine_doc(org_id: &str) -> Value {
    let mut doc = doc(org_id, RosterTier::Public, vec![], vec![]);
    doc.engine = "cedar".to_string();
    serde_json::to_value(&doc).unwrap()
}

/// 请求者 JSON → 求值输入（read-gate §4 第 1–4 步通过后的可信摘要）。
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

/// 请求 JSON → ReadRequest。
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

/// 按 case 覆盖项重建策略文档与 policyRef。
fn case_policy<'a>(group: &'a Value, case: &'a Value) -> (PolicyDoc, String) {
    let entry = &group["policies"][case["policy"].as_str().unwrap()];
    let mut doc: PolicyDoc =
        serde_json::from_value(entry["doc"].clone()).expect("deserialize policy doc");
    if let Some(engine) = case["engineOverride"].as_str() {
        doc.engine = engine.to_string();
    }
    // 覆盖后的文档按自身复算哈希（保持文档自认证）；显式篡改覆盖优先
    let policy_ref = case["policyRefOverride"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| policy_doc_hash(&doc).expect("case policy hash"));
    (doc, policy_ref)
}

/// 求值结论/错误 → 稳定名（vectors `expect` 口径）。
fn outcome_name(result: spark_core::policy::Result<spark_core::policy::ReadVerdict>) -> String {
    match result {
        Ok(verdict) => verdict.kind().to_string(),
        Err(err) => err.kind().to_string(),
    }
}

fn self_check(group: &Value) {
    // 求值逐 case
    for case in group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (doc, policy_ref) = case_policy(group, case);
        let requester = requester_from(&case["requester"]);
        let request = request_from(&case["request"]);
        let actual = outcome_name(evaluate_read(&policy_ref, &doc, &requester, &request));
        assert_eq!(actual, case["expect"].as_str().unwrap(), "eval case {name}");
    }
    // 分析逐 case
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
    // 既有组交叉自检：policies 复算哈希与登记值一致（双保险，写盘前拦截漂移）
    for (key, entry) in group["policies"].as_object().unwrap() {
        let doc: PolicyDoc = serde_json::from_value(entry["doc"].clone()).unwrap();
        assert_eq!(
            policy_doc_hash(&doc).unwrap(),
            entry["policyDocHash"].as_str().unwrap(),
            "policy hash drift {key}"
        );
    }
}
