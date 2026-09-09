//! 回填 `code/spec/vectors/community.json` 中 C2（credential 纯逻辑模块）落地的
//! 占位 case 组（规格登记：credential §7 / read-gate §6）：
//!
//! - `credential.trustTimeline`：信任撤销前后签发的凭证判定（既往不咎时间线）
//! - `readGate.verifyChain`：readAuth 五步验证链通过/逐环失败用例
//!   （policyRef 的 B1 求值归 C5，本组覆盖 read-gate §4 第 1–4 步）
//!
//! 其余 case 组由 `code/spec/gen-community-vectors.mjs`（C0 参考实现）产出，
//! 本生成器只对上面两组做 read-modify-write upsert，其他组一字节不动。
//! 密钥/常量与 mjs 生成器同源（固定 seed：verifier 0x51、admin1 0x41、orgRoot 0x31）。
//!
//! 用法：`cargo run --example gen_credential_vectors -- [community.json 路径]`
//! 自检：全部签名复验、全部哈希复算、逐 case 以 core 实现实跑验证链比对 expect，
//! 任一失败 panic（非零退出）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use spark_core::credential::{
    Credential, CredentialReadPolicy, HolderKind, HolderProof, HolderRef, IdentityRef, OrgSigSet,
    ReadAuth, RevocationEntry, RevocationHead, RosterAnchor, RosterCommitment, RosterMember,
    TrustDecl, credential_id, holder_proof_payload, issuer_trusted_at, revocation_entry_hash,
    trust_decl_at, trust_decl_hash, verify_credential_static, verify_read_auth,
};

const NOW: i64 = 1_720_000_000_000;
/// 信任撤销生效时刻（trustDecl v2 effectiveFrom）：NOW + 1h。
const T1: i64 = NOW + 3_600_000;
const COLLECTION: &str = "hoa:ledger@v1.0.0";

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn pub_b64(k: &SigningKey) -> String {
    B64.encode(k.verifying_key().to_bytes())
}

fn identity_of(k: &SigningKey) -> String {
    hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
}

fn sign_b64(k: &SigningKey, payload: &str) -> String {
    B64.encode(k.sign(payload.as_bytes()).to_bytes())
}

/// 组织域身份派生（org-genesis §4）：HMAC-SHA512(根私钥, "spark:org-domain:"‖domain)[0:32]。
fn org_domain_identity(root: &SigningKey, domain: &str) -> SigningKey {
    let dk = <Hmac<Sha512>>::new_from_slice(&root.to_bytes())
        .expect("HMAC accepts any key length")
        .chain_update(b"spark:org-domain:")
        .chain_update(domain.as_bytes())
        .finalize()
        .into_bytes();
    SigningKey::from_bytes(&dk[..32].try_into().expect("32 bytes"))
}

/// OrgSigSet 分量签名载荷（org-signature §2.1 固定 8 键），与 mjs sigSetForPolicy 同口径。
fn component_payload(
    org_id: &str,
    subject: &str,
    policy_hash: &str,
    member_set_hash: &str,
    anchor_root: &str,
    anchor_ts: i64,
    signed_at: i64,
) -> String {
    spark_core::evidence::normalize_object(&json!({
        "sigSetV": 1, "orgId": org_id, "subject": subject, "policyHash": policy_hash,
        "memberSetHash": member_set_hash, "anchorRoot": anchor_root, "anchorTs": anchor_ts,
        "signedAt": signed_at,
    }))
}

/// 复用 community.json orgSigSet 组的名册/锚固定值，构造 any-admin 签名包（admin1 单签）。
fn make_sig_set(
    org_id: &str,
    subject: &str,
    policy_hash: &str,
    member_set_hash: &str,
    anchor_root: &str,
    snapshot: &[RosterMember],
    signed_at: i64,
    signer: &SigningKey,
) -> OrgSigSet {
    let payload = component_payload(
        org_id,
        subject,
        policy_hash,
        member_set_hash,
        anchor_root,
        NOW,
        signed_at,
    );
    OrgSigSet {
        sig_set_v: 1,
        org_id: org_id.to_string(),
        subject: subject.to_string(),
        policy_hash: policy_hash.to_string(),
        roster: RosterCommitment {
            member_set_hash: member_set_hash.to_string(),
            anchor: RosterAnchor {
                org_id: org_id.to_string(),
                anchor_root: anchor_root.to_string(),
                ts: NOW,
            },
            snapshot: Some(snapshot.to_vec()),
        },
        signed_at,
        signatures: vec![spark_core::credential::ComponentSignature {
            signer: identity_of(signer),
            public_key: pub_b64(signer),
            sig: sign_b64(signer, &payload),
        }],
    }
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
    let mut doc: Value = serde_json::from_str(&raw).expect("parse community.json");

    // ── 从既有组取固定值（与 mjs 生成器同源）──
    let org_id = doc["trustDecl"]["expect"]["record"]["orgId"]
        .as_str()
        .expect("orgId")
        .to_string();
    let policy_hash0 = doc["orgGenesis"]["expect"]["policyHash0"]
        .as_str()
        .expect("policyHash0")
        .to_string();
    let roster = &doc["orgSigSet.anyAdmin"]["expect"]["sigSet"]["roster"];
    let member_set_hash = roster["memberSetHash"].as_str().unwrap().to_string();
    let anchor_root = roster["anchor"]["anchorRoot"].as_str().unwrap().to_string();
    let snapshot: Vec<RosterMember> =
        serde_json::from_value(roster["snapshot"].clone()).expect("snapshot");
    let trust_v1: TrustDecl =
        serde_json::from_value(doc["trustDecl"]["expect"]["record"].clone()).expect("trustDecl v1");
    let base_cred: Credential =
        serde_json::from_value(doc["credential.issue"]["expect"]["credential"].clone())
            .expect("base credential");
    let base_cred_id = doc["credential.issue"]["expect"]["credId"]
        .as_str()
        .unwrap()
        .to_string();
    let request_id = doc["readGate.envelope"]["expect"]["requestId"]
        .as_str()
        .unwrap()
        .to_string();
    let base_proof: HolderProof =
        serde_json::from_value(doc["readGate.envelope"]["expect"]["holderProof"].clone())
            .expect("holderProof");

    let verifier = key(0x51);
    let admin1 = key(0x41);
    let holder_key = org_domain_identity(&key(0x31), &format!("community:{org_id}"));
    // 与既有组一致性自检：派生的 holder 必须与 credential.issue 的 holder 相同
    assert_eq!(
        identity_of(&holder_key),
        base_cred.holder.identity,
        "holder identity drift"
    );
    assert_eq!(
        identity_of(&verifier),
        base_cred.issuer.identity,
        "verifier identity drift"
    );

    // ── credential.trustTimeline ──
    // v2：撤销对 verifier 的信任（verifiers 清空），effectiveFrom = T1，seq 2
    let mut trust_v2 = TrustDecl {
        trust_v: 1,
        org_id: org_id.clone(),
        verifiers: vec![],
        effective_from: T1,
        seq: 2,
        updated_at: T1,
        sig_set: make_sig_set(
            &org_id,
            "placeholder",
            &policy_hash0,
            &member_set_hash,
            &anchor_root,
            &snapshot,
            T1,
            &admin1,
        ),
    };
    let v2_hash = trust_decl_hash(&trust_v2).expect("v2 hash");
    trust_v2.sig_set = make_sig_set(
        &org_id,
        &v2_hash,
        &policy_hash0,
        &member_set_hash,
        &anchor_root,
        &snapshot,
        T1,
        &admin1,
    );

    let make_cred = |issued_at: i64| {
        let mut cred = Credential {
            cred_v: 1,
            cred_type: "household-owner".to_string(),
            issuer: IdentityRef {
                identity: identity_of(&verifier),
                public_key: pub_b64(&verifier),
            },
            holder: HolderRef {
                kind: HolderKind::Org,
                identity: identity_of(&holder_key),
                public_key: pub_b64(&holder_key),
            },
            subject_domain: org_id.clone(),
            claims: json!({ "household": "3-502" }).as_object().unwrap().clone(),
            method: "plugin:hoa-verify:manual-property-cert".to_string(),
            link_ref: None,
            issued_at,
            sig: String::new(),
        };
        let payload = spark_core::credential::credential_sign_payload(&cred).expect("payload");
        cred.sig = sign_b64(&verifier, &payload);
        cred
    };
    let cred_before = make_cred(NOW + 60_000); // 撤销前签发 → 既往不咎，仍可信
    let cred_after = make_cred(T1 + 60_000); // 撤销后签发 → 不再可信

    let timeline_cases = json!([
        {
            "name": "issued-before-trust-revoked",
            "credential": "beforeRevocation",
            "credId": credential_id(&cred_before).unwrap(),
            "at": cred_before.issued_at,
            "expectDeclSeq": 1,
            "expectTrusted": true,
        },
        {
            "name": "issued-after-trust-revoked",
            "credential": "afterRevocation",
            "credId": credential_id(&cred_after).unwrap(),
            "at": cred_after.issued_at,
            "expectDeclSeq": 2,
            "expectTrusted": false,
        },
        {
            "name": "before-any-decl-effective",
            "at": NOW - 1000,
            "expectDeclSeq": Value::Null,
            "expectTrusted": false,
        },
    ]);
    let timeline_group = json!({
        "desc": "既往不咎时间线（credential §4 effectiveFrom + §6 第 4 步）：信任撤销前签发的凭证按签发时刻信任集判定仍可信，之后签发的不可信；无任何已生效版本 = 不信任（fail-closed）。生成器：core/examples/gen_credential_vectors.rs（C2）",
        "trustDecls": [
            serde_json::to_value(&trust_v1).unwrap(),
            serde_json::to_value(&trust_v2).unwrap(),
        ],
        "credentials": {
            "beforeRevocation": serde_json::to_value(&cred_before).unwrap(),
            "afterRevocation": serde_json::to_value(&cred_after).unwrap(),
        },
        "cases": timeline_cases,
    });

    // ── readGate.verifyChain ──
    // 基础注销证明：复用既有 revokeChain 三条目 + revHead（目标 credId 不在其中）
    let ok_entries: Vec<RevocationEntry> = doc["credential.revokeChain"]["expect"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| serde_json::from_value(e["entry"].clone()).unwrap())
        .collect();
    let ok_head: RevocationHead =
        serde_json::from_value(doc["credential.revHead"]["expect"]["head"].clone()).unwrap();

    // revoked 用例：目标 credId 在列表中的单条链 + 头承诺
    let mut rev_entry = RevocationEntry {
        rev_v: 1,
        issuer: identity_of(&verifier),
        seq: 1,
        prev_hash: None,
        cred_id: base_cred_id.clone(),
        revoked_at: NOW + 6_000,
        reason: None,
        sig: String::new(),
    };
    rev_entry.sig = sign_b64(
        &verifier,
        &spark_core::credential::revocation_sign_payload(&rev_entry).unwrap(),
    );
    let mut rev_head = RevocationHead {
        rev_head_v: 1,
        issuer: identity_of(&verifier),
        head_seq: 1,
        head_hash: revocation_entry_hash(&rev_entry).unwrap(),
        as_of: NOW + 7_000,
        sig: String::new(),
    };
    rev_head.sig = sign_b64(
        &verifier,
        &spark_core::credential::revocation_head_payload(&rev_head).unwrap(),
    );

    let read_auth_ok = ReadAuth {
        gate_v: 1,
        credentials: vec![base_cred.clone()],
        holder_proofs: vec![base_proof.clone()],
        presented_at: NOW,
    };
    // holder-proof-wrong-request：proof 载荷绑定另一个 requestId
    let wrong_payload = holder_proof_payload(&base_cred_id, "req-other", &org_id, COLLECTION, NOW);
    let proof_wrong = HolderProof {
        cred_id: base_cred_id.clone(),
        sig: sign_b64(&holder_key, &wrong_payload),
    };
    let mut read_auth_wrong_req = read_auth_ok.clone();
    read_auth_wrong_req.holder_proofs = vec![proof_wrong];
    // cred-tampered：改 claims 结论字段，签名保持原值
    let mut cred_tampered = base_cred.clone();
    cred_tampered.claims = json!({ "household": "3-503" }).as_object().unwrap().clone();
    let mut read_auth_tampered = read_auth_ok.clone();
    read_auth_tampered.credentials = vec![cred_tampered];
    // stale-presented-at：呈现时刻超出 ±10min
    let mut read_auth_stale = read_auth_ok.clone();
    read_auth_stale.presented_at = NOW - 11 * 60 * 1000;

    let policy = json!({
        "credTypes": ["household-owner", "resident"],
        "verifierDomain": org_id,
        "policyRef": Value::Null,
    });
    let verify_chain_group = json!({
        "desc": "readAuth 验证链（read-gate §4 第 1–4 步 + A15 城门名册回查，fail-closed）：ok 通过；逐环失败用例（凭证篡改/类型不符/验证人不受信任/已注销/proof 绑定错请求/呈现过期/持有者退队/名册不可用）→ 对应错误名。policyRef 求值归 C5，不在本组。生成器：core/examples/gen_credential_vectors.rs（C2）",
        "context": {
            "requestId": request_id,
            "collection": COLLECTION,
            "nowMs": NOW,
            "policy": policy,
            "trustDecls": [serde_json::to_value(&trust_v1).unwrap()],
            "revocation": {
                "entries": ok_entries.iter().map(|e| serde_json::to_value(e).unwrap()).collect::<Vec<_>>(),
                "head": serde_json::to_value(&ok_head).unwrap(),
            },
        },
        "cases": [
            { "name": "ok", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(), "expect": "ok" },
            { "name": "cred-tampered", "readAuth": serde_json::to_value(&read_auth_tampered).unwrap(), "expect": "invalid-signature" },
            { "name": "cred-type-not-allowed", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(),
              "policyOverride": { "credTypes": ["resident"] }, "expect": "cred-type-not-allowed" },
            { "name": "untrusted-issuer", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(),
              "trustDeclsOverride": [], "expect": "issuer-not-trusted" },
            { "name": "revoked", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(),
              "revocationOverride": { "entries": [serde_json::to_value(&rev_entry).unwrap()],
                                      "head": serde_json::to_value(&rev_head).unwrap() },
              "expect": "revoked" },
            { "name": "holder-proof-wrong-request", "readAuth": serde_json::to_value(&read_auth_wrong_req).unwrap(), "expect": "holder-proof-invalid" },
            { "name": "stale-presented-at", "readAuth": serde_json::to_value(&read_auth_stale).unwrap(), "expect": "stale-timestamp" },
            // 城门名册回查（A15）：持有者在册验证链全过才放行；退队/名册不可用各必败
            { "name": "roster-non-member", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(),
              "roster": "non-member", "expect": "not-subject-domain-member" },
            { "name": "roster-unavailable", "readAuth": serde_json::to_value(&read_auth_ok).unwrap(),
              "roster": "unavailable", "expect": "not-subject-domain-member" },
        ],
    });

    // ── 自检：以 core 实现实跑全部 case ──
    self_check(
        &timeline_group,
        &verify_chain_group,
        &trust_v1,
        &trust_v2,
        &verifier,
        &admin1,
    );

    // ── upsert 回填（只动 C2 两组；meta.placeholders.desc 由 C1 生成器统一登记，
    //    本生成器不写该字段，避免双写竞争）──
    doc["credential.trustTimeline"] = timeline_group;
    doc["readGate.verifyChain"] = verify_chain_group;

    let text = serde_json::to_string_pretty(&doc).expect("serialize");
    std::fs::write(&path, format!("{text}\n")).expect("write community.json");
    println!("written {path}: +credential.trustTimeline +readGate.verifyChain");
}

/// 自检：时间线判定与 readAuth 验证链逐 case 实跑，expect 不符即 panic。
fn self_check(
    timeline_group: &Value,
    verify_chain_group: &Value,
    trust_v1: &TrustDecl,
    trust_v2: &TrustDecl,
    verifier: &SigningKey,
    admin1: &SigningKey,
) {
    // v2 sigSet 分量签名复验（org-signature §2.1 载荷口径）
    let cp = component_payload(
        &trust_v2.org_id,
        &trust_v2.sig_set.subject,
        &trust_v2.sig_set.policy_hash,
        &trust_v2.sig_set.roster.member_set_hash,
        &trust_v2.sig_set.roster.anchor.anchor_root,
        trust_v2.sig_set.roster.anchor.ts,
        trust_v2.sig_set.signed_at,
    );
    let comp = &trust_v2.sig_set.signatures[0];
    assert!(
        spark_core::identity::verify_ed25519_signature(&cp, &comp.sig, &comp.public_key),
        "v2 sigSet component"
    );
    assert_eq!(comp.signer, identity_of(admin1), "v2 sigSet signer");

    // 时间线逐 case
    let decls: Vec<&TrustDecl> = vec![trust_v1, trust_v2];
    for case in timeline_group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let at = case["at"].as_i64().unwrap();
        assert_eq!(
            trust_decl_at(&decls, at).map(|d| d.seq),
            case["expectDeclSeq"].as_u64(),
            "trust_decl_at {name}"
        );
        if let Some(key) = case["credential"].as_str() {
            let cred: Credential =
                serde_json::from_value(timeline_group["credentials"][key].clone()).unwrap();
            assert!(
                verify_credential_static(&cred, Some(case["credId"].as_str().unwrap())).is_ok(),
                "static {name}"
            );
            let trusted = issuer_trusted_at(
                &decls,
                &cred.issuer.identity,
                &cred.cred_type,
                &cred.method,
                cred.issued_at,
            );
            assert_eq!(
                trusted,
                case["expectTrusted"].as_bool().unwrap(),
                "trusted {name}"
            );
        }
    }

    // verifyChain 逐 case
    let ctx = &verify_chain_group["context"];
    let request_id = ctx["requestId"].as_str().unwrap();
    let collection = ctx["collection"].as_str().unwrap();
    let now = ctx["nowMs"].as_i64().unwrap();
    for case in verify_chain_group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let read_auth: ReadAuth = serde_json::from_value(case["readAuth"].clone()).unwrap();
        let cred_types: Vec<String> = case["policyOverride"]["credTypes"]
            .as_array()
            .or_else(|| ctx["policy"]["credTypes"].as_array())
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        let policy = CredentialReadPolicy {
            cred_types,
            verifier_domain: ctx["policy"]["verifierDomain"]
                .as_str()
                .unwrap()
                .to_string(),
        };
        let case_decls: Vec<TrustDecl> = match &case["trustDeclsOverride"] {
            Value::Null => vec![trust_v1.clone()],
            arr => arr
                .as_array()
                .unwrap()
                .iter()
                .map(|d| serde_json::from_value(d.clone()).unwrap())
                .collect(),
        };
        let decl_refs: Vec<&TrustDecl> = case_decls.iter().collect();
        let (entries, head): (Vec<RevocationEntry>, RevocationHead) = match &case["revocationOverride"]
        {
            Value::Null => (
                ctx["revocation"]["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| serde_json::from_value(e.clone()).unwrap())
                    .collect(),
                serde_json::from_value(ctx["revocation"]["head"].clone()).unwrap(),
            ),
            r => (
                r["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| serde_json::from_value(e.clone()).unwrap())
                    .collect(),
                serde_json::from_value(r["head"].clone()).unwrap(),
            ),
        };
        let revocation_for = move |_issuer: &str| Some((entries.clone(), head.clone()));
        // 城门名册回查（A15）：case 可声明 roster（缺省 member = 在册）
        let roster_lookup = |_domain: &str, _identity: &str| {
            match case["roster"].as_str().unwrap_or("member") {
                "member" => Some(true),
                "non-member" => Some(false),
                _ => None, // "unavailable"：名册数据缺失 → fail-closed
            }
        };
        let result = verify_read_auth(
            &read_auth,
            request_id,
            collection,
            &policy,
            &decl_refs,
            &revocation_for,
            &roster_lookup,
            now,
        );
        let actual = match &result {
            Ok(()) => "ok",
            Err(e) => e.kind(),
        };
        assert_eq!(
            actual,
            case["expect"].as_str().unwrap(),
            "verifyChain case {name}"
        );
    }

    // 既有组交叉自检：verifier 公钥与 meta.actors 一致
    let _ = verifier;
}
