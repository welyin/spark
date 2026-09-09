//! A17 golden vectors 消费测试（`policy.acceptCredentials` + `joinRequest.merge`
//! 组；生成器：`core/examples/gen_accept_vectors.rs` 自产回填，规格登记
//! policy §9 / org-join §8）。
//! 逐 case 复算比对：准入声明线形哈希自认证、准入面扩大判定真值表、生效
//! 门控与规则匹配、免预录合入双路径验证（有效凭证 / 已注销 / 签发者不受
//! 信任 / 类型不匹配 / 策略缺失 / 未附凭证）。

use serde_json::Value;
use spark_core::credential::{RevocationSnapshot, RevocationView, TrustDecl};
use spark_core::org::join_request::{JoinPath, JoinRequest, adjudicate_join_request};
use spark_core::policy::{
    AcceptPolicyRecord, accept_policy_admits, accept_policy_hash, accept_policy_widening,
    effective_accept_policy, validate_accept_policy,
};

fn vectors() -> Value {
    let raw = include_str!("../../spec/vectors/community.json");
    let doc: Value = serde_json::from_str(raw).expect("parse community.json");
    doc.clone()
}

fn case_record(group: &Value, key: &str) -> AcceptPolicyRecord {
    serde_json::from_value(group["records"][key].clone()).expect("deserialize case record")
}

// ── policy.acceptCredentials：线形哈希自认证 ────────────────────────────

#[test]
fn accept_policy_records_valid_and_hash_stable() {
    let group = &vectors()["policy.acceptCredentials"];
    for (key, expect) in group["expect"]["acceptPolicyHash"]
        .as_object()
        .expect("hash map")
    {
        let record = case_record(group, key);
        validate_accept_policy(&record).unwrap_or_else(|e| panic!("{key} invalid: {e}"));
        assert_eq!(
            accept_policy_hash(&record).expect("rehash"),
            expect.as_str().unwrap(),
            "acceptPolicyHash drift {key}（线形/canonical 口径回归）"
        );
    }
}

#[test]
fn accept_policy_widening_truth_table() {
    let group = &vectors()["policy.acceptCredentials"];
    for case in group["wideningCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let prev = case["prev"].as_str().map(|k| case_record(group, k));
        let next = case_record(group, case["next"].as_str().unwrap());
        assert_eq!(
            accept_policy_widening(prev.as_ref(), &next),
            case["expect"].as_bool().unwrap(),
            "widening case {name}"
        );
    }
}

#[test]
fn accept_policy_effective_gate_and_admits() {
    let group = &vectors()["policy.acceptCredentials"];
    for case in group["effectiveCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let record = case["record"].as_str().map(|k| case_record(group, k));
        let effective = effective_accept_policy(record.as_ref(), case["nowMs"].as_i64().unwrap());
        assert_eq!(
            effective.is_some(),
            case["expectEffective"].as_bool().unwrap(),
            "effective gate {name}（公示延迟窗口内不采信）"
        );
        if let Some(admits) = case.get("expectAdmits") {
            let policy = effective.expect("effective record");
            for probe in admits.as_array().unwrap() {
                assert_eq!(
                    accept_policy_admits(
                        policy,
                        probe["credType"].as_str().unwrap(),
                        probe["subjectDomain"].as_str().unwrap(),
                    ),
                    probe["expect"].as_bool().unwrap(),
                    "admits probe {name} {probe}"
                );
            }
        }
    }
}

// ── joinRequest.merge：免预录合入双路径验证 ─────────────────────────────

#[test]
fn join_request_merge_cases() {
    let group = &vectors()["joinRequest.merge"];
    let org_id = group["context"]["orgId"].as_str().unwrap();
    let now_ms = group["context"]["nowMs"].as_i64().unwrap();
    let policy: AcceptPolicyRecord =
        serde_json::from_value(group["fixtures"]["policy"].clone()).expect("policy");
    let decl: TrustDecl =
        serde_json::from_value(group["fixtures"]["trustDecl"].clone()).expect("trustDecl");
    for case in group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let request: JoinRequest = serde_json::from_value(
            group["fixtures"]["requests"][case["request"].as_str().unwrap()].clone(),
        )
        .expect("request");
        let revocation: RevocationSnapshot = serde_json::from_value(
            group["fixtures"][case["revocation"].as_str().unwrap()].clone(),
        )
        .expect("revocation");
        let view = RevocationView {
            entries: &revocation.entries,
            head: &revocation.head,
        };
        let outcome = adjudicate_join_request(
            org_id,
            &request,
            case["preRegistered"].as_bool().unwrap(),
            case["policy"].as_bool().unwrap().then_some(&policy),
            &[&decl],
            Some(&view),
            now_ms,
        );
        let expect = &case["expect"];
        match (expect.get("path"), expect.get("reject")) {
            (Some(path), None) => {
                let admission = outcome.unwrap_or_else(|e| panic!("case {name} 应受理: {e}"));
                let expect_path = match path.as_str().unwrap() {
                    "claim" => JoinPath::Claim,
                    "credential" => JoinPath::Credential,
                    other => panic!("unknown path {other}"),
                };
                assert_eq!(admission.path, expect_path, "case {name} path");
                if let Some(cred_id) = expect.get("credId") {
                    assert_eq!(
                        admission.cred_id.as_deref(),
                        Some(cred_id.as_str().unwrap()),
                        "case {name} credId"
                    );
                }
            }
            (None, Some(reject)) => {
                let err = outcome.unwrap_err();
                assert_eq!(
                    err.kind(),
                    reject.as_str().unwrap(),
                    "case {name} reject kind"
                );
            }
            _ => panic!("case {name} expect malformed"),
        }
    }
}
