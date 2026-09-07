//! golden vectors 验收测试（community-affairs C2：credential 纯逻辑模块）。
//!
//! 加载 `../spec/vectors/community.json`，逐字节断言 credential 相关 case 组：
//! - `credential.issue` / `credential.revokeChain` / `credential.revHead`（§2/§3 线形与哈希链）
//! - `trustDecl`（§4 合入校验：sigSet 绑定 + OrgSigSet 验证注入 + LWW）
//! - `samePersonLink`（§5 opt-in 关联声明）
//! - `readGate.envelope`（read-gate §3 holderProof 载荷逐字节）
//! - `credential.trustTimeline` / `readGate.verifyChain`（C2 生成器回填的实现依赖组）
//!
//! C1（affair）消费测试在 `community_vectors.rs`，与本文件分文件避让并行冲突。
//!
//! OrgSigSet 五步验证链归 C3；本测试以 `VectorSigSetVerifier` 实现向量可覆盖的
//! 部分（分量载荷重建验签 + memberSetHash 快照复算 + any-admin 策略求值），
//! 锚根复算（org-signature §5 第 4 步中段）依赖存证锚状态，向量内为固定替身值，
//! 测试侧按已知值放行（注释处）。

use std::collections::BTreeSet;

use serde_json::{Value, json};
use sha2::Digest as _;
use spark_core::credential::{
    Credential, CredentialError, CredentialReadPolicy, FRESHNESS_WINDOW_MS, MergeVerdict,
    OrgSigSet, OrgSigSetVerifier, ReadAuth, RevocationEntry, RevocationHead, SamePersonLink,
    TrustDecl, credential_id, credential_sign_payload, holder_proof_payload, issuer_trusted_at,
    link_id, link_sign_payload, merge_trust_decl, revocation_entry_hash, revocation_head_payload,
    revocation_sign_payload, trust_decl_at, trust_decl_hash, verify_credential_static,
    verify_holder_proof, verify_link_membership, verify_not_revoked, verify_read_auth,
    verify_revocation_chain, verify_same_person_link,
};
use spark_core::evidence::normalize_object;
use spark_core::identity::verify_ed25519_signature;

const NOW: i64 = 1_720_000_000_000;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

fn cred_from(value: &Value) -> Credential {
    serde_json::from_value(value.clone()).expect("deserialize credential")
}

// ------------------------------------------------------------------
// 测试侧 OrgSigSet 验证器（向量可覆盖的 §5 子集；C3 提供线上实现）
// ------------------------------------------------------------------

/// org-signature §2.1 分量签名载荷（固定 8 键）。
fn component_payload(sig_set: &OrgSigSet) -> String {
    normalize_object(&json!({
        "sigSetV": sig_set.sig_set_v,
        "orgId": sig_set.org_id,
        "subject": sig_set.subject,
        "policyHash": sig_set.policy_hash,
        "memberSetHash": sig_set.roster.member_set_hash,
        "anchorRoot": sig_set.roster.anchor.anchor_root,
        "anchorTs": sig_set.roster.anchor.ts,
        "signedAt": sig_set.signed_at,
    }))
}

/// 复算 memberSetHash（§3：按 identity 字典序排序的成员条目数组）。
fn member_set_hash(sig_set: &OrgSigSet) -> Option<String> {
    let snapshot = sig_set.roster.snapshot.as_ref()?;
    let mut entries: Vec<Value> = snapshot
        .iter()
        .map(|m| json!({ "identity": m.identity, "role": m.role }))
        .collect();
    entries.sort_by(|a, b| a["identity"].as_str().cmp(&b["identity"].as_str()));
    Some(spark_core::evidence::sha256_hex(&normalize_object(
        &Value::Array(entries),
    )))
}

/// 向量场景的 OrgSigSet 验证器：分量验签 + 名册快照复算 + any-admin 求值。
/// 锚根复算不可由向量自证（anchorRoot 为生成器固定替身），按已知值放行。
struct VectorSigSetVerifier;

impl OrgSigSetVerifier for VectorSigSetVerifier {
    fn verify_org_sig_set(&self, sig_set: &OrgSigSet) -> bool {
        if sig_set.sig_set_v != 1 {
            return false;
        }
        let snapshot = match &sig_set.roster.snapshot {
            Some(s) => s,
            None => return false,
        };
        // 名册回查（§5 第 4 步前半）：快照复算 memberSetHash 匹配承诺
        if member_set_hash(sig_set).as_deref() != Some(sig_set.roster.member_set_hash.as_str()) {
            return false;
        }
        let admins: BTreeSet<&str> = snapshot
            .iter()
            .filter(|m| m.role == "admin")
            .map(|m| m.identity.as_str())
            .collect();
        let payload = component_payload(sig_set);
        let mut seen = BTreeSet::new();
        let mut valid_admin_sigs = 0u32;
        for comp in &sig_set.signatures {
            // 结构（§5 第 1 步）：signer == sha256hex(publicKey) 绑定 + 去重（重复只计一次）
            let Ok(raw) = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &comp.public_key,
            ) else {
                return false;
            };
            if raw.len() != 32 || hex::encode(sha2::Sha256::digest(&raw)) != comp.signer {
                return false;
            }
            if !seen.insert(comp.signer.as_str()) {
                continue;
            }
            if !admins.contains(comp.signer.as_str()) {
                continue;
            }
            // 分量验签（§5 第 3 步）
            if verify_ed25519_signature(&payload, &comp.sig, &comp.public_key) {
                valid_admin_sigs += 1;
            }
        }
        // 策略求值（§5 第 5 步）：向量组织均为 any-admin 创世策略
        valid_admin_sigs >= 1
    }
}

// ------------------------------------------------------------------
// credential.issue
// ------------------------------------------------------------------

#[test]
fn credential_issue_byte_exact() {
    let v = vectors();
    let group = &v["credential.issue"]["expect"];
    let cred = cred_from(&group["credential"]);

    // canonical 载荷逐字节 + credId + sig 固定值
    assert_eq!(
        credential_sign_payload(&cred).unwrap(),
        group["payload"].as_str().unwrap()
    );
    assert_eq!(
        credential_id(&cred).unwrap(),
        group["credId"].as_str().unwrap()
    );
    assert_eq!(cred.sig, group["credential"]["sig"].as_str().unwrap());
    assert!(verify_credential_static(&cred, Some(group["credId"].as_str().unwrap())).is_ok());
}

#[test]
fn credential_issue_tamper_fails() {
    let v = vectors();
    let group = &v["credential.issue"]["expect"];
    let mut tampered = group["credential"].clone();
    tampered["claims"]["household"] = json!("3-503");
    let cred = cred_from(&tampered);
    // 篡改字段：credId 复算与引用值不符（静态链 fail-closed）
    let err = verify_credential_static(&cred, Some(group["credId"].as_str().unwrap())).unwrap_err();
    assert_eq!(err.kind(), "cred-id-mismatch");
    // 不带引用值时：签名不再覆盖篡改后字段
    let err = verify_credential_static(&cred, None).unwrap_err();
    assert_eq!(err.kind(), "invalid-signature");
}

// ------------------------------------------------------------------
// credential.revokeChain / credential.revHead
// ------------------------------------------------------------------

#[test]
fn revocation_chain_byte_exact() {
    let v = vectors();
    let entries = v["credential.revokeChain"]["expect"]["entries"]
        .as_array()
        .unwrap();
    let verifier_pubkey = v["meta"]["actors"]["verifier"]["publicKey"]
        .as_str()
        .unwrap();

    let mut parsed: Vec<RevocationEntry> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let entry: RevocationEntry = serde_json::from_value(e["entry"].clone()).unwrap();
        assert_eq!(
            revocation_sign_payload(&entry).unwrap(),
            e["payload"].as_str().unwrap(),
            "entry {i} payload"
        );
        assert_eq!(
            revocation_entry_hash(&entry).unwrap(),
            e["entryHash"].as_str().unwrap(),
            "entry {i} hash"
        );
        parsed.push(entry);
    }
    assert!(verify_revocation_chain(&parsed, verifier_pubkey).is_ok());

    // 断链（删中间条目）必败
    let broken = vec![parsed[0].clone(), parsed[2].clone()];
    let err = verify_revocation_chain(&broken, verifier_pubkey).unwrap_err();
    assert_eq!(err.kind(), "revocation-chain-broken");
}

#[test]
fn revocation_head_byte_exact() {
    let v = vectors();
    let group = &v["credential.revHead"]["expect"];
    let head: RevocationHead = serde_json::from_value(group["head"].clone()).unwrap();
    let verifier_pubkey = v["meta"]["actors"]["verifier"]["publicKey"]
        .as_str()
        .unwrap();

    assert_eq!(
        revocation_head_payload(&head).unwrap(),
        group["payload"].as_str().unwrap()
    );
    assert!(verify_ed25519_signature(
        &revocation_head_payload(&head).unwrap(),
        &head.sig,
        verifier_pubkey
    ));
}

#[test]
fn freshness_window_matches_meta() {
    let v = vectors();
    assert_eq!(
        FRESHNESS_WINDOW_MS,
        v["meta"]["constants"]["freshnessWindowMs"]
            .as_i64()
            .unwrap()
    );
    assert_eq!(NOW, v["meta"]["constants"]["nowMs"].as_i64().unwrap());
}

// ------------------------------------------------------------------
// trustDecl
// ------------------------------------------------------------------

#[test]
fn trust_decl_byte_exact_and_merge() {
    let v = vectors();
    let group = &v["trustDecl"]["expect"];
    let decl: TrustDecl = serde_json::from_value(group["record"].clone()).unwrap();
    let now = v["meta"]["constants"]["nowMs"].as_i64().unwrap();

    // canonical 载荷逐字节 + trustHash 固定值
    assert_eq!(
        trust_decl_hash(&decl).unwrap(),
        group["trustHash"].as_str().unwrap()
    );
    // sigSet.subject 绑定 = trustHash
    assert_eq!(decl.sig_set.subject, group["trustHash"].as_str().unwrap());

    // 合入校验：有效 sigSet → Accept
    let verdict = merge_trust_decl(None, &decl, &VectorSigSetVerifier).unwrap();
    assert_eq!(verdict, MergeVerdict::Accept);
    let _ = now;
}

#[test]
fn trust_decl_invalid_sig_set_rejected() {
    let v = vectors();
    let group = &v["trustDecl"]["expect"];
    let decl: TrustDecl = serde_json::from_value(group["record"].clone()).unwrap();

    // subject 篡改：绑定校验拒绝（不进入 OrgSigSet 验证）
    let mut bad_subject = decl.clone();
    bad_subject.sig_set.subject = "0".repeat(64);
    let err = merge_trust_decl(None, &bad_subject, &VectorSigSetVerifier).unwrap_err();
    assert_eq!(err.kind(), "sig-set-subject-mismatch");

    // 分量签名篡改（换一个有效签名但载荷不符）：OrgSigSet 验证拒绝合入
    let forged = &v["orgSigSet.tamper"]["expect"]["forgedSubjectPayload"];
    let mut bad_sig = decl.clone();
    // 用 forged subject 对应的载荷语境下原签名必败——直接断言测试验证器拒绝
    assert!(forged.is_string());
    bad_sig.sig_set.signatures[0].sig =
        v["orgSigSet.mOfN"]["expect"]["sigSet"]["signatures"][1]["sig"]
            .as_str()
            .unwrap()
            .to_string();
    let err = merge_trust_decl(None, &bad_sig, &VectorSigSetVerifier).unwrap_err();
    assert_eq!(err.kind(), "sig-set-rejected");
}

#[test]
fn trust_decl_lww_keep_current() {
    let v = vectors();
    let group = &v["trustDecl"]["expect"];
    let incoming: TrustDecl = serde_json::from_value(group["record"].clone()).unwrap();
    let mut current = incoming.clone();
    current.seq = 2; // 本地版本更新（seq 大者胜）
    let verdict = merge_trust_decl(Some(&current), &incoming, &VectorSigSetVerifier).unwrap();
    assert_eq!(verdict, MergeVerdict::KeepCurrent);

    // 同 seq：updatedAt 大者胜
    let mut current = incoming.clone();
    current.updated_at = incoming.updated_at + 1;
    let verdict = merge_trust_decl(Some(&current), &incoming, &VectorSigSetVerifier).unwrap();
    assert_eq!(verdict, MergeVerdict::KeepCurrent);
    let mut current = incoming.clone();
    current.updated_at = incoming.updated_at - 1;
    let verdict = merge_trust_decl(Some(&current), &incoming, &VectorSigSetVerifier).unwrap();
    assert_eq!(verdict, MergeVerdict::Accept);
}

// ------------------------------------------------------------------
// samePersonLink
// ------------------------------------------------------------------

#[test]
fn same_person_link_byte_exact() {
    let v = vectors();
    let group = &v["samePersonLink"]["expect"];
    let link: SamePersonLink = serde_json::from_value(group["record"].clone()).unwrap();
    let now = v["meta"]["constants"]["nowMs"].as_i64().unwrap();

    assert_eq!(
        link_sign_payload(&link).unwrap(),
        group["payload"].as_str().unwrap()
    );
    assert_eq!(link_id(&link).unwrap(), group["linkId"].as_str().unwrap());
    assert!(verify_same_person_link(&link, now).is_ok());

    // members < 2 拒绝
    let mut few = link.clone();
    few.members.truncate(1);
    let err = verify_same_person_link(&few, now).unwrap_err();
    assert_eq!(err.kind(), "invalid-link");
}

#[test]
fn same_person_link_membership_binding() {
    let v = vectors();
    let link: SamePersonLink =
        serde_json::from_value(v["samePersonLink"]["expect"]["record"].clone()).unwrap();
    let cred = cred_from(&v["credential.issue"]["expect"]["credential"]);

    // 向量凭证的 linkRef 为 null（未回指）→ 双向索引核对失败
    let err = verify_link_membership(&link, &[&cred]).unwrap_err();
    assert_eq!(err.kind(), "invalid-link");

    // 补上回指后：members[0] 通过，但 members[1] 凭证未给出 → 仍失败
    let mut linked = cred.clone();
    linked.link_ref = Some(link_id(&link).unwrap());
    let err = verify_link_membership(&link, &[&linked]).unwrap_err();
    assert_eq!(err.kind(), "invalid-link");
}

// ------------------------------------------------------------------
// readGate.envelope（holderProof）
// ------------------------------------------------------------------

#[test]
fn read_gate_holder_proof_byte_exact() {
    let v = vectors();
    let group = &v["readGate.envelope"]["expect"];
    let cred = cred_from(&v["credential.issue"]["expect"]["credential"]);
    let cred_id = v["credential.issue"]["expect"]["credId"].as_str().unwrap();
    // orgId / collection / presentedAt 分量值见 gen-community-vectors.mjs readGate 段：
    // orgId = 共同体域 orgId，collection = "hoa:ledger@v1.0.0"，presentedAt = NOW
    let org_id = v["trustDecl"]["expect"]["record"]["orgId"]
        .as_str()
        .unwrap();
    let request_id = group["requestId"].as_str().unwrap();

    let payload = holder_proof_payload(cred_id, request_id, org_id, "hoa:ledger@v1.0.0", NOW);
    assert_eq!(payload, group["holderProofPayload"].as_str().unwrap());

    let proof = &group["holderProof"];
    assert!(proof["credId"].as_str().unwrap() == cred_id);
    assert!(
        verify_holder_proof(
            &serde_json::from_value(proof.clone()).unwrap(),
            &cred.holder.public_key,
            cred_id,
            request_id,
            org_id,
            "hoa:ledger@v1.0.0",
            NOW,
        )
        .is_ok()
    );
}

// ------------------------------------------------------------------
// readGate.declExt（read-gate §2 声明追加 readPolicy；生成器自产组）
// ------------------------------------------------------------------

#[test]
fn read_gate_decl_ext_parse_roundtrip() {
    let v = vectors();
    let record = &v["readGate.declExt"]["expect"]["record"];
    // 声明记录解析：向量 record 无 scope/sensitivity/orgId 键（缺省值兜底），
    // readPolicy 追加字段须完整落地（旧端忽略未知键无损的另一面是新端认得它）
    let decl: spark_core::plugindata::CollectionDeclaration =
        serde_json::from_value(record.clone()).expect("parse declaration with readPolicy");
    let rp = decl.read_policy.as_ref().expect("readPolicy parsed");
    assert_eq!(rp.kind, spark_core::plugindata::ReadPolicyKind::Credential);
    assert_eq!(
        rp.cred_types,
        vec![
            "household-owner".to_string(),
            "resident".to_string()
        ]
    );
    assert_eq!(
        rp.verifier_domain,
        record["readPolicy"]["verifierDomain"].as_str().unwrap()
    );
    assert_eq!(rp.policy_ref, None, "policyRef 缺省 null");
    rp.validate().expect("声明录入校验通过");
    // readPolicy 子对象 canonical 往返逐字节（与生成器 normalizeObject 同口径）：
    // 反序列化 → 再序列化后的 canonical 与向量 record 子对象 canonical 一致
    assert_eq!(
        normalize_object(&serde_json::to_value(rp).unwrap()),
        normalize_object(&record["readPolicy"]),
        "readPolicy 子对象 canonical 往返逐字节"
    );
}

// ------------------------------------------------------------------
// credential.trustTimeline（C2 生成器回填）
// ------------------------------------------------------------------

#[test]
fn trust_timeline_cases() {
    let v = vectors();
    let group = &v["credential.trustTimeline"];
    let decls: Vec<TrustDecl> = group["trustDecls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| serde_json::from_value(d.clone()).unwrap())
        .collect();
    let decl_refs: Vec<&TrustDecl> = decls.iter().collect();

    for case in group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let at = case["at"].as_i64().unwrap();
        let expect_seq = case["expectDeclSeq"].as_u64();
        let picked = trust_decl_at(&decl_refs, at);
        assert_eq!(
            picked.map(|d| d.seq),
            expect_seq,
            "trust_decl_at case {name}"
        );

        if let Some(cred_key) = case["credential"].as_str() {
            let cred = cred_from(&group["credentials"][cred_key]);
            assert!(
                verify_credential_static(&cred, Some(case["credId"].as_str().unwrap())).is_ok(),
                "static verify case {name}"
            );
            let trusted = issuer_trusted_at(
                &decl_refs,
                &cred.issuer.identity,
                &cred.cred_type,
                &cred.method,
                cred.issued_at,
            );
            assert_eq!(
                trusted,
                case["expectTrusted"].as_bool().unwrap(),
                "trust case {name}"
            );
        }
    }
}

// ------------------------------------------------------------------
// readGate.verifyChain（C2 生成器回填；policyRef 求值待 C5）
// ------------------------------------------------------------------

#[test]
fn read_gate_verify_chain_cases() {
    let v = vectors();
    let group = &v["readGate.verifyChain"];
    let ctx = &group["context"];
    let request_id = ctx["requestId"].as_str().unwrap();
    let collection = ctx["collection"].as_str().unwrap();
    let now = ctx["nowMs"].as_i64().unwrap();

    let base_policy = CredentialReadPolicy {
        cred_types: ctx["policy"]["credTypes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect(),
        verifier_domain: ctx["policy"]["verifierDomain"]
            .as_str()
            .unwrap()
            .to_string(),
    };
    let base_decls: Vec<TrustDecl> = ctx["trustDecls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| serde_json::from_value(d.clone()).unwrap())
        .collect();
    let base_revocation: (Vec<RevocationEntry>, RevocationHead) = (
        ctx["revocation"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| serde_json::from_value(e.clone()).unwrap())
            .collect(),
        serde_json::from_value(ctx["revocation"]["head"].clone()).unwrap(),
    );

    for case in group["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let read_auth: ReadAuth = serde_json::from_value(case["readAuth"].clone()).unwrap();

        // case 级覆盖：policy / trustDecls / revocation
        let policy = match &case["policyOverride"] {
            Value::Null => None,
            p => Some(CredentialReadPolicy {
                cred_types: p["credTypes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t.as_str().unwrap().to_string())
                    .collect(),
                verifier_domain: p["verifierDomain"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| base_policy.verifier_domain.clone()),
            }),
        };
        let policy = policy.unwrap_or_else(|| CredentialReadPolicy {
            cred_types: base_policy.cred_types.clone(),
            verifier_domain: base_policy.verifier_domain.clone(),
        });

        let case_decls: Vec<TrustDecl> = match &case["trustDeclsOverride"] {
            Value::Null => base_decls.clone(),
            arr => arr
                .as_array()
                .unwrap()
                .iter()
                .map(|d| serde_json::from_value(d.clone()).unwrap())
                .collect(),
        };
        let decl_refs: Vec<&TrustDecl> = case_decls.iter().collect();

        let (rev_entries, rev_head): (Vec<RevocationEntry>, RevocationHead) = match &case["revocationOverride"]
        {
            Value::Null => base_revocation.clone(),
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
        let revocation_for = move |_issuer: &str| Some((rev_entries.clone(), rev_head.clone()));

        let result = verify_read_auth(
            &read_auth,
            request_id,
            collection,
            &policy,
            &decl_refs,
            &revocation_for,
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
}

// ------------------------------------------------------------------
// 注销检查单环（向量既有组组合的 fail-closed 断言）
// ------------------------------------------------------------------

#[test]
fn not_revoked_proof_full_chain() {
    let v = vectors();
    let entries: Vec<RevocationEntry> = v["credential.revokeChain"]["expect"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| serde_json::from_value(e["entry"].clone()).unwrap())
        .collect();
    let head: RevocationHead =
        serde_json::from_value(v["credential.revHead"]["expect"]["head"].clone()).unwrap();
    let verifier_pubkey = v["meta"]["actors"]["verifier"]["publicKey"]
        .as_str()
        .unwrap();
    let target_cred_id = v["credential.issue"]["expect"]["credId"].as_str().unwrap();

    // 目标 credId 不在列表中 → 未注销证明成立（asOf = NOW+5000，在 ±10min 内）
    assert!(verify_not_revoked(target_cred_id, &entries, &head, verifier_pubkey, NOW).is_ok());
    // 列表中的 credId → Revoked
    let revoked_id = entries[0].cred_id.clone();
    let err = verify_not_revoked(&revoked_id, &entries, &head, verifier_pubkey, NOW).unwrap_err();
    assert_eq!(err.kind(), "revoked");
    // 头承诺过期 → StaleTimestamp（消费方按 asOf 新鲜度取舍的默认 fail-closed 口径）
    let err = verify_not_revoked(
        target_cred_id,
        &entries,
        &head,
        verifier_pubkey,
        NOW + 11 * 60 * 1000,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "stale-timestamp");
}

#[test]
fn error_kind_names_stable() {
    // kind() 是向量 expect 与跨层上报的稳定口径，防无意改动
    assert_eq!(
        CredentialError::InvalidSignature.kind(),
        "invalid-signature"
    );
    assert_eq!(CredentialError::Revoked.kind(), "revoked");
    assert_eq!(
        CredentialError::IssuerNotTrusted.kind(),
        "issuer-not-trusted"
    );
    assert_eq!(
        CredentialError::CredTypeNotAllowed.kind(),
        "cred-type-not-allowed"
    );
    assert_eq!(
        CredentialError::HolderProofInvalid.kind(),
        "holder-proof-invalid"
    );
}
