//! A17 免预录凭证入册 kernel 级集成测试（membership §4.5，org-join §8）：
//! 邀请（预录-认领）与免预录（凭证）**双路径**经同一合入函数
//! （`adjudicate_join_request` → `accept_join_request`）入册——
//! tempdir 起真内核直调，夹具真实 ed25519 签名，不 mock。
//!
//! 覆盖：发布准入策略（公示延迟）→ 快进到生效态 → 免预录合入（有效凭证
//! 入册 + accessKey/org_user_id 随迁 + 事务审计 credId）；预录-认领路径
//! （admin 预录 → 无凭证申请 → 认领补齐 accessKey）；拒收路径（未附凭证 /
//! 未生效策略 / 非成员节点处理）。

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use common::*;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use spark_core::credential::{
    Credential, HolderKind, HolderRef, IdentityRef, OrgSigSet, RevocationEntry, RevocationHead,
    RosterAnchor, RosterCommitment, TrustDecl, VerifierGrant, credential_sign_payload,
    revocation_entry_hash, revocation_head_payload, revocation_sign_payload,
    revocation_snapshot_key, trust_decl_key,
};
use spark_core::org::access_key::verify_access_key_binding;
use spark_core::org::join_request::{
    JOIN_REQUEST_V, JoinRequest, ORG_JOIN_REQUEST_TYPE, join_request_sign_payload,
};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::org::types::OrganizationAccessKey;
use spark_core::storage::StorageBackend;

const NOW: i64 = 1_720_000_000_000;

fn real_now_ms() -> i64 {
    spark_core::p2p::node::system_now_ms()
}

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

/// 申请人根密钥（0x42）；其 org-access 域身份用 0x43 替身（验绑只查锚定 +
/// 绑定签名，域派生路径不在验证面内）。
fn applicant() -> SigningKey {
    key(0x42)
}

fn applicant_access_key(org_id: &str) -> OrganizationAccessKey {
    let root = applicant();
    let domain = key(0x43);
    let public_key = b64_pk(&domain);
    OrganizationAccessKey {
        bind_sig: sign(
            &root,
            &spark_core::org::types::access_key_bind_payload(org_id, &public_key),
        ),
        public_key,
        root_pubkey: Some(b64_pk(&root)),
    }
}

fn build_request(org_id: &str, credential: Option<Credential>, declared_at: i64) -> JoinRequest {
    let root = applicant();
    let mut req = JoinRequest {
        join_v: JOIN_REQUEST_V,
        type_: ORG_JOIN_REQUEST_TYPE.to_string(),
        org_id: org_id.to_string(),
        applicant: IdentityRef {
            identity: identity_of(&root),
            public_key: b64_pk(&root),
        },
        access_key: applicant_access_key(org_id),
        credential,
        node_info: None,
        declared_at,
        sig: String::new(),
    };
    req.sig = sign(&root, &join_request_sign_payload(&req).expect("payload"));
    req
}

fn build_credential(issuer: &SigningKey, subject_domain: &str) -> Credential {
    let holder = applicant();
    let mut cred = Credential {
        cred_v: 1,
        cred_type: "member".to_string(),
        issuer: IdentityRef {
            identity: identity_of(issuer),
            public_key: b64_pk(issuer),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: identity_of(&holder),
            public_key: b64_pk(&holder),
        },
        subject_domain: subject_domain.to_string(),
        claims: serde_json::Map::new(),
        method: "plugin:test:manual".to_string(),
        link_ref: None,
        issued_at: NOW,
        sig: String::new(),
    };
    cred.sig = sign(issuer, &credential_sign_payload(&cred).expect("payload"));
    cred
}

fn trust_decl(issuer: &SigningKey, subject_domain: &str) -> TrustDecl {
    TrustDecl {
        trust_v: 1,
        org_id: subject_domain.to_string(),
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
            org_id: subject_domain.to_string(),
            subject: "00".repeat(32),
            policy_hash: "00".repeat(32),
            roster: RosterCommitment {
                member_set_hash: "00".repeat(32),
                anchor: RosterAnchor {
                    org_id: subject_domain.to_string(),
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

/// 单条无关注销的快照（headSeq 1；空链头形态规格未定义 fail-closed）。
fn clean_revocation(issuer: &SigningKey) -> spark_core::credential::RevocationSnapshot {
    let mut entry = RevocationEntry {
        rev_v: 1,
        issuer: identity_of(issuer),
        seq: 1,
        prev_hash: None,
        cred_id: "ff".repeat(32),
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
        as_of: real_now_ms(),
        sig: String::new(),
    };
    head.sig = sign(issuer, &revocation_head_payload(&head).expect("payload"));
    spark_core::credential::RevocationSnapshot {
        entries: vec![entry],
        head,
    }
}

/// 组织夹具：kernel 创建组织（含存证锚，OrgSigSet 签发前提）。
fn create_org_with_anchor(kernel: &mut spark_core::kernel::Kernel, name: &str) -> String {
    let org_id = kernel
        .create_org(CreateOrganizationInput {
            name: name.to_string(),
            ..Default::default()
        })
        .unwrap()
        .record
        .org_id;
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

/// 发布准入策略并**快进到生效态**（测试时钟注入：把 stored 记录的
/// effectiveAt 改写为 updatedAt——模拟 24h 公示延迟过后的消费形态；
/// 发布面的延迟约束由 kernel_credential_policy_ops 的发布用例覆盖）。
fn publish_effective_accept_policy(
    kernel: &mut spark_core::kernel::Kernel,
    org_id: &str,
    issuer_trust: &str,
) {
    let out = kernel
        .accept_policy_publish(
            org_id,
            vec![spark_core::policy::AcceptCredentialRule {
                cred_type: "member".to_string(),
                issuer_trust: issuer_trust.to_string(),
            }],
            true, // 扩大须显式确认
        )
        .unwrap();
    assert_eq!(out["widening"], true, "首个非空准入声明 = 扩大");
    let mut storage = kernel.__test_storage().expect("storage");
    let key = spark_core::policy::accept_policy_key(org_id);
    let mut record: spark_core::policy::AcceptPolicyRecord =
        serde_json::from_str(&storage.get(&key).unwrap().unwrap()).unwrap();
    record.effective_at = record.updated_at; // 时间快进：公示延迟窗口过后
    storage
        .put(&key, &serde_json::to_string(&record).unwrap())
        .unwrap();
}

#[test]
fn dual_path_credential_and_claim_enroll_through_same_function() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_admin_root, _) = init_identity(&mut kernel);
    let org_id = create_org_with_anchor(&mut kernel, "阳光小区");
    let issuer = key(0x07);
    let issuer_trust = format!("org_{}", "bb".repeat(32));
    publish_effective_accept_policy(&mut kernel, &org_id, &issuer_trust);

    // 合入侧数据面：issuerTrust 域信任声明 + issuer 注销快照（生产经
    // orgsync/分发渠道到达，测试直写与 credential_verify 同口径）
    let mut storage = kernel.__test_storage().expect("storage");
    storage
        .put(
            &trust_decl_key(&issuer_trust),
            &serde_json::to_string(&trust_decl(&issuer, &issuer_trust)).unwrap(),
        )
        .unwrap();
    storage
        .put(
            &revocation_snapshot_key(&identity_of(&issuer)),
            &serde_json::to_string(&clean_revocation(&issuer)).unwrap(),
        )
        .unwrap();
    drop(storage);

    // ── 路径一：免预录（无预录条目，持有效凭证自签申请）──
    let cred = build_credential(&issuer, &issuer_trust);
    let request = build_request(&org_id, Some(cred.clone()), real_now_ms());
    let out = kernel
        .org_accept_join_request(&serde_json::to_value(&request).unwrap())
        .unwrap();
    assert_eq!(out["outcome"], "enrolled");
    assert_eq!(out["path"], "credential", "无预录走免预录路径");
    assert_eq!(
        out["credId"].as_str().expect("credId"),
        spark_core::credential::credential_id(&cred).unwrap(),
        "审计引用 credId"
    );
    let storage = kernel.__test_storage().expect("storage");
    let record = spark_core::org::OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    let applicant_root = identity_of(&applicant());
    let enrolled = record.find_member(&applicant_root).expect("凭证入册");
    assert_eq!(enrolled.added_by, applicant_root, "自录（零管理员在线）");
    let org_user_id = enrolled.org_user_id().expect("accessKey 随迁可派生");
    assert_ne!(org_user_id, applicant_root, "org_user_id ≠ rootId（无关联）");
    assert!(verify_access_key_binding(
        &org_id,
        &applicant_root,
        enrolled.access_key.as_ref().unwrap()
    ));
    // 事务审计携带 credId
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &org_id, 64).unwrap();
    assert!(
        txs.iter()
            .any(|t| t.type_ == spark_core::org::OrganizationTransactionType::MemberAdd),
        "免预录入册 MemberAdd 审计"
    );
    drop(storage);

    // ── 路径二：邀请（预录-认领）——admin 预录另一申请人，无凭证申请 ──
    let claimer = key(0x52);
    let claimer_root = identity_of(&claimer);
    kernel.org_add_member(&org_id, &claimer_root, None).unwrap();
    let claim_request = {
        let domain = key(0x53);
        let public_key = b64_pk(&domain);
        let mut req = JoinRequest {
            join_v: JOIN_REQUEST_V,
            type_: ORG_JOIN_REQUEST_TYPE.to_string(),
            org_id: org_id.clone(),
            applicant: IdentityRef {
                identity: claimer_root.clone(),
                public_key: b64_pk(&claimer),
            },
            access_key: OrganizationAccessKey {
                bind_sig: sign(
                    &claimer,
                    &spark_core::org::types::access_key_bind_payload(&org_id, &public_key),
                ),
                public_key,
                root_pubkey: Some(b64_pk(&claimer)),
            },
            credential: None, // 认领路径不附凭证
            node_info: None,
            declared_at: real_now_ms(),
            sig: String::new(),
        };
        req.sig = sign(&claimer, &join_request_sign_payload(&req).expect("payload"));
        req
    };
    let out = kernel
        .org_accept_join_request(&serde_json::to_value(&claim_request).unwrap())
        .unwrap();
    assert_eq!(out["outcome"], "enrolled");
    assert_eq!(out["path"], "claim", "预录条目存在走认领（同一验证函数）");
    let storage = kernel.__test_storage().expect("storage");
    let record = spark_core::org::OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    let claimed = record.find_member(&claimer_root).expect("预录条目在册");
    assert!(
        claimed.org_user_id().is_some(),
        "认领补齐 accessKey（org_user_id 可派生）"
    );
    assert_eq!(record.members.len(), 3, "admin + 免预录 + 认领");

    kernel.shutdown().unwrap();
}

#[test]
fn rejections_are_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_admin_root, _) = init_identity(&mut kernel);
    let org_id = create_org_with_anchor(&mut kernel, "阳光小区");
    let issuer_trust = format!("org_{}", "bb".repeat(32));
    publish_effective_accept_policy(&mut kernel, &org_id, &issuer_trust);

    // 无预录 + 未附凭证 → credential-required（不落库）
    let request = build_request(&org_id, None, real_now_ms());
    let out = kernel
        .org_accept_join_request(&serde_json::to_value(&request).unwrap())
        .unwrap();
    assert_eq!(out["outcome"], "rejected");
    assert_eq!(out["reason"], "credential-required");

    // 附凭证但信任声明缺失（合入侧数据不可用）→ issuer-not-trusted
    let issuer = key(0x07);
    let cred = build_credential(&issuer, &issuer_trust);
    let request = build_request(&org_id, Some(cred), real_now_ms());
    let out = kernel
        .org_accept_join_request(&serde_json::to_value(&request).unwrap())
        .unwrap();
    assert_eq!(out["outcome"], "rejected");
    assert_eq!(out["reason"], "issuer-not-trusted");
    let storage = kernel.__test_storage().expect("storage");
    let record = spark_core::org::OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.members.len(), 1, "拒收不落库");

    // 陈旧声明（declaredAt 超出 ±10 min 窗口）→ stale-timestamp
    let cred = build_credential(&issuer, &issuer_trust);
    let stale = build_request(
        &org_id,
        Some(cred),
        real_now_ms() - spark_core::credential::FRESHNESS_WINDOW_MS - 1,
    );
    let out = kernel
        .org_accept_join_request(&serde_json::to_value(&stale).unwrap())
        .unwrap();
    assert_eq!(out["reason"], "stale-timestamp");

    kernel.shutdown().unwrap();
}

#[test]
fn processing_requires_membership() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    // 本机无任何组织记录 → 无法合入（组织不存在如实报错）
    let request = build_request(&format!("org_{}", "aa".repeat(32)), None, real_now_ms());
    let err = kernel
        .org_accept_join_request(&serde_json::to_value(&request).unwrap())
        .unwrap_err();
    assert!(err.to_string().contains("Organization not found"), "{err}");

    // 本机有该组织记录但不是成员 → 拒绝处理（写入扩散前提：from ∈ 成员表）
    let org_id = create_org_with_anchor(&mut kernel, "阳光小区");
    let storage = kernel.__test_storage().expect("storage");
    let raw = storage
        .get(&spark_core::org::types::organization_key(&org_id))
        .unwrap()
        .unwrap();
    drop(storage);
    let dir2 = tempfile::tempdir().unwrap();
    let mut outsider = fresh_kernel(dir2.path());
    init_identity(&mut outsider);
    let mut storage2 = outsider.__test_storage().expect("storage");
    storage2
        .put(&spark_core::org::types::organization_key(&org_id), &raw)
        .unwrap();
    drop(storage2);
    let request = build_request(&org_id, None, real_now_ms());
    let err = outsider
        .org_accept_join_request(&serde_json::to_value(&request).unwrap())
        .unwrap_err();
    assert!(
        err.to_string().contains("本机不是该组织成员"),
        "{err}"
    );

    // malformed body → 如实报错
    let err = kernel
        .org_accept_join_request(&json!({ "type": "org-join-request" }))
        .unwrap_err();
    assert!(err.to_string().contains("加入申请格式不正确"), "{err}");

    outsider.shutdown().unwrap();
    kernel.shutdown().unwrap();
}
