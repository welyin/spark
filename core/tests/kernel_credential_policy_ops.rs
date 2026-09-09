//! kernel 凭证/策略门面集成测试（community-affairs C9）：`credential_list_held` /
//! `credential_present_holder_proof` / `credential_query_verifiers` / `policy_read` /
//! `policy_submit_draft`。tempdir 起真内核直调（仓库 kernel 集成测试惯例），
//! 夹具用真实 ed25519 密钥签名，不 mock。

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use common::*;
use spark_core::credential::{
    Credential, HolderKind, HolderRef, IdentityRef, OrgSigSet, RevocationEntry, RevocationHead,
    RosterAnchor, RosterCommitment, TrustDecl, VerifierGrant, credential_id,
    credential_sign_payload, held_credential_key, holder_proof_payload, revocation_entry_hash,
    revocation_head_payload, revocation_sign_payload, revocation_snapshot_key, trust_decl_key,
};
use spark_core::identity::{derive_domain_identity, parse_mnemonic, verify_ed25519_signature};
use spark_core::org::DomainType;
use spark_core::org::service::CreateOrganizationInput;
use spark_core::storage::StorageBackend;

const DOMAIN: &str = "plugin:facade-test@v1";
const ORG: &str = "org_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
const REQUEST_ID: &str = "req-1";
const COLLECTION: &str = "finance:monthly@v1";
const NOW: i64 = 1_720_000_000_000;

fn identity_of(key: &SigningKey) -> String {
    hex::encode(Sha256::digest(key.verifying_key().to_bytes()))
}

fn b64_pk(key: &SigningKey) -> String {
    B64.encode(key.verifying_key().to_bytes())
}

fn sign(key: &SigningKey, payload: &str) -> String {
    B64.encode(key.sign(payload.as_bytes()).to_bytes())
}

/// 构造 issuer 签名齐全的结构合法凭证（holder 由调用方指定）。
fn build_credential(
    issuer: &SigningKey,
    holder_identity: &str,
    holder_pub_b64: &str,
) -> Credential {
    let mut cred = Credential {
        cred_v: 1,
        cred_type: "member".to_string(),
        issuer: IdentityRef {
            identity: identity_of(issuer),
            public_key: b64_pk(issuer),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: holder_identity.to_string(),
            public_key: holder_pub_b64.to_string(),
        },
        subject_domain: ORG.to_string(),
        claims: serde_json::Map::new(),
        method: "plugin:test:manual".to_string(),
        link_ref: None,
        issued_at: NOW,
        sig: String::new(),
    };
    cred.sig = sign(issuer, &credential_sign_payload(&cred).expect("payload"));
    cred
}

/// 结构合法的 trustDecl（sigSet 载荷不在 query_verifiers 校验面内，占位即可）。
fn build_trust_decl(verifier: &SigningKey) -> TrustDecl {
    TrustDecl {
        trust_v: 1,
        org_id: ORG.to_string(),
        verifiers: vec![VerifierGrant {
            identity: identity_of(verifier),
            public_key: b64_pk(verifier),
            cred_types: vec!["member".to_string()],
            methods: vec!["plugin:test:*".to_string()],
        }],
        effective_from: 0,
        seq: 1,
        updated_at: 0,
        sig_set: OrgSigSet {
            sig_set_v: 1,
            org_id: ORG.to_string(),
            subject: "00".repeat(32),
            policy_hash: "00".repeat(32),
            roster: RosterCommitment {
                member_set_hash: "00".repeat(32),
                anchor: RosterAnchor {
                    org_id: ORG.to_string(),
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

fn policy_doc(tier: &str, fields: Vec<Value>) -> Value {
    // canonical 形态：空 fields 键省略（线形 skip_serializing_if 口径）
    let mut roster = json!({ "tier": tier });
    if !fields.is_empty() {
        roster["fields"] = json!(fields);
    }
    json!({
        "policyV": 1,
        "engine": "b1",
        "orgId": ORG,
        "roster": roster,
        "updatedAt": NOW,
    })
}

fn finding_codes(out: &Value) -> Vec<String> {
    out["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| f["code"].as_str().expect("code").to_string())
        .collect()
}

#[test]
fn list_held_returns_stored_records_and_skips_corrupted() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    assert!(kernel.credential_list_held().unwrap().is_empty());

    let mut storage = kernel.__test_storage().expect("storage");
    let good_id = "11".repeat(32);
    storage
        .put(
            &held_credential_key(&good_id),
            &json!({"credV": 1}).to_string(),
        )
        .unwrap();
    storage
        .put(&held_credential_key(&"22".repeat(32)), "{ broken json")
        .unwrap();

    let items = kernel.credential_list_held().unwrap();
    assert_eq!(items.len(), 1, "损坏记录跳过不报错");
    assert_eq!(items[0]["credId"], good_id);
    assert_eq!(items[0]["credential"]["credV"], 1);
}

#[test]
fn present_holder_proof_signs_payload_with_holder_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, mnemonic) = init_identity(&mut kernel);
    let seed = parse_mnemonic(&mnemonic).expect("parse mnemonic").seed;
    let domain_identity = derive_domain_identity(&seed, DOMAIN);
    let holder_pub = B64.encode(domain_identity.public_key());

    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let cred = build_credential(&issuer, &domain_identity.id(), &holder_pub);
    let cred_id = credential_id(&cred).expect("credId");
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &held_credential_key(&cred_id),
            &serde_json::to_string(&cred).unwrap(),
        )
        .unwrap();

    let out = kernel
        .credential_present_holder_proof(DOMAIN, &cred_id, REQUEST_ID, ORG, COLLECTION)
        .unwrap();
    let presented_at = out["presentedAt"].as_i64().expect("presentedAt");
    let proof_sig = out["holderProof"]["sig"].as_str().expect("proof sig");
    assert_eq!(out["holderProof"]["credId"], cred_id);
    // holderProof 验签：载荷五键绑定本次请求，密钥 = 凭证 holder 公钥
    let payload = holder_proof_payload(&cred_id, REQUEST_ID, ORG, COLLECTION, presented_at);
    assert!(verify_ed25519_signature(&payload, proof_sig, &holder_pub));
}

#[test]
fn present_holder_proof_rejects_foreign_holder_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // holder 是另一个身份（不是本插件域身份派生公钥）——门面最重要的安全分支：
    // 不得为别人的凭证出具持有证明
    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let other_holder = SigningKey::from_bytes(&[8u8; 32]);
    let cred = build_credential(&issuer, &identity_of(&other_holder), &b64_pk(&other_holder));
    let cred_id = credential_id(&cred).expect("credId");
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &held_credential_key(&cred_id),
            &serde_json::to_string(&cred).unwrap(),
        )
        .unwrap();

    let err = kernel
        .credential_present_holder_proof(DOMAIN, &cred_id, REQUEST_ID, ORG, COLLECTION)
        .unwrap_err();
    assert!(err.to_string().contains("holder key mismatch"), "{err}");
}

#[test]
fn present_holder_proof_rejects_tampered_or_missing_held_credential() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, mnemonic) = init_identity(&mut kernel);
    let seed = parse_mnemonic(&mnemonic).expect("parse mnemonic").seed;
    let domain_identity = derive_domain_identity(&seed, DOMAIN);
    let holder_pub = B64.encode(domain_identity.public_key());

    // 缺失：未持有该 credId
    let err = kernel
        .credential_present_holder_proof(DOMAIN, &"ee".repeat(32), REQUEST_ID, ORG, COLLECTION)
        .unwrap_err();
    assert!(
        err.to_string().contains("held credential not found"),
        "{err}"
    );

    // 篡改：存的是合法 credId 键，但正文被改（credId 复算不符 → 静态校验拒绝签名）
    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let cred = build_credential(&issuer, &domain_identity.id(), &holder_pub);
    let cred_id = credential_id(&cred).expect("credId");
    let mut tampered = cred.clone();
    tampered.issued_at += 1;
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &held_credential_key(&cred_id),
            &serde_json::to_string(&tampered).unwrap(),
        )
        .unwrap();

    let err = kernel
        .credential_present_holder_proof(DOMAIN, &cred_id, REQUEST_ID, ORG, COLLECTION)
        .unwrap_err();
    assert!(err.to_string().contains("held credential invalid"), "{err}");
}

#[test]
fn query_verifiers_rejects_invalid_org_id_shape() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    for bad in ["not-an-org", "org_xyz", &format!("org_{}", "A".repeat(64))] {
        let err = kernel.credential_query_verifiers(bad).unwrap_err();
        assert!(err.to_string().contains("invalid orgId"), "{bad}: {err}");
    }
}

#[test]
fn query_verifiers_missing_corrupted_and_valid_decl() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 声明缺失 → 空集（不是错误）
    let out = kernel.credential_query_verifiers(ORG).unwrap();
    assert_eq!(out["orgId"], ORG);
    assert_eq!(out["verifiers"], json!([]));

    let mut storage = kernel.__test_storage().expect("storage");
    // JSON 损坏 → fail-closed 报错（不把坏数据当空集）
    storage.put(&trust_decl_key(ORG), "{ broken json").unwrap();
    let err = kernel.credential_query_verifiers(ORG).unwrap_err();
    assert!(err.to_string().contains("corrupted trust decl"), "{err}");

    // JSON 合法但结构非法（trustV != 1）→ 同样 fail-closed
    let verifier = SigningKey::from_bytes(&[7u8; 32]);
    let mut bad_decl = serde_json::to_value(build_trust_decl(&verifier)).unwrap();
    bad_decl["trustV"] = json!(2);
    storage
        .put(&trust_decl_key(ORG), &bad_decl.to_string())
        .unwrap();
    let err = kernel.credential_query_verifiers(ORG).unwrap_err();
    assert!(err.to_string().contains("corrupted trust decl"), "{err}");

    // 合法声明 → 返回验证人授权集
    let decl = build_trust_decl(&verifier);
    storage
        .put(&trust_decl_key(ORG), &serde_json::to_string(&decl).unwrap())
        .unwrap();
    let out = kernel.credential_query_verifiers(ORG).unwrap();
    assert_eq!(out["seq"], 1);
    assert_eq!(out["verifiers"][0]["identity"], identity_of(&verifier));
    assert_eq!(out["verifiers"][0]["credTypes"], json!(["member"]));
}

#[test]
fn policy_read_rejects_invalid_org_id_and_returns_none_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let err = kernel.policy_read("org_xyz").unwrap_err();
    assert!(err.to_string().contains("invalid orgId"), "{err}");
    assert!(kernel.policy_read(ORG).unwrap().is_none());
}

#[test]
fn submit_draft_saves_and_read_returns_latest() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let doc = policy_doc("org-only", vec![]);
    let out = kernel.policy_submit_draft(&doc).unwrap();
    let hash = out["policyDocHash"].as_str().expect("hash").to_string();
    assert_eq!(hash.len(), 64);
    assert_eq!(out["findings"], json!([]), "干净首版无 findings");

    let read = kernel.policy_read(ORG).unwrap().expect("draft saved");
    assert_eq!(read["policyDocHash"], hash);
    assert_eq!(read["doc"]["orgId"], ORG);
    assert!(read["savedAt"].is_i64());
}

#[test]
fn submit_draft_compares_prev_draft_and_warns_on_widening() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    kernel
        .policy_submit_draft(&policy_doc("org-only", vec![]))
        .unwrap();
    // 档位提高 + 新增公开字段规则 → 暴露面扩大 Warning 组
    let v2 = policy_doc(
        "representatives",
        vec![json!({"field": "nickname", "audience": "public"})],
    );
    let out = kernel.policy_submit_draft(&v2).unwrap();
    let codes = finding_codes(&out);
    assert!(
        codes.contains(&"roster-tier-raised".to_string()),
        "{codes:?}"
    );
    assert!(codes.contains(&"field-exposed".to_string()), "{codes:?}");
    assert!(
        out["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["severity"] == "warning")
    );
}

#[test]
fn submit_draft_with_error_findings_still_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    kernel
        .policy_submit_draft(&policy_doc(
            "representatives",
            vec![json!({"field": "nickname", "audience": "public"})],
        ))
        .unwrap();
    // 同一字段重复声明 → duplicate-field-rule 为 Error；草稿语义：Error findings
    // 仍保存（供编辑界面回显阻断项），是否阻断发布由插件/产品流程裁决
    let v2 = policy_doc(
        "representatives",
        vec![
            json!({"field": "nickname", "audience": "public"}),
            json!({"field": "nickname", "audience": "representatives"}),
        ],
    );
    let out = kernel.policy_submit_draft(&v2).unwrap();
    let dup = out["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "duplicate-field-rule")
        .expect("duplicate-field-rule finding");
    assert_eq!(dup["severity"], "error");

    let read = kernel.policy_read(ORG).unwrap().expect("draft still saved");
    assert_eq!(read["doc"]["roster"]["fields"].as_array().unwrap().len(), 2);
}

// ------------------------------------------------------------------
// credential_verify / credential_query_revocations（sdk.credentials 验证/注销查询）
// ------------------------------------------------------------------

/// 真实时钟毫秒（注销头承诺新鲜度按本机时钟判定，夹具不能用固定 NOW）。
fn real_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// 构造 issuer 的合法注销快照（空链 = 只含承诺「无任何注销」语义的最小形态：
/// 规格未定空链头形态，故快照至少含一条占位条目；entries 不含被验 credId）。
fn build_revocation_snapshot(issuer: &SigningKey, revoked_cred_ids: &[String]) -> spark_core::credential::RevocationSnapshot {
    let now = real_now_ms();
    let mut entries: Vec<RevocationEntry> = Vec::new();
    let mut prev: Option<String> = None;
    for (index, cred_id) in revoked_cred_ids.iter().enumerate() {
        let mut entry = RevocationEntry {
            rev_v: 1,
            issuer: identity_of(issuer),
            seq: (index + 1) as u64,
            prev_hash: prev.clone(),
            cred_id: cred_id.clone(),
            revoked_at: now,
            reason: None,
            sig: String::new(),
        };
        entry.sig = sign(issuer, &revocation_sign_payload(&entry).expect("payload"));
        prev = Some(revocation_entry_hash(&entry).expect("entryHash"));
        entries.push(entry);
    }
    let mut head = RevocationHead {
        rev_head_v: 1,
        issuer: identity_of(issuer),
        head_seq: entries.len() as u64,
        head_hash: prev.expect("at least one entry"),
        as_of: now,
        sig: String::new(),
    };
    head.sig = sign(issuer, &revocation_head_payload(&head).expect("payload"));
    spark_core::credential::RevocationSnapshot { entries, head }
}

#[test]
fn verify_malformed_and_tampered_credential_fail_static() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 非协议线形 → 结构化 invalid（不整体报错，插件可回显）
    let out = kernel.credential_verify(&json!({"credV": 1})).unwrap();
    assert_eq!(out["valid"], false);
    assert_eq!(out["checks"]["static"], false);
    assert!(
        out["reason"].as_str().unwrap().starts_with("malformed-credential"),
        "{out}"
    );

    // 签名篡改（issuedAt 改动后未重签）→ 静态链不过
    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let holder = SigningKey::from_bytes(&[9u8; 32]);
    let mut cred = build_credential(&issuer, &identity_of(&holder), &b64_pk(&holder));
    cred.issued_at += 1;
    let out = kernel
        .credential_verify(&serde_json::to_value(&cred).unwrap())
        .unwrap();
    assert_eq!(out["valid"], false);
    assert_eq!(out["checks"]["static"], false);
    assert_eq!(out["reason"], "invalid-signature");
}

#[test]
fn verify_trust_and_revocation_stages() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let holder = SigningKey::from_bytes(&[9u8; 32]);
    let cred = build_credential(&issuer, &identity_of(&holder), &b64_pk(&holder));
    let cred_id = credential_id(&cred).expect("credId");
    let cred_value = serde_json::to_value(&cred).unwrap();

    // 信任声明缺失 → trust=false（fail-closed）
    let out = kernel.credential_verify(&cred_value).unwrap();
    assert_eq!(out["valid"], false);
    assert_eq!(out["checks"]["static"], true);
    assert_eq!(out["checks"]["trust"], false);
    assert_eq!(out["reason"], "issuer-not-trusted");
    assert_eq!(out["credId"], cred_id);

    // 信任声明就位但无注销快照 → revocation-unavailable（fail-closed）
    let mut storage = kernel.__test_storage().expect("storage");
    let decl = build_trust_decl(&issuer);
    storage
        .put(&trust_decl_key(ORG), &serde_json::to_string(&decl).unwrap())
        .unwrap();
    let out = kernel.credential_verify(&cred_value).unwrap();
    assert_eq!(out["valid"], false);
    assert_eq!(out["checks"]["trust"], true);
    assert_eq!(out["checks"]["revocation"], "unavailable");
    assert_eq!(out["reason"], "revocation-unavailable");

    // 快照就位且不含本凭证 → 全链通过
    let snapshot = build_revocation_snapshot(&issuer, &["ff".repeat(32)]);
    storage
        .put(
            &revocation_snapshot_key(&identity_of(&issuer)),
            &serde_json::to_string(&snapshot).unwrap(),
        )
        .unwrap();
    let out = kernel.credential_verify(&cred_value).unwrap();
    assert_eq!(out["valid"], true, "{out}");
    assert_eq!(out["checks"]["revocation"], "not-revoked");
    assert_eq!(out["reason"], serde_json::Value::Null);

    // 快照含本凭证 → revoked
    let snapshot = build_revocation_snapshot(&issuer, &[cred_id.clone()]);
    storage
        .put(
            &revocation_snapshot_key(&identity_of(&issuer)),
            &serde_json::to_string(&snapshot).unwrap(),
        )
        .unwrap();
    let out = kernel.credential_verify(&cred_value).unwrap();
    assert_eq!(out["valid"], false);
    assert_eq!(out["checks"]["revocation"], "revoked");
    assert_eq!(out["reason"], "revoked");
}

#[test]
fn query_revocations_reports_snapshot_or_absence() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let err = kernel.credential_query_revocations("not-an-issuer").unwrap_err();
    assert!(err.to_string().contains("invalid issuer"), "{err}");

    let issuer = SigningKey::from_bytes(&[7u8; 32]);
    let issuer_id = identity_of(&issuer);
    // 快照缺失 → available:false（不冒充「无注销」）
    let out = kernel.credential_query_revocations(&issuer_id).unwrap();
    assert_eq!(out["available"], false);

    // 快照就位 → 头承诺 + 条目清单
    let revoked = "ab".repeat(32);
    let snapshot = build_revocation_snapshot(&issuer, std::slice::from_ref(&revoked));
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &revocation_snapshot_key(&issuer_id),
            &serde_json::to_string(&snapshot).unwrap(),
        )
        .unwrap();
    let out = kernel.credential_query_revocations(&issuer_id).unwrap();
    assert_eq!(out["available"], true);
    assert_eq!(out["headSeq"], 1);
    assert_eq!(out["entries"][0]["credId"], revoked);
}

// ------------------------------------------------------------------
// policy_publish（sdk.policy 发布合入：草稿 → OrgSigSet → org:policydoc: 键域）
// ------------------------------------------------------------------

/// 指定 orgId 的策略文档（policy_doc helper 的 orgId 参数化版）。
fn policy_doc_for(org_id: &str, tier: &str) -> Value {
    json!({
        "policyV": 1,
        "engine": "b1",
        "orgId": org_id,
        "roster": { "tier": tier },
        "updatedAt": NOW,
    })
}

/// 创建共同体域组织（创世哈希型 orgId + 创世策略记录 + 本机为 admin）并
/// 直写一条存证锚（sign_anchor 夹具：evidence_anchor 空链不锚，测试无真实
/// 存证链头可用），满足 policy_publish 的全部前置。
fn create_community_with_anchor(kernel: &mut spark_core::kernel::Kernel, name: &str) -> String {
    let input = CreateOrganizationInput {
        name: name.to_string(),
        domain_type: Some(DomainType::Community),
        ..Default::default()
    };
    let org_id = kernel.create_org(input).unwrap().record.org_id;
    let anchor = spark_core::evidence::sign_anchor(
        &SigningKey::from_bytes(&[5u8; 32]),
        &org_id,
        "node-t",
        1,
        &"ab".repeat(32),
        real_now_ms(),
    );
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &spark_core::evidence::anchor_key(&org_id, "node-t"),
            &serde_json::to_string(&anchor).unwrap(),
        )
        .unwrap();
    org_id
}

#[test]
fn publish_requires_draft_and_valid_org_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let err = kernel.policy_publish("org_xyz").unwrap_err();
    assert!(err.to_string().contains("invalid orgId"), "{err}");
    let err = kernel.policy_publish(ORG).unwrap_err();
    assert!(err.to_string().contains("no policy draft"), "{err}");
}

#[test]
fn publish_signs_draft_into_shared_key_domain() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);
    let org_id = create_community_with_anchor(&mut kernel, "阳光共同体");

    kernel
        .policy_submit_draft(&policy_doc_for(&org_id, "org-only"))
        .unwrap();
    let out = kernel.policy_publish(&org_id).unwrap();
    let doc_hash = out["policyDocHash"].as_str().expect("hash").to_string();
    assert_eq!(doc_hash.len(), 64);
    // A16 签名面：signer = org_user_id（org-access 域公钥的 sha256hex），不再
    // 出示 rootId；与本机成员条目 accessKey 派生值一致。
    let my_uid = {
        let s = kernel.__test_storage().expect("storage");
        let rec = spark_core::org::OrganizationService::get_record(&s, &org_id)
            .unwrap()
            .unwrap();
        rec.members
            .iter()
            .find(|m| m.root_id == root_id)
            .and_then(|m| m.org_user_id())
            .expect("创建即发布 accessKey")
    };
    assert_eq!(out["signer"], my_uid, "签名主体 = 本机 org_user_id（A16）");
    assert_ne!(out["signer"], root_id, "公共面不出示 rootId");
    assert_eq!(out["degraded"], false, "创世哈希型组织不降级");

    // 发布件落 org:policydoc: 键域，携带 OrgSigSet 且 subject 绑定文档哈希
    let storage = kernel.__test_storage().expect("storage");
    let raw = storage
        .get(&spark_core::org::service::policy_doc_key(&org_id))
        .unwrap()
        .expect("published doc stored");
    let published: spark_core::policy::PolicyDoc = serde_json::from_str(&raw).unwrap();
    let sig_set = published.sig_set.clone().expect("sigSet attached");
    assert_eq!(sig_set.subject, doc_hash);
    assert_eq!(sig_set.signatures.len(), 1);
    assert_eq!(sig_set.signatures[0].signer, my_uid);
    // 名册快照双写：成员条目携带 orgUserId（验证端双键兼容的前提）。
    let snapshot = sig_set.roster.snapshot.as_ref().expect("snapshot");
    assert!(
        snapshot
            .iter()
            .any(|m| m.org_user_id.as_deref() == Some(my_uid.as_str())),
        "名册快照携带 orgUserId"
    );

    // 入站合入裁决：同值重放 Accept/KeepCurrent；篡改 sigSet subject → Rejected
    let value = serde_json::to_value(&published).unwrap();
    // 本地已有同 updatedAt 版本 → KeepCurrent（幂等重放不放大）
    let verdict = spark_core::org::service::adjudicate_incoming_policy_doc(&storage, &org_id, &value).unwrap();
    assert_eq!(
        verdict,
        spark_core::org::service::PolicyDocMerge::KeepCurrent
    );
    let mut tampered = value.clone();
    tampered["sigSet"]["subject"] = json!("00".repeat(32));
    let verdict =
        spark_core::org::service::adjudicate_incoming_policy_doc(&storage, &org_id, &tampered).unwrap();
    assert_eq!(verdict, spark_core::org::service::PolicyDocMerge::Rejected);

    kernel.shutdown().unwrap();
}

#[test]
fn publish_rejects_non_admin_and_missing_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 草稿的 orgId 无策略链（未发布创世记录的 legacy 形状 orgId）→ 如实报错
    kernel
        .policy_submit_draft(&policy_doc_for(ORG, "org-only"))
        .unwrap();
    let err = kernel.policy_publish(ORG).unwrap_err();
    assert!(err.to_string().contains("no policy chain"), "{err}");
}

// ------------------------------------------------------------------
// disclosure_publish（A15 / membership §4.3：名册开放声明发布 + 公示延迟）
// ------------------------------------------------------------------

#[test]
fn disclosure_publish_pub_delay_and_merge() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);
    let org_id = create_community_with_anchor(&mut kernel, "阳光共同体");
    let target = ORG; // 上级目标域（线形合法的创世哈希型 orgId 替身）

    // 首个非全隐开放声明 = 暴露面扩大：未显式确认 → 如实报错，不落库
    let err = kernel
        .disclosure_publish(
            &org_id,
            target,
            spark_core::policy::RosterTier::Representatives,
            vec![],
            vec![],
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("暴露面扩大"), "{err}");
    let storage = kernel.__test_storage().expect("storage");
    assert!(
        storage
            .get(&spark_core::policy::disclosure_key(&org_id, target))
            .unwrap()
            .is_none(),
        "未确认的扩大发布不得落库"
    );

    // 确认后发布：公示延迟（effectiveAt = updatedAt + 24h），version = 1
    let out = kernel
        .disclosure_publish(
            &org_id,
            target,
            spark_core::policy::RosterTier::Representatives,
            vec![],
            vec!["finance:monthly@v1".to_string()],
            true,
        )
        .unwrap();
    assert_eq!(out["version"], 1);
    assert_eq!(out["widening"], true);
    let updated_at = out["updatedAt"].as_i64().expect("updatedAt");
    assert_eq!(
        out["effectiveAt"].as_i64().unwrap(),
        updated_at + spark_core::policy::DISCLOSURE_PUB_PERIOD_MS,
        "扩大方向公示延迟 24h 生效"
    );
    let hash = out["disclosureHash"].as_str().expect("hash").to_string();
    assert_eq!(hash.len(), 64);

    // 落库线形：sigSet.subject 绑定 disclosureHash，签名者 = org_user_id（A16）
    let storage = kernel.__test_storage().expect("storage");
    let raw = storage
        .get(&spark_core::policy::disclosure_key(&org_id, target))
        .unwrap()
        .expect("disclosure stored");
    let published: spark_core::policy::DisclosureRecord = serde_json::from_str(&raw).unwrap();
    spark_core::policy::validate_disclosure(&published).expect("published record valid");
    let sig_set = published.sig_set.clone().expect("sigSet attached");
    assert_eq!(sig_set.subject, hash);
    assert_eq!(sig_set.signatures.len(), 1);

    // 公示延迟窗口内：求值不装配（发布即公示 ≠ 即时生效）
    let view = spark_core::policy::eval_disclosure(&[&published], target, updated_at);
    assert_eq!(view, spark_core::policy::DisclosureView::default());
    // 生效时刻起：视图 = 声明内容
    let view = spark_core::policy::eval_disclosure(
        &[&published],
        target,
        updated_at + spark_core::policy::DISCLOSURE_PUB_PERIOD_MS,
    );
    assert_eq!(view.tier, spark_core::policy::RosterTier::Representatives);
    assert_eq!(view.collections, vec!["finance:monthly@v1".to_string()]);

    // 收窄（档位降回仅组织）即时生效：version 2，effectiveAt == updatedAt
    let out2 = kernel
        .disclosure_publish(
            &org_id,
            target,
            spark_core::policy::RosterTier::OrgOnly,
            vec![],
            vec![],
            false, // 收窄无需确认
        )
        .unwrap();
    assert_eq!(out2["version"], 2);
    assert_eq!(out2["widening"], false);
    assert_eq!(
        out2["effectiveAt"], out2["updatedAt"],
        "收窄即时生效（无公示延迟）"
    );

    // 入站合入裁决（orgsync-data `org:disclosure:` 键分支同一函数）：
    let storage = kernel.__test_storage().expect("storage");
    let v2: spark_core::policy::DisclosureRecord = serde_json::from_str(
        &storage
            .get(&spark_core::policy::disclosure_key(&org_id, target))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let value = serde_json::to_value(&v2).unwrap();
    // 同版本重放 → KeepCurrent（幂等不放大）
    let verdict = spark_core::org::service::adjudicate_incoming_disclosure(
        &storage, &org_id, target, &value,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::DisclosureMerge::KeepCurrent);
    // 篡改 sigSet subject（搬签）→ Rejected
    let mut tampered = value.clone();
    tampered["sigSet"]["subject"] = json!("00".repeat(32));
    let verdict = spark_core::org::service::adjudicate_incoming_disclosure(
        &storage, &org_id, target, &tampered,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::DisclosureMerge::Rejected);
    // 键域与记录错位（targetDomain 不符）→ Rejected
    let verdict = spark_core::org::service::adjudicate_incoming_disclosure(
        &storage,
        &org_id,
        "org_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        &value,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::DisclosureMerge::Rejected);
    // 高版本发布件（取首个声明 v1 对本地 v2 属旧版 → KeepCurrent）
    let v1_value = serde_json::to_value(&published).unwrap();
    let verdict = spark_core::org::service::adjudicate_incoming_disclosure(
        &storage, &org_id, target, &v1_value,
    )
    .unwrap();
    assert_eq!(
        verdict,
        spark_core::org::service::DisclosureMerge::KeepCurrent,
        "旧版本入站不覆盖本地新版（version LWW）"
    );
    // 全新键域的 Accept：换 targetDomain 的 v1 发布件本地缺席 → Accept
    let fresh_target =
        "org_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let out3 = kernel
        .disclosure_publish(
            &org_id,
            fresh_target,
            spark_core::policy::RosterTier::OrgOnly,
            vec![],
            vec![],
            false,
        )
        .unwrap();
    assert_eq!(out3["widening"], false, "全隐首声明不算扩大");
    let storage = kernel.__test_storage().expect("storage");
    let v1_fresh: spark_core::policy::DisclosureRecord = serde_json::from_str(
        &storage
            .get(&spark_core::policy::disclosure_key(&org_id, fresh_target))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    // 未发布前的本地缺席场景：先删掉再裁决（Accept 路径）
    let mut storage2 = kernel.__test_storage().expect("storage");
    storage2
        .delete(&spark_core::policy::disclosure_key(&org_id, fresh_target))
        .unwrap();
    let verdict = spark_core::org::service::adjudicate_incoming_disclosure(
        &storage2,
        &org_id,
        fresh_target,
        &serde_json::to_value(&v1_fresh).unwrap(),
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::DisclosureMerge::Accept);

    kernel.shutdown().unwrap();
}

#[test]
fn disclosure_publish_rejects_bad_org_id_and_missing_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let err = kernel
        .disclosure_publish(
            "org_xyz",
            ORG,
            spark_core::policy::RosterTier::OrgOnly,
            vec![],
            vec![],
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("invalid orgId"), "{err}");
    // 无策略链（legacy 组织未发布创世记录）→ 如实报错
    let err = kernel
        .disclosure_publish(
            ORG,
            "org_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            spark_core::policy::RosterTier::OrgOnly,
            vec![],
            vec![],
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("no policy chain"), "{err}");
}

// ------------------------------------------------------------------
// accept_policy_publish（A17 / membership §4.5：准入策略声明发布 + 公示延迟）
// ------------------------------------------------------------------

fn accept_rule(cred_type: &str, issuer_trust: &str) -> spark_core::policy::AcceptCredentialRule {
    spark_core::policy::AcceptCredentialRule {
        cred_type: cred_type.to_string(),
        issuer_trust: issuer_trust.to_string(),
    }
}

#[test]
fn accept_policy_publish_pub_delay_and_merge() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);
    let org_id = create_community_with_anchor(&mut kernel, "阳光共同体");

    // 首个非空准入声明 = 准入面扩大：未显式确认 → 如实报错，不落库
    let err = kernel
        .accept_policy_publish(&org_id, vec![accept_rule("member", ORG)], false)
        .unwrap_err();
    assert!(err.to_string().contains("准入面扩大"), "{err}");
    let storage = kernel.__test_storage().expect("storage");
    assert!(
        storage
            .get(&spark_core::policy::accept_policy_key(&org_id))
            .unwrap()
            .is_none(),
        "未确认的扩大发布不得落库"
    );

    // 确认后发布：公示延迟（effectiveAt = updatedAt + 24h），version = 1
    let out = kernel
        .accept_policy_publish(&org_id, vec![accept_rule("member", ORG)], true)
        .unwrap();
    assert_eq!(out["version"], 1);
    assert_eq!(out["widening"], true);
    let updated_at = out["updatedAt"].as_i64().expect("updatedAt");
    assert_eq!(
        out["effectiveAt"].as_i64().unwrap(),
        updated_at + spark_core::policy::ACCEPT_POLICY_PUB_PERIOD_MS,
        "首个策略声明公示延迟 24h 生效"
    );
    let hash = out["acceptPolicyHash"].as_str().expect("hash").to_string();
    assert_eq!(hash.len(), 64);

    // 落库线形：sigSet.subject 绑定 acceptPolicyHash；公示延迟窗口内不采信
    let storage = kernel.__test_storage().expect("storage");
    let raw = storage
        .get(&spark_core::policy::accept_policy_key(&org_id))
        .unwrap()
        .expect("accept policy stored");
    let published: spark_core::policy::AcceptPolicyRecord = serde_json::from_str(&raw).unwrap();
    spark_core::policy::validate_accept_policy(&published).expect("published record valid");
    assert_eq!(published.sig_set.as_ref().expect("sigSet").subject, hash);
    assert!(
        spark_core::policy::effective_accept_policy(Some(&published), updated_at).is_none(),
        "公示延迟窗口内不采信（发布即公示 ≠ 即时生效）"
    );
    assert!(
        spark_core::policy::effective_accept_policy(
            Some(&published),
            updated_at + spark_core::policy::ACCEPT_POLICY_PUB_PERIOD_MS,
        )
        .is_some(),
        "生效时刻起采信"
    );

    // 收窄（移除全部规则）即时生效：version 2，effectiveAt == updatedAt
    let out2 = kernel
        .accept_policy_publish(&org_id, vec![], false)
        .unwrap();
    assert_eq!(out2["version"], 2);
    assert_eq!(out2["widening"], false);
    assert_eq!(
        out2["effectiveAt"], out2["updatedAt"],
        "收窄即时生效（无公示延迟）"
    );

    // 入站合入裁决（orgsync-data `org:accept:` 键分支同一函数）：
    let storage = kernel.__test_storage().expect("storage");
    let v2: spark_core::policy::AcceptPolicyRecord = serde_json::from_str(
        &storage
            .get(&spark_core::policy::accept_policy_key(&org_id))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let value = serde_json::to_value(&v2).unwrap();
    // 同版本重放 → KeepCurrent（幂等不放大）
    let verdict = spark_core::org::service::adjudicate_incoming_accept_policy(
        &storage, &org_id, &value,
    )
    .unwrap();
    assert_eq!(
        verdict,
        spark_core::org::service::AcceptPolicyMerge::KeepCurrent
    );
    // 篡改 sigSet subject（搬签）→ Rejected
    let mut tampered = value.clone();
    tampered["sigSet"]["subject"] = json!("00".repeat(32));
    let verdict = spark_core::org::service::adjudicate_incoming_accept_policy(
        &storage, &org_id, &tampered,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::AcceptPolicyMerge::Rejected);
    // 键域与记录错位（orgId 不符）→ Rejected
    let verdict = spark_core::org::service::adjudicate_incoming_accept_policy(
        &storage,
        "org_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        &value,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::AcceptPolicyMerge::Rejected);
    // 旧版本入站不覆盖本地新版（version LWW）
    let v1_value = serde_json::to_value(&published).unwrap();
    let verdict = spark_core::org::service::adjudicate_incoming_accept_policy(
        &storage, &org_id, &v1_value,
    )
    .unwrap();
    assert_eq!(
        verdict,
        spark_core::org::service::AcceptPolicyMerge::KeepCurrent
    );
    // 本地缺席场景 → Accept（先删掉再裁决）
    let mut storage2 = kernel.__test_storage().expect("storage");
    storage2
        .delete(&spark_core::policy::accept_policy_key(&org_id))
        .unwrap();
    let verdict = spark_core::org::service::adjudicate_incoming_accept_policy(
        &storage2, &org_id, &v1_value,
    )
    .unwrap();
    assert_eq!(verdict, spark_core::org::service::AcceptPolicyMerge::Accept);

    kernel.shutdown().unwrap();
}

#[test]
fn accept_policy_publish_rejects_bad_org_id_and_missing_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    let err = kernel
        .accept_policy_publish("org_xyz", vec![], false)
        .unwrap_err();
    assert!(err.to_string().contains("invalid orgId"), "{err}");
    // 无策略链（legacy 组织未发布创世记录）→ 如实报错
    let err = kernel
        .accept_policy_publish(ORG, vec![], false)
        .unwrap_err();
    assert!(err.to_string().contains("no policy chain"), "{err}");
    // 空规则首声明不算扩大（无需确认即可发布）——但也需真实组织与策略链
}

#[test]
fn submit_draft_rejects_unknown_or_misspelled_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 误拼键（正确键为 sigSet）：serde 默认忽略未知键会无声丢签名，
    // round-trip 比对拒绝
    let mut doc = policy_doc("org-only", vec![]);
    doc["sigset"] = json!({"sigSetV": 1});
    let err = kernel.policy_submit_draft(&doc).unwrap_err();
    assert!(
        err.to_string().contains("unknown or non-canonical keys"),
        "{err}"
    );
    assert!(
        kernel.policy_read(ORG).unwrap().is_none(),
        "拒绝的草稿不得落盘"
    );
}
