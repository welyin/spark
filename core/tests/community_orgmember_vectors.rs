//! community-affairs C3（组织作为成员）golden vectors 消费测试：
//! 加载 `../spec/vectors/community.json` 中 C3 case 组逐条断言。
//!
//! 向量来源：`code/core/examples/gen_community_orgmember_vectors.rs` 自产回填
//! （cycleCheck / memberKindEnforce 登记于 org-genesis §7，legacyDegraded 登记于
//! org-signature §6）；生成器内已用 core 实现自检一遍，本测试为消费侧独立复算。
//!
//! 规格权威：wiki/protocol/community/org-genesis.md 与 org-signature.md。

use serde_json::Value;
use spark_core::credential::OrgSigSetVerifier;
use spark_core::org::{
    DEGRADED_LEGACY_ORG_ID, DomainType, MemberKind, OrgSigSetVerifyContext, PolicyVersion,
    enforce_member_kind, membership_would_cycle,
};

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

/// cycleCheck 组（org-genesis §3.3）：名册线形（域→成员组织）反查上级域闭包后
/// 实跑 membership_would_cycle，拒绝/放行与 expect 逐 case 一致。
#[test]
fn cycle_check_vectors() {
    let v = vectors();
    let section = &v["cycleCheck"]["expect"];
    let roster = section["roster"].as_object().unwrap();

    // 上级域闭包：成员组织 → 已将其列入公开名册的域列表（名册反查）
    let parents = |org: &str| -> Vec<String> {
        roster
            .iter()
            .filter(|(_, members)| {
                members
                    .as_array()
                    .is_some_and(|a| a.iter().any(|m| m.as_str() == Some(org)))
            })
            .map(|(domain, _)| domain.clone())
            .collect()
    };

    for case in section["cases"].as_array().unwrap() {
        let joiner = case["joiner"].as_str().unwrap();
        let target = case["target"].as_str().unwrap();
        let rejected = membership_would_cycle(joiner, target, &parents);
        assert_eq!(
            rejected,
            case["expect"].as_str().unwrap() == "reject",
            "cycleCheck case {} ({} → {})",
            case["name"].as_str().unwrap(),
            joiner,
            target
        );
    }
}

/// memberKindEnforce 组（org-genesis §3.2 内核硬规则）：域类型 × kind 矩阵，
/// 拒绝 case 的用户可见文案逐字稳定。
#[test]
fn member_kind_enforce_vectors() {
    let v = vectors();
    let section = &v["memberKindEnforce"]["expect"];
    for case in section["cases"].as_array().unwrap() {
        let domain = match case["domain"].as_str().unwrap() {
            "community" => DomainType::Community,
            _ => DomainType::Leaf,
        };
        let kind = match case["kind"].as_str().unwrap() {
            "org" => MemberKind::Org,
            _ => MemberKind::Person,
        };
        let result = enforce_member_kind(domain, kind);
        match case["expect"].as_str().unwrap() {
            "ok" => assert!(result.is_ok(), "case {}", case["name"]),
            "reject" => {
                let err = result.unwrap_err();
                assert_eq!(err.to_string(), case["message"].as_str().unwrap());
            }
            other => panic!("unknown expect {other}"),
        }
    }
}

/// legacyDegraded 组（org-signature §5.1）：线上验证器（OrgSigSetVerifyContext，
/// credential::OrgSigSetVerifier 的线上实现）实跑——legacy orgId 验证通过且
/// 标注 degraded，创世哈希型对照组不降级。锚根为向量固定替身，按已知值放行
/// （与 C2 向量测试同口径）。
#[test]
fn legacy_degraded_vectors() {
    let v = vectors();
    let section = &v["legacyDegraded"]["expect"];
    let genesis: PolicyVersion =
        serde_json::from_value(section["genesis"].clone()).expect("parse genesis record");
    assert_eq!(
        genesis.policy_hash().unwrap(),
        section["policyHash0"].as_str().unwrap()
    );
    let policies = [genesis];
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &|_| true,
        roster_lookup: &|_| None,
    };

    for case in section["cases"].as_array().unwrap() {
        let sig_set = serde_json::from_value(case["sigSet"].clone()).expect("parse sigSet");
        let expect = &case["expect"];
        let verdict = ctx
            .verify_detailed(&sig_set)
            .unwrap_or_else(|e| panic!("case {} rejected: {e}", case["name"]));
        assert!(expect["verifies"].as_bool().unwrap());
        if expect["degraded"].is_null() {
            assert!(!verdict.degraded, "case {} must not degrade", case["name"]);
        } else {
            assert!(verdict.degraded, "case {} must degrade", case["name"]);
            assert_eq!(
                verdict.degraded_reason,
                Some(DEGRADED_LEGACY_ORG_ID),
                "degraded annotation"
            );
        }
        // trait 接线口径：两种形态的五步链都完整通过（信任裁决归消费方）
        assert!(ctx.verify_org_sig_set(&sig_set));
    }
}
