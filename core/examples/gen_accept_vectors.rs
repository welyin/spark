//! 回填 `code/spec/vectors/community.json` 的 A17 免预录凭证入册 case 组
//! （规格登记：policy §9 / org-join §8）：
//!
//! - `policy.acceptCredentials`：准入策略声明线形（canonical →
//!   acceptPolicyHash 固定值，sigSet 剔除自认证）；准入面扩大判定
//!   （`accept_policy_widening`）真值表——公示延迟触发条件；生效门控
//!   （公示延迟窗口内不采信）与规则匹配；
//! - `joinRequest.merge`：合入侧双路径验证（`adjudicate_join_request`）——
//!   认领受理 / 有效凭证受理 / 已注销 / 签发者不受信任 / 类型不匹配 /
//!   策略缺失 / 未附凭证各必败。夹具用真实 ed25519 密钥签名（申请人根
//!   0x42、org-access 域 0x43、签发者 0x07、不受信任签发者 0x09）。
//!
//! 本生成器只对上述两组做 read-modify-write upsert，其他组一字节不动
//! （与 gen_disclosure_vectors.rs 同纪律）。
//!
//! 用法：`cargo run --example gen_accept_vectors -- [community.json 路径]`
//! 自检：哈希复算 + widening/effective 逐 case + 全部签名复验 + merge
//! 逐 case 以 core 实现实跑比对 expect，任一失败 panic（非零退出）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use spark_core::credential::{
    Credential, HolderKind, HolderRef, IdentityRef, OrgSigSet, RevocationEntry, RevocationHead,
    RevocationView, RosterAnchor, RosterCommitment, TrustDecl, VerifierGrant, credential_id,
    credential_sign_payload, revocation_entry_hash, revocation_head_payload,
    revocation_sign_payload,
};
use spark_core::org::join_request::{
    JOIN_REQUEST_V, JoinPath, JoinRequest, ORG_JOIN_REQUEST_TYPE, adjudicate_join_request,
    join_request_sign_payload,
};
use spark_core::org::types::{OrganizationAccessKey, access_key_bind_payload};
use spark_core::policy::{
    ACCEPT_POLICY_PUB_PERIOD_MS, ACCEPT_POLICY_V, AcceptCredentialRule, AcceptPolicyRecord,
    accept_policy_admits, accept_policy_hash, accept_policy_widening, effective_accept_policy,
    validate_accept_policy,
};

const NOW: i64 = 1_720_000_000_000;

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn b64_pk(k: &SigningKey) -> String {
    B64.encode(k.verifying_key().to_bytes())
}

fn identity_of(k: &SigningKey) -> String {
    hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
}

fn sign(k: &SigningKey, payload: &str) -> String {
    B64.encode(k.sign(payload.as_bytes()).to_bytes())
}

fn org_id() -> String {
    format!("org_{}", "aa".repeat(32))
}

fn trust_domain() -> String {
    format!("org_{}", "bb".repeat(32))
}

// ── 准入策略声明夹具 ────────────────────────────────────────────────────

fn rule(cred_type: &str) -> AcceptCredentialRule {
    AcceptCredentialRule {
        cred_type: cred_type.to_string(),
        issuer_trust: trust_domain(),
    }
}

/// v1 空规则首声明（不接受任何免预录，即时生效；不扩大）。
fn v1_empty() -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        accept_v: ACCEPT_POLICY_V,
        org_id: org_id(),
        accept_credentials: vec![],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None,
    }
}

/// v1 含规则首声明（扩大方向，effectiveAt = updatedAt + 24h 公示延迟）。
fn v1_rules() -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        accept_credentials: vec![rule("household-owner")],
        effective_at: NOW + ACCEPT_POLICY_PUB_PERIOD_MS,
        ..v1_empty()
    }
}

/// v2 收窄（移除规则，即时生效）。
fn v2_narrowed() -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        version: 2,
        accept_credentials: vec![],
        updated_at: NOW + ACCEPT_POLICY_PUB_PERIOD_MS,
        effective_at: NOW + ACCEPT_POLICY_PUB_PERIOD_MS, // 收窄即时：= updatedAt
        ..v1_rules()
    }
}

/// v2 扩大（新增规则对）。
fn v2_widened() -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        version: 2,
        accept_credentials: vec![rule("household-owner"), rule("resident")],
        updated_at: NOW + ACCEPT_POLICY_PUB_PERIOD_MS,
        effective_at: NOW + 2 * ACCEPT_POLICY_PUB_PERIOD_MS,
        ..v1_rules()
    }
}

// ── 加入声明夹具 ────────────────────────────────────────────────────────

fn applicant() -> SigningKey {
    key(0x42)
}

fn issuer() -> SigningKey {
    key(0x07)
}

/// 申请人根（0x42）自发布的 org-access 域身份（域密钥 0x43 替身——验绑只
/// 查锚定 + 绑定签名，域派生路径不在验证面内）。
fn applicant_access_key() -> OrganizationAccessKey {
    let root = applicant();
    let domain = key(0x43);
    let public_key = b64_pk(&domain);
    OrganizationAccessKey {
        bind_sig: sign(&root, &access_key_bind_payload(&org_id(), &public_key)),
        public_key,
        root_pubkey: Some(b64_pk(&root)),
    }
}

fn build_credential(issuer: &SigningKey, cred_type: &str) -> Credential {
    let holder = applicant();
    let mut cred = Credential {
        cred_v: 1,
        cred_type: cred_type.to_string(),
        issuer: IdentityRef {
            identity: identity_of(issuer),
            public_key: b64_pk(issuer),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: identity_of(&holder),
            public_key: b64_pk(&holder),
        },
        subject_domain: trust_domain(),
        claims: serde_json::Map::new(),
        method: "plugin:test:manual".to_string(),
        link_ref: None,
        issued_at: NOW,
        sig: String::new(),
    };
    cred.sig = sign(issuer, &credential_sign_payload(&cred).expect("payload"));
    cred
}

fn trust_decl(issuer: &SigningKey) -> TrustDecl {
    TrustDecl {
        trust_v: 1,
        org_id: trust_domain(),
        verifiers: vec![VerifierGrant {
            identity: identity_of(issuer),
            public_key: b64_pk(issuer),
            cred_types: vec!["member".to_string()],
            methods: vec!["plugin:test:*".to_string()],
        }],
        effective_from: 0,
        seq: 1,
        updated_at: 0,
        sig_set: OrgSigSet {
            sig_set_v: 1,
            org_id: trust_domain(),
            subject: "00".repeat(32),
            policy_hash: "00".repeat(32),
            roster: RosterCommitment {
                member_set_hash: "00".repeat(32),
                anchor: RosterAnchor {
                    org_id: trust_domain(),
                    anchor_root: "00".repeat(32),
                    ts: 0,
                },
                snapshot: None,
            },
            signed_at: 0,
            signatures: vec![],
        },
    }
}

/// 单条注销的链快照（headSeq 1；空链头形态规格未定义 fail-closed）。
fn revocation_snapshot(issuer: &SigningKey, revoked_cred_id: &str) -> Value {
    let mut entry = RevocationEntry {
        rev_v: 1,
        issuer: identity_of(issuer),
        seq: 1,
        prev_hash: None,
        cred_id: revoked_cred_id.to_string(),
        revoked_at: NOW,
        reason: None,
        sig: String::new(),
    };
    entry.sig = sign(issuer, &revocation_sign_payload(&entry).expect("payload"));
    let mut head = RevocationHead {
        rev_head_v: 1,
        issuer: identity_of(issuer),
        head_seq: 1,
        head_hash: revocation_entry_hash(&entry).expect("entryHash"),
        as_of: NOW,
        sig: String::new(),
    };
    head.sig = sign(issuer, &revocation_head_payload(&head).expect("payload"));
    json!({ "entries": [entry], "head": head })
}

fn build_request(credential: Option<Credential>) -> JoinRequest {
    let root = applicant();
    let mut req = JoinRequest {
        join_v: JOIN_REQUEST_V,
        type_: ORG_JOIN_REQUEST_TYPE.to_string(),
        org_id: org_id(),
        applicant: IdentityRef {
            identity: identity_of(&root),
            public_key: b64_pk(&root),
        },
        access_key: applicant_access_key(),
        credential,
        node_info: None,
        declared_at: NOW,
        sig: String::new(),
    };
    req.sig = sign(&root, &join_request_sign_payload(&req).expect("payload"));
    req
}

// ── 生成 ────────────────────────────────────────────────────────────────

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../spec/vectors/community.json"
        )
        .to_string()
    });
    let raw = std::fs::read_to_string(&path).expect("read community.json");
    let mut doc: Value = serde_json::from_str(&raw).expect("parse community.json");

    let policy_group = build_policy_group();
    let merge_group = build_merge_group();

    // ── 自检：以 core 实现实跑全部 case ──
    self_check_policy(&policy_group);
    self_check_merge(&merge_group);

    doc["policy.acceptCredentials"] = policy_group;
    doc["joinRequest.merge"] = merge_group;
    let text = serde_json::to_string_pretty(&doc).expect("serialize");
    std::fs::write(&path, format!("{text}\n")).expect("write community.json");
    println!("written {path}: +policy.acceptCredentials +joinRequest.merge");
}

fn build_policy_group() -> Value {
    let empty = v1_empty();
    let rules = v1_rules();
    let narrowed = v2_narrowed();
    let widened = v2_widened();
    for (name, r) in [
        ("v1Empty", &empty),
        ("v1Rules", &rules),
        ("v2Narrowed", &narrowed),
        ("v2Widened", &widened),
    ] {
        validate_accept_policy(r).unwrap_or_else(|e| panic!("{name} invalid: {e}"));
    }
    let rec = |r: &AcceptPolicyRecord| serde_json::to_value(r).expect("serialize");
    json!({
        "desc": "准入策略声明（A17 / membership §4.5，policy §9）：线形 canonical → acceptPolicyHash 固定值（sigSet 剔除自认证）；准入面扩大判定（accept_policy_widening）真值表——公示延迟触发条件；生效门控（公示延迟窗口内不采信）与规则匹配。生成器：core/examples/gen_accept_vectors.rs",
        "context": {
            "orgId": org_id(),
            "trustDomain": trust_domain(),
            "nowMs": NOW,
            "pubPeriodMs": ACCEPT_POLICY_PUB_PERIOD_MS,
        },
        "records": {
            "v1Empty": rec(&empty),
            "v1Rules": rec(&rules),
            "v2Narrowed": rec(&narrowed),
            "v2Widened": rec(&widened),
        },
        "expect": {
            "acceptPolicyHash": {
                "v1Empty": accept_policy_hash(&empty).expect("hash v1Empty"),
                "v1Rules": accept_policy_hash(&rules).expect("hash v1Rules"),
                "v2Narrowed": accept_policy_hash(&narrowed).expect("hash v2Narrowed"),
                "v2Widened": accept_policy_hash(&widened).expect("hash v2Widened"),
            },
        },
        "wideningCases": [
            // 首份声明：空规则不扩大；任何非空规则集皆扩大（从无到有即准入面扩大）
            { "name": "first-empty-not-widening", "prev": Value::Null, "next": "v1Empty", "expect": false },
            { "name": "first-rules-widening", "prev": Value::Null, "next": "v1Rules", "expect": true },
            // 持平 / 收窄（移除规则）不扩大
            { "name": "unchanged-not-widening", "prev": "v1Rules", "next": "v1Rules", "expect": false },
            { "name": "narrowed-not-widening", "prev": "v1Rules", "next": "v2Narrowed", "expect": false },
            // 新增规则对 = 扩大（公示延迟触发）
            { "name": "rule-added-widening", "prev": "v1Rules", "next": "v2Widened", "expect": true },
        ],
        "effectiveCases": [
            { "name": "no-record-not-effective", "record": Value::Null, "nowMs": NOW, "expectEffective": false },
            { "name": "pending-not-effective",
              "record": "v1Rules", "nowMs": NOW, "expectEffective": false },
            { "name": "effective-after-pub-period",
              "record": "v1Rules", "nowMs": NOW + ACCEPT_POLICY_PUB_PERIOD_MS,
              "expectEffective": true,
              "expectAdmits": [
                  { "credType": "household-owner", "subjectDomain": trust_domain(), "expect": true },
                  { "credType": "resident", "subjectDomain": trust_domain(), "expect": false },
                  { "credType": "household-owner", "subjectDomain": format!("org_{}", "cc".repeat(32)), "expect": false },
              ] },
        ],
    })
}

fn build_merge_group() -> Value {
    let cred_valid = build_credential(&issuer(), "member");
    let cred_type_mismatch = build_credential(&issuer(), "resident");
    let stranger = key(0x09);
    let cred_untrusted = build_credential(&stranger, "member");

    let req = |c: Option<Credential>| serde_json::to_value(build_request(c)).expect("serialize");
    json!({
        "desc": "免预录合入双路径验证（A17 / org-join §8.2，adjudicate_join_request）：认领受理 / 有效凭证受理 / 已注销 / 签发者不受信任 / 类型不匹配 / 策略缺失 / 未附凭证各必败。夹具真实 ed25519 签名：申请人根 0x42、org-access 域 0x43、签发者 0x07、不受信任签发者 0x09。生成器：core/examples/gen_accept_vectors.rs",
        "context": {
            "orgId": org_id(),
            "trustDomain": trust_domain(),
            "nowMs": NOW,
        },
        "fixtures": {
            "policy": serde_json::to_value(AcceptPolicyRecord {
                accept_credentials: vec![rule("member")], // 与夹具凭证 credType 对齐
                effective_at: NOW, // 已生效（门控后的消费形态）
                ..v1_rules()
            })
            .expect("policy"),
            "trustDecl": serde_json::to_value(trust_decl(&issuer())).expect("trustDecl"),
            "revocationClean": revocation_snapshot(&issuer(), &"ff".repeat(32)),
            "revocationRevoked": revocation_snapshot(
                &issuer(),
                &credential_id(&cred_valid).expect("credId"),
            ),
            "revocationStranger": revocation_snapshot(&stranger, &"ff".repeat(32)),
            "requests": {
                "claim": req(None),
                "valid": req(Some(cred_valid.clone())),
                "typeMismatch": req(Some(cred_type_mismatch)),
                "untrustedIssuer": req(Some(cred_untrusted)),
            },
        },
        "cases": [
            { "name": "claim-accepted",
              "request": "claim", "preRegistered": true, "policy": true, "revocation": "revocationClean",
              "expect": { "path": "claim" } },
            { "name": "credential-accepted",
              "request": "valid", "preRegistered": false, "policy": true, "revocation": "revocationClean",
              "expect": { "path": "credential", "credId": credential_id(&cred_valid).expect("credId") } },
            { "name": "revoked-rejected",
              "request": "valid", "preRegistered": false, "policy": true, "revocation": "revocationRevoked",
              "expect": { "reject": "revoked" } },
            { "name": "issuer-not-trusted-rejected",
              "request": "untrustedIssuer", "preRegistered": false, "policy": true, "revocation": "revocationStranger",
              "expect": { "reject": "issuer-not-trusted" } },
            { "name": "type-mismatch-rejected",
              "request": "typeMismatch", "preRegistered": false, "policy": true, "revocation": "revocationClean",
              "expect": { "reject": "cred-type-not-accepted" } },
            { "name": "policy-missing-rejected",
              "request": "valid", "preRegistered": false, "policy": false, "revocation": "revocationClean",
              "expect": { "reject": "accept-policy-missing" } },
            { "name": "credential-required-rejected",
              "request": "claim", "preRegistered": false, "policy": true, "revocation": "revocationClean",
              "expect": { "reject": "credential-required" } },
        ],
    })
}

// ── 自检 ────────────────────────────────────────────────────────────────

fn case_policy_record(group: &Value, key: &str) -> AcceptPolicyRecord {
    serde_json::from_value(group["records"][key].clone()).expect("deserialize case record")
}

fn self_check_policy(group: &Value) {
    for (key, expect) in group["expect"]["acceptPolicyHash"]
        .as_object()
        .expect("hash map")
    {
        let record = case_policy_record(group, key);
        assert_eq!(
            accept_policy_hash(&record).expect("rehash"),
            expect.as_str().unwrap(),
            "hash drift {key}"
        );
    }
    for case in group["wideningCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let prev = case["prev"].as_str().map(|k| case_policy_record(group, k));
        let next = case_policy_record(group, case["next"].as_str().unwrap());
        assert_eq!(
            accept_policy_widening(prev.as_ref(), &next),
            case["expect"].as_bool().unwrap(),
            "widening case {name}"
        );
    }
    for case in group["effectiveCases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let record = case["record"]
            .as_str()
            .map(|k| case_policy_record(group, k));
        let now_ms = case["nowMs"].as_i64().unwrap();
        let effective = effective_accept_policy(record.as_ref(), now_ms);
        assert_eq!(
            effective.is_some(),
            case["expectEffective"].as_bool().unwrap(),
            "effective case {name}"
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

fn self_check_merge(group: &Value) {
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
        let revocation: spark_core::credential::RevocationSnapshot = serde_json::from_value(
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
                assert_eq!(err.kind(), reject.as_str().unwrap(), "case {name} reject kind");
            }
            _ => panic!("case {name} expect malformed"),
        }
    }
}
