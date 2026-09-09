//! `org::join_request` 单元测试（A17，org-join §8.2）：加入声明的合入侧
//! 双路径合一验证（`adjudicate_join_request`）——认领受理 / 有效凭证受理 /
//! 免预录必败矩阵（已注销 / 签发者不受信任 / 类型不匹配 / 策略缺失 / 未附
//! 凭证 / 持有人不符 / 注销快照缺失）与信封级拒绝（签名篡改 / 陈旧 /
//! accessKey 张冠李戴 / 跨组织转投）。夹具真实 ed25519 签名，不 mock。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest as _, Sha256};

use spark_core::credential::{
    Credential, FRESHNESS_WINDOW_MS, HolderKind, HolderRef, IdentityRef, OrgSigSet,
    RevocationEntry, RevocationHead, RevocationView, RosterAnchor, RosterCommitment, TrustDecl,
    VerifierGrant, credential_id, credential_sign_payload, revocation_entry_hash,
    revocation_head_payload, revocation_sign_payload,
};
use spark_core::org::join_request::{
    JOIN_REQUEST_V, JoinAdmission, JoinPath, JoinRejection, JoinRequest, ORG_JOIN_REQUEST_TYPE,
    adjudicate_join_request, join_request_sign_payload,
};
use spark_core::org::types::OrganizationAccessKey;
use spark_core::policy::{AcceptCredentialRule, AcceptPolicyRecord};

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

/// 申请人根密钥（0x42）与其 org-access 域身份（0x43 替身——验绑只查
/// 锚定 + 绑定签名，域派生路径不在验证面内）。
fn applicant() -> SigningKey {
    key(0x42)
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

fn issuer() -> SigningKey {
    key(0x07)
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

/// 单条无关注销的链（headSeq 1，注销的是别的 credId）：合法「未注销」
/// 证明——空链（headSeq 0）头形态规格未定义，fail-closed 拒绝
/// （revocation.rs verify_revocation_head）。
fn empty_revocation(issuer: &SigningKey) -> (Vec<RevocationEntry>, RevocationHead) {
    revoking_revocation(issuer, &"ff".repeat(32))
}

/// 含目标 credId 的注销链（headSeq 1）。
fn revoking_revocation(
    issuer: &SigningKey,
    cred_id: &str,
) -> (Vec<RevocationEntry>, RevocationHead) {
    let mut entry = RevocationEntry {
        rev_v: 1,
        issuer: identity_of(issuer),
        seq: 1,
        prev_hash: None,
        cred_id: cred_id.to_string(),
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
    (vec![entry], head)
}

fn policy() -> AcceptPolicyRecord {
    AcceptPolicyRecord {
        accept_v: spark_core::policy::ACCEPT_POLICY_V,
        org_id: org_id(),
        accept_credentials: vec![AcceptCredentialRule {
            cred_type: "member".to_string(),
            issuer_trust: trust_domain(),
        }],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None,
    }
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
        access_key: access_key_of(&root),
        credential,
        node_info: None,
        declared_at: NOW,
        sig: String::new(),
    };
    req.sig = sign(&root, &join_request_sign_payload(&req).expect("payload"));
    req
}

struct Ctx {
    decls: Vec<TrustDecl>,
    entries: Vec<RevocationEntry>,
    head: RevocationHead,
    policy: AcceptPolicyRecord,
}

fn fixture() -> Ctx {
    let (entries, head) = empty_revocation(&issuer());
    Ctx {
        decls: vec![trust_decl(&issuer())],
        entries,
        head,
        policy: policy(),
    }
}

fn adjudicate(
    ctx: &Ctx,
    request: &JoinRequest,
    pre_registered: bool,
) -> Result<JoinAdmission, JoinRejection> {
    let decls: Vec<&TrustDecl> = ctx.decls.iter().collect();
    let view = RevocationView {
        entries: &ctx.entries,
        head: &ctx.head,
    };
    adjudicate_join_request(
        &org_id(),
        request,
        pre_registered,
        Some(&ctx.policy),
        &decls,
        Some(&view),
        NOW,
    )
}

// ── 双路径受理 ──────────────────────────────────────────────────────

#[test]
fn claim_path_accepts_without_credential() {
    let ctx = fixture();
    let req = build_request(None);
    let admission = adjudicate(&ctx, &req, true).expect("预录条目存在 → 认领受理");
    assert_eq!(admission.path, JoinPath::Claim);
    assert_eq!(admission.cred_id, None);
}

#[test]
fn credential_path_accepts_valid_credential() {
    let ctx = fixture();
    let cred = build_credential(&issuer(), &applicant());
    let req = build_request(Some(cred));
    let admission = adjudicate(&ctx, &req, false).expect("有效凭证 → 免预录受理");
    assert_eq!(admission.path, JoinPath::Credential);
    assert_eq!(admission.cred_id.as_ref().map(|s| s.len()), Some(64));
}

#[test]
fn pre_registered_takes_claim_path_even_with_credential() {
    let ctx = fixture();
    let cred = build_credential(&issuer(), &applicant());
    let req = build_request(Some(cred));
    let admission = adjudicate(&ctx, &req, true).expect("预录优先走认领");
    assert_eq!(admission.path, JoinPath::Claim, "预录条目存在时不消费凭证");
}

// ── 免预录必败矩阵 ──────────────────────────────────────────────────

#[test]
fn revoked_credential_rejected() {
    let cred = build_credential(&issuer(), &applicant());
    let cred_id = credential_id(&cred).expect("credId");
    let (entries, head) = revoking_revocation(&issuer(), &cred_id);
    let ctx = Ctx {
        entries,
        head,
        ..fixture()
    };
    let req = build_request(Some(cred));
    let err = adjudicate(&ctx, &req, false).unwrap_err();
    assert_eq!(err.kind(), "revoked");
}

#[test]
fn untrusted_issuer_rejected() {
    let stranger = key(0x09); // 信任声明外的签发者
    let cred = build_credential(&stranger, &applicant());
    let req = build_request(Some(cred));
    let err = adjudicate(&fixture(), &req, false).unwrap_err();
    assert_eq!(err.kind(), "issuer-not-trusted");
}

#[test]
fn cred_type_not_accepted_rejected() {
    let mut cred = build_credential(&issuer(), &applicant());
    cred.cred_type = "resident".to_string(); // 策略只接受 member
    cred.sig = sign(&issuer(), &credential_sign_payload(&cred).expect("payload"));
    let req = build_request(Some(cred));
    let err = adjudicate(&fixture(), &req, false).unwrap_err();
    assert_eq!(err.kind(), "cred-type-not-accepted");
}

#[test]
fn missing_policy_or_credential_rejected() {
    let ctx = fixture();
    // 无预录 + 未附凭证
    let req = build_request(None);
    let err = adjudicate(&ctx, &req, false).unwrap_err();
    assert_eq!(err.kind(), "credential-required");

    // 附凭证但无生效策略
    let cred = build_credential(&issuer(), &applicant());
    let req = build_request(Some(cred));
    let decls: Vec<&TrustDecl> = ctx.decls.iter().collect();
    let view = RevocationView {
        entries: &ctx.entries,
        head: &ctx.head,
    };
    let err = adjudicate_join_request(&org_id(), &req, false, None, &decls, Some(&view), NOW)
        .unwrap_err();
    assert_eq!(err.kind(), "accept-policy-missing");
}

#[test]
fn holder_mismatch_and_revocation_unavailable_rejected() {
    let ctx = fixture();
    // 凭证持有人不是申请人（为别人凭证申请入册）
    let other = key(0x55);
    let cred = build_credential(&issuer(), &other);
    let req = build_request(Some(cred));
    let err = adjudicate(&ctx, &req, false).unwrap_err();
    assert_eq!(err.kind(), "identity-mismatch");

    // 注销快照缺失 → fail-closed
    let cred = build_credential(&issuer(), &applicant());
    let req = build_request(Some(cred));
    let decls: Vec<&TrustDecl> = ctx.decls.iter().collect();
    let err = adjudicate_join_request(
        &org_id(),
        &req,
        false,
        Some(&ctx.policy),
        &decls,
        None,
        NOW,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "revocation-unavailable");
}

#[test]
fn envelope_level_rejections() {
    let ctx = fixture();
    let cred = build_credential(&issuer(), &applicant());
    let good = build_request(Some(cred.clone()));

    // 声明签名篡改
    let mut bad_sig = good.clone();
    bad_sig.declared_at = NOW + 1; // 改动签名内容不重签
    assert_eq!(
        adjudicate(&ctx, &bad_sig, false).unwrap_err().kind(),
        "invalid-signature"
    );
    // 陈旧声明
    let root = applicant();
    let mut stale = build_request(Some(cred.clone()));
    stale.declared_at = NOW - FRESHNESS_WINDOW_MS - 1;
    stale.sig = sign(&root, &join_request_sign_payload(&stale).expect("payload"));
    assert_eq!(
        adjudicate(&ctx, &stale, false).unwrap_err().kind(),
        "stale-timestamp"
    );
    // accessKey 张冠李戴（别人的 accessKey）
    let mut bad_key = build_request(Some(cred.clone()));
    bad_key.access_key = access_key_of(&key(0x77));
    bad_key.sig = sign(&root, &join_request_sign_payload(&bad_key).expect("payload"));
    assert_eq!(
        adjudicate(&ctx, &bad_key, false).unwrap_err().kind(),
        "identity-mismatch"
    );
    // 目标组织不符（防跨组织转投）
    assert_eq!(
        adjudicate_join_request(
            &format!("org_{}", "cc".repeat(32)),
            &good,
            false,
            Some(&ctx.policy),
            &ctx.decls.iter().collect::<Vec<_>>(),
            None,
            NOW,
        )
        .unwrap_err()
        .kind(),
        "invalid-structure"
    );
}
