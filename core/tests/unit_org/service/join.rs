//! `OrganizationService::accept_join_request` 单元测试（A17，org-join §8）：
//! 加入声明合入的存储接线——免预录入册（whole + per-member 双写、accessKey
//! 随迁、事务审计 credId、幂等重放退化为认领）、预录-认领（补 accessKey
//! 写一次）、拒收不落库（未生效策略 / 未附凭证）。夹具真实 ed25519 签名。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest as _, Sha256};

use spark_core::credential::{
    Credential, HolderKind, HolderRef, IdentityRef, OrgSigSet, RevocationHead, RevocationSnapshot,
    RosterAnchor, RosterCommitment, TrustDecl, VerifierGrant, credential_id,
    credential_sign_payload, revocation_head_payload, revocation_snapshot_key, trust_decl_key,
};
use spark_core::org::join_request::{
    JOIN_REQUEST_V, JoinPath, JoinRequest, ORG_JOIN_REQUEST_TYPE, join_request_sign_payload,
};
use spark_core::org::service::{JoinOutcome, OrganizationService};
use spark_core::org::tx::{OrganizationTransactionType, list_organization_transactions};
use spark_core::org::types::{
    OrganizationAccessKey, OrganizationMember, OrganizationRecord, OrganizationRole, org_member_key,
};
use spark_core::policy::{AcceptCredentialRule, AcceptPolicyRecord, accept_policy_key};
use spark_core::storage::{MemoryStorage, StorageBackend};

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

fn member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role,
        joined_at: 1000,
        added_by: "creator".to_string(),
        ..Default::default()
    }
}

/// 组织夹具：admin 一人在册；`pre_register_applicant` 控制是否把申请人
/// （0x42）预录进去。
fn setup(pre_register_applicant: bool) -> MemoryStorage {
    let mut storage = MemoryStorage::new();
    let admin = identity_of(&key(0x01));
    let mut members = vec![member(&admin, OrganizationRole::Admin)];
    if pre_register_applicant {
        members.push(member(&identity_of(&key(0x42)), OrganizationRole::Member));
    }
    let record = OrganizationRecord {
        org_id: org_id(),
        name: "t".to_string(),
        created_at: 1000,
        created_by: admin.clone(),
        updated_at: 1000,
        members,
        ..Default::default()
    };
    OrganizationService::save_record(&mut storage, &record).unwrap();
    storage
}

fn access_key_of(root: &SigningKey) -> OrganizationAccessKey {
    let domain = key(0x43);
    let public_key = b64_pk(&domain);
    OrganizationAccessKey {
        bind_sig: sign(
            root,
            &spark_core::org::types::access_key_bind_payload(&org_id(), &public_key),
        ),
        public_key,
        root_pubkey: Some(b64_pk(root)),
    }
}

fn build_request(credential: Option<Credential>) -> JoinRequest {
    let root = key(0x42);
    let mut req = JoinRequest {
        join_v: JOIN_REQUEST_V,
        type_: ORG_JOIN_REQUEST_TYPE.to_string(),
        org_id: org_id(),
        applicant: IdentityRef {
            identity: identity_of(&root),
            public_key: b64_pk(&root),
        },
        access_key: access_key_of(&root),
        credential,
        node_info: None,
        declared_at: NOW,
        sig: String::new(),
    };
    req.sig = sign(&root, &join_request_sign_payload(&req).expect("payload"));
    req
}

fn build_credential(issuer: &SigningKey, holder: &SigningKey) -> Credential {
    let mut cred = Credential {
        cred_v: 1,
        cred_type: "member".to_string(),
        issuer: IdentityRef {
            identity: identity_of(issuer),
            public_key: b64_pk(issuer),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: identity_of(holder),
            public_key: b64_pk(holder),
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

/// 单条无关注销的快照（headSeq 1，注销的是别的 credId）：合法「未注销」
/// 证明——空链（headSeq 0）头形态规格未定义，fail-closed 拒绝。
fn empty_revocation(issuer: &SigningKey) -> RevocationSnapshot {
    let mut entry = spark_core::credential::RevocationEntry {
        rev_v: 1,
        issuer: identity_of(issuer),
        seq: 1,
        prev_hash: None,
        cred_id: "ff".repeat(32),
        revoked_at: NOW,
        reason: None,
        sig: String::new(),
    };
    entry.sig = sign(
        issuer,
        &spark_core::credential::revocation_sign_payload(&entry).expect("payload"),
    );
    let mut head = RevocationHead {
        rev_head_v: 1,
        issuer: identity_of(issuer),
        head_seq: 1,
        head_hash: spark_core::credential::revocation_entry_hash(&entry).expect("entryHash"),
        as_of: NOW,
        sig: String::new(),
    };
    head.sig = sign(issuer, &revocation_head_payload(&head).expect("payload"));
    RevocationSnapshot {
        entries: vec![entry],
        head,
    }
}

fn accept_policy(effective: bool) -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        accept_v: spark_core::policy::ACCEPT_POLICY_V,
        org_id: org_id(),
        accept_credentials: vec![AcceptCredentialRule {
            cred_type: "member".to_string(),
            issuer_trust: trust_domain(),
        }],
        version: 1,
        updated_at: NOW,
        effective_at: if effective { NOW } else { NOW + 86_400_000 },
        sig_set: None,
    }
}

/// 免预录全链夹具：组织 + 生效策略 + 信任声明 + 注销快照入库。
fn setup_credential_path() -> (MemoryStorage, Credential) {
    let mut storage = setup(false);
    let issuer = key(0x07);
    storage
        .put(
            &accept_policy_key(&org_id()),
            &serde_json::to_string(&accept_policy(true)).unwrap(),
        )
        .unwrap();
    storage
        .put(
            &trust_decl_key(&trust_domain()),
            &serde_json::to_string(&trust_decl(&issuer)).unwrap(),
        )
        .unwrap();
    storage
        .put(
            &revocation_snapshot_key(&identity_of(&issuer)),
            &serde_json::to_string(&empty_revocation(&issuer)).unwrap(),
        )
        .unwrap();
    (storage, build_credential(&issuer, &key(0x42)))
}

#[test]
fn credential_path_enrolls_new_member_dual_written() {
    let (mut storage, cred) = setup_credential_path();
    let req = build_request(Some(cred.clone()));
    let outcome = OrganizationService::accept_join_request(
        &mut storage,
        &crate::test_io_lock(),
        &org_id(),
        &req,
        NOW,
    )
    .unwrap();
    let JoinOutcome::Enrolled { path, cred_id } = outcome else {
        panic!("有效凭证应受理: {outcome:?}");
    };
    assert_eq!(path, JoinPath::Credential);
    assert_eq!(
        cred_id.as_deref(),
        Some(credential_id(&cred).expect("credId").as_str())
    );

    // whole + per-member 条目双写（原子段原语）
    let applicant = identity_of(&key(0x42));
    let record = OrganizationService::get_record(&storage, &org_id())
        .unwrap()
        .unwrap();
    let enrolled = record.find_member(&applicant).expect("已入册");
    assert_eq!(enrolled.role, OrganizationRole::Member);
    assert_eq!(enrolled.added_by, applicant, "自录（免预录）");
    assert!(enrolled.access_key.is_some(), "入册条目携带验过的 accessKey");
    assert!(enrolled.org_user_id().is_some(), "org_user_id 可派生");
    let entry_raw = storage
        .get(&org_member_key(&org_id(), &applicant))
        .unwrap()
        .expect("成员条目双写");
    let entry: OrganizationMember = serde_json::from_str(&entry_raw).unwrap();
    assert_eq!(&entry, enrolled, "条目与装配视图逐字段等价");

    // 事务审计携带 credId
    let txs = list_organization_transactions(&storage, &org_id(), 64).unwrap();
    let tx = txs
        .iter()
        .find(|t| t.type_ == OrganizationTransactionType::MemberAdd);
    assert!(tx.is_some(), "MemberAdd 审计已追加");

    // 幂等重放：重复申请不再变更（已在册 → 认领退化，无写）
    let outcome = OrganizationService::accept_join_request(
        &mut storage,
        &crate::test_io_lock(),
        &org_id(),
        &req,
        NOW,
    )
    .unwrap();
    assert!(
        matches!(outcome, JoinOutcome::Enrolled { path: JoinPath::Claim, .. }),
        "重复申请幂等（认领退化）: {outcome:?}"
    );
    assert_eq!(
        OrganizationService::get_record(&storage, &org_id())
            .unwrap()
            .unwrap()
            .members
            .len(),
        2,
        "无重复条目"
    );
}

#[test]
fn claim_path_fills_access_key_on_preregistered_entry() {
    let mut storage = setup(true); // 申请人已预录（无 accessKey）
    let req = build_request(None);
    let outcome = OrganizationService::accept_join_request(
        &mut storage,
        &crate::test_io_lock(),
        &org_id(),
        &req,
        NOW,
    )
    .unwrap();
    assert!(
        matches!(outcome, JoinOutcome::Enrolled { path: JoinPath::Claim, cred_id: None }),
        "预录走认领: {outcome:?}"
    );
    let record = OrganizationService::get_record(&storage, &org_id())
        .unwrap()
        .unwrap();
    let claimed = record
        .find_member(&identity_of(&key(0x42)))
        .expect("预录条目在册");
    assert!(claimed.access_key.is_some(), "认领补齐 accessKey");
    assert!(claimed.org_user_id().is_some());
}

#[test]
fn rejections_write_nothing() {
    // 未生效策略（公示延迟窗口内）→ 拒收不落库
    let (mut storage, cred) = setup_credential_path();
    storage
        .put(
            &accept_policy_key(&org_id()),
            &serde_json::to_string(&accept_policy(false)).unwrap(),
        )
        .unwrap();
    let req = build_request(Some(cred));
    let outcome = OrganizationService::accept_join_request(
        &mut storage,
        &crate::test_io_lock(),
        &org_id(),
        &req,
        NOW,
    )
    .unwrap();
    assert_eq!(
        outcome,
        JoinOutcome::Rejected("accept-policy-missing".to_string())
    );
    let record = OrganizationService::get_record(&storage, &org_id())
        .unwrap()
        .unwrap();
    assert_eq!(record.members.len(), 1, "拒收不落库");

    // 无策略 + 无预录 + 未附凭证 → credential-required
    let mut storage = setup(false);
    let req = build_request(None);
    let outcome = OrganizationService::accept_join_request(
        &mut storage,
        &crate::test_io_lock(),
        &org_id(),
        &req,
        NOW,
    )
    .unwrap();
    assert_eq!(
        outcome,
        JoinOutcome::Rejected("credential-required".to_string())
    );
}
