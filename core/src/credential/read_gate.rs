//! 读授权门禁对接（read-gate §3–§4）：holderProof 载荷与 readAuth 段验证。
//!
//! readAuth 段随 orgq-req body 入 dm 信封既有签名面（信封由 from root 身份签，
//! p2p-dm），本模块不重复信封验签——只验证 readAuth 段内部：结构/新鲜度、
//! 逐凭证验证链（credential §6 第 1–5 步）、credType/subjectDomain 匹配
//! readPolicy、holderProof 逐凭证绑定本次请求（防重放转投）。
//!
//! policyRef 的求值（A15 起 = 开放声明求值，policy §8）在调用方
//! （`kernel/inbound_dm/orgq.rs` 第 5 步 `disclosure_allows`）执行，本模块
//! 只承载 readAuth 段验证（§4 第 1–4 步 + A15 城门名册回查），不消费
//! policyRef。

use serde_json::json;

use super::credential::{RevocationView, is_fresh, is_valid_hash, verify_credential_chain};
use super::error::{CredentialError, Result};
use super::types::{Credential, HolderProof, ReadAuth, RevocationEntry, RevocationHead, TrustDecl};
use crate::evidence::normalize_object;
use crate::identity::verify_ed25519_signature;

/// holderProof 签名载荷（read-gate §3）：
/// `canonical({credId, requestId, orgId, collection, presentedAt})`——绑定本次
/// 请求（requestId 与目标集合），防重放转投。
pub fn holder_proof_payload(
    cred_id: &str,
    request_id: &str,
    org_id: &str,
    collection: &str,
    presented_at: i64,
) -> String {
    normalize_object(&json!({
        "credId": cred_id,
        "requestId": request_id,
        "orgId": org_id,
        "collection": collection,
        "presentedAt": presented_at,
    }))
}

/// readPolicy `kind == "credential"` 的匹配口径（read-gate §2；只含本模块
/// 消费的字段，policyRef 归 C5 故不在此）。
pub struct CredentialReadPolicy {
    /// 放行的凭证类型集（任一匹配）。
    pub cred_types: Vec<String>,
    /// 验证人信任声明所在域（凭证 subjectDomain 必须等于它）。
    pub verifier_domain: String,
}

// ── 查询发起侧（read-gate §3 readAuth 构造） ─────────────────────────────

/// 呈现候选筛选（read-gate §3 发起侧）：从本地持有凭证中选出匹配 readPolicy
/// 且 holder 为指定域身份的凭证——`credType ∈ credTypes`、`subjectDomain ==
/// verifierDomain`、`holder.publicKey == holder_public_key`（签不出持有证明
/// 的凭证呈现了也必被拒）。返回 `(credId, 凭证)`，credId 以**内容复算**为准
/// （存储键名不置信）；复算失败的损坏记录跳过。
pub fn select_presentable_credentials(
    held: impl IntoIterator<Item = Credential>,
    policy: &CredentialReadPolicy,
    holder_public_key: &str,
) -> Vec<(String, Credential)> {
    held.into_iter()
        .filter(|cred| {
            policy.cred_types.iter().any(|t| t == &cred.cred_type)
                && cred.subject_domain == policy.verifier_domain
                && cred.holder.public_key == holder_public_key
        })
        .filter_map(|cred| {
            super::credential::credential_id(&cred)
                .ok()
                .map(|id| (id, cred))
        })
        .collect()
}

/// 构造 readAuth 呈现段（read-gate §3）：holderProof 逐凭证绑定本次请求
/// （载荷 orgId = 各凭证 subjectDomain，与验证链 `verify_holder_proof` 同
/// 口径）；签名由 `sign` 闭包完成（域私钥属装配层，不入纯逻辑）。
///
/// 候选集为空或任一凭证签名不可用 → `None`（整体不呈现——半套呈现必被
/// 门禁 fail-closed 拒绝，不如按「未呈现」维持现状语义）。
pub fn build_read_auth(
    presentable: &[(String, Credential)],
    request_id: &str,
    collection: &str,
    presented_at: i64,
    sign: &dyn Fn(&str) -> Option<String>,
) -> Option<ReadAuth> {
    if presentable.is_empty() {
        return None;
    }
    let mut credentials = Vec::with_capacity(presentable.len());
    let mut holder_proofs = Vec::with_capacity(presentable.len());
    for (cred_id, cred) in presentable {
        let payload = holder_proof_payload(
            cred_id,
            request_id,
            &cred.subject_domain,
            collection,
            presented_at,
        );
        let sig = sign(&payload)?;
        credentials.push(cred.clone());
        holder_proofs.push(HolderProof {
            cred_id: cred_id.clone(),
            sig,
        });
    }
    Some(ReadAuth {
        gate_v: 1,
        credentials,
        holder_proofs,
        presented_at,
    })
}

/// 注销证明快照的本地存储键前缀（`cred:rev:{issuerIdentity}`）。
///
/// 注销列表的**分发承载面**协议未定（credential §3「承载面随 C10 定」），
/// 本键域是数据账号侧的本地暂存形态：分发渠道落地后按 issuer 写入快照，
/// read-gate 消费点（orgq 查询门禁）按 issuer 读取；缺失 = 数据不可用 →
/// fail-closed 拒绝（`revocation-unavailable`）。
pub const CRED_REV_PREFIX: &str = "cred:rev:";

/// 注销证明快照存储键（`cred:rev:{issuerIdentity}`）。
pub fn revocation_snapshot_key(issuer: &str) -> String {
    format!("{CRED_REV_PREFIX}{issuer}")
}

/// `cred:rev:` 值形态：某 issuer 的注销链全量条目 + 现行头承诺（本地存储
/// 形态，非协议线形；条目须覆盖 `seq 1..=head.headSeq`，链式校验在
/// [`verify_credential_chain`] 内逐条重算）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevocationSnapshot {
    /// 注销链全量条目。
    pub entries: Vec<RevocationEntry>,
    /// 现行头承诺。
    pub head: RevocationHead,
}

/// 单条 holderProof 验签（read-gate §4 第 4 步 / credential §6 第 6 步）：
/// 载荷按五键重建并绑定 `request_id`/`org_id`/`collection`/`presented_at`，
/// 验签密钥 = 凭证 `holder.publicKey`。
///
/// `org_id` = 凭证呈现所对的域（与凭证 subjectDomain / readPolicy
/// verifierDomain 同口径，见向量 readGate.envelope）。
pub fn verify_holder_proof(
    proof: &HolderProof,
    holder_public_key: &str,
    cred_id: &str,
    request_id: &str,
    org_id: &str,
    collection: &str,
    presented_at: i64,
) -> Result<()> {
    if proof.cred_id != cred_id {
        return Err(CredentialError::HolderProofInvalid);
    }
    let payload = holder_proof_payload(cred_id, request_id, org_id, collection, presented_at);
    if !verify_ed25519_signature(&payload, &proof.sig, holder_public_key) {
        return Err(CredentialError::HolderProofInvalid);
    }
    Ok(())
}

/// readAuth 段验证（read-gate §4 第 1–4 步 + A15 城门名册回查，fail-closed）。
///
/// - `request_id` / `collection`：本次 orgq-req 的请求 id 与目标集合
///   （holderProof 载荷绑定值）；
/// - `trust_decls`：`policy.verifier_domain` 的全部已知信任声明版本；
/// - `revocation_for`：按 issuer identity 取注销证明（头承诺 + 全量条目）；
///   返回 None = 数据缺失 → 失败（fail-closed）；
/// - `roster_lookup`：城门名册回查（A15 membership §4.3「成员资格凭证验签 + 名册回查（当时确为成员）」），按 (subjectDomain, holder identity) 查成员资格——`Some(true)` 才放行；`Some(false)` 退队即拒，零密钥轮换；`None` 名册不可用，fail-closed。
///
/// 语义：credentials 非空且**逐条**全过（验证链 + 类型/域匹配 + holderProof +
/// 名册回查），holderProofs 不得有无凭证对应的孤儿。任一失败即拒绝
/// （`denied` 由调用方落）。
#[allow(clippy::too_many_arguments)]
pub fn verify_read_auth(
    read_auth: &ReadAuth,
    request_id: &str,
    collection: &str,
    policy: &CredentialReadPolicy,
    trust_decls: &[&TrustDecl],
    revocation_for: &dyn Fn(&str) -> Option<(Vec<RevocationEntry>, RevocationHead)>,
    roster_lookup: &dyn Fn(&str, &str) -> Option<bool>,
    now_ms: i64,
) -> Result<()> {
    // 第 1 步：结构 + presentedAt 新鲜度
    if read_auth.gate_v != 1 {
        return Err(CredentialError::InvalidStructure("gateV must be 1"));
    }
    if read_auth.credentials.is_empty() {
        return Err(CredentialError::InvalidStructure("credentials required"));
    }
    if !is_fresh(read_auth.presented_at, now_ms) {
        return Err(CredentialError::StaleTimestamp);
    }
    for proof in &read_auth.holder_proofs {
        if !is_valid_hash(&proof.cred_id) {
            return Err(CredentialError::InvalidStructure(
                "holderProof credId shape",
            ));
        }
    }

    let mut proven_cred_ids: Vec<String> = Vec::with_capacity(read_auth.credentials.len());
    for cred in read_auth.credentials.iter() {
        // 第 2 步：逐凭证验证链（结构 → credId → 验签 → 信任匹配 → 注销检查）
        let cred_id = verify_one_credential(cred, trust_decls, revocation_for, now_ms)?;
        // 重复呈现同一 credId 拒绝（专用诊断码；fail-closed 行为不变——此前
        // 会落到尾部 proof 数比对报 holder-proof-missing，与真实原因不符）
        if proven_cred_ids.contains(&cred_id) {
            return Err(CredentialError::DuplicateCredential);
        }
        // 第 3 步：credType ∈ readPolicy.credTypes 且 subjectDomain 匹配 verifierDomain
        if !policy.cred_types.iter().any(|t| t == &cred.cred_type) {
            return Err(CredentialError::CredTypeNotAllowed);
        }
        if cred.subject_domain != policy.verifier_domain {
            return Err(CredentialError::SubjectDomainMismatch);
        }
        // 城门名册回查（A15）：持有者当时确为凭证 subjectDomain 成员——退队
        // 即失效（零密钥轮换；名册不可用 fail-closed）
        if roster_lookup(&cred.subject_domain, &cred.holder.identity) != Some(true) {
            return Err(CredentialError::NotSubjectDomainMember);
        }
        // 第 4 步：holderProof 逐凭证验签（载荷绑定 requestId 防重放）
        let proof = find_proof(&read_auth.holder_proofs, &cred_id)?;
        verify_holder_proof(
            proof,
            &cred.holder.public_key,
            &cred_id,
            request_id,
            &cred.subject_domain,
            collection,
            read_auth.presented_at,
        )?;
        proven_cred_ids.push(cred_id);
    }
    // 孤儿 proof 拒绝（proof 数多于凭证数即存在无凭证对应者）
    if read_auth.holder_proofs.len() != proven_cred_ids.len() {
        return Err(CredentialError::HolderProofMissing);
    }
    Ok(())
}

/// 单凭证：验证链 + 取注销证明。
fn verify_one_credential(
    cred: &Credential,
    trust_decls: &[&TrustDecl],
    revocation_for: &dyn Fn(&str) -> Option<(Vec<RevocationEntry>, RevocationHead)>,
    now_ms: i64,
) -> Result<String> {
    let domain_decls: Vec<&TrustDecl> = trust_decls
        .iter()
        .copied()
        .filter(|d| d.org_id == cred.subject_domain)
        .collect();
    let (entries, head) =
        revocation_for(&cred.issuer.identity).ok_or(CredentialError::RevocationUnavailable)?;
    let view = RevocationView {
        entries: &entries,
        head: &head,
    };
    verify_credential_chain(cred, &domain_decls, Some(&view), now_ms)
}

/// 按 credId 取 holderProof；重复 credId 视为结构错误（一证多证必有一假）。
fn find_proof<'a>(proofs: &'a [HolderProof], cred_id: &str) -> Result<&'a HolderProof> {
    let mut matches = proofs.iter().filter(|p| p.cred_id == cred_id);
    let proof = matches.next().ok_or(CredentialError::HolderProofMissing)?;
    if matches.next().is_some() {
        return Err(CredentialError::HolderProofInvalid);
    }
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use ed25519_dalek::{Signer, SigningKey};

    use super::super::credential::{
        credential_id, credential_sign_payload, identity_from_public_key,
    };
    use super::super::revocation::{
        revocation_entry_hash, revocation_head_payload, revocation_sign_payload,
    };
    use super::super::types::{
        HolderKind, HolderRef, IdentityRef, OrgSigSet, RosterAnchor, RosterCommitment,
        VerifierGrant,
    };

    const NOW: i64 = 1_720_000_000_000;
    const REQUEST_ID: &str = "req-1";
    const COLLECTION: &str = "finance:monthly@v1";

    fn org_id() -> String {
        format!("org_{}", "aa".repeat(32))
    }

    fn b64_pk(key: &SigningKey) -> String {
        B64.encode(key.verifying_key().to_bytes())
    }

    fn identity_of(key: &SigningKey) -> String {
        identity_from_public_key(&b64_pk(key)).expect("signing key pubkey shape")
    }

    fn sign(key: &SigningKey, payload: &str) -> String {
        B64.encode(key.sign(payload.as_bytes()).to_bytes())
    }

    /// 全链路合法夹具：真实 ed25519 密钥 + 签名（凭证/注销链/头承诺/holderProof）。
    struct Fixture {
        holder: SigningKey,
        cred: Credential,
        cred_id: String,
        trust_decl: TrustDecl,
        entries: Vec<RevocationEntry>,
        head: RevocationHead,
    }

    fn fixture() -> Fixture {
        let issuer = SigningKey::from_bytes(&[7u8; 32]);
        let holder = SigningKey::from_bytes(&[9u8; 32]);
        let mut cred = Credential {
            cred_v: 1,
            cred_type: "member".to_string(),
            issuer: IdentityRef {
                identity: identity_of(&issuer),
                public_key: b64_pk(&issuer),
            },
            holder: HolderRef {
                kind: HolderKind::Person,
                identity: identity_of(&holder),
                public_key: b64_pk(&holder),
            },
            subject_domain: org_id(),
            claims: serde_json::Map::new(),
            method: "plugin:test:manual".to_string(),
            link_ref: None,
            issued_at: NOW,
            sig: String::new(),
        };
        cred.sig = sign(&issuer, &credential_sign_payload(&cred).expect("payload"));
        let cred_id = credential_id(&cred).expect("credId");
        let trust_decl = TrustDecl {
            trust_v: 1,
            org_id: org_id(),
            verifiers: vec![VerifierGrant {
                identity: identity_of(&issuer),
                public_key: b64_pk(&issuer),
                cred_types: vec!["member".to_string()],
                methods: vec!["plugin:test:*".to_string()],
            }],
            effective_from: 0,
            seq: 1,
            updated_at: 0,
            sig_set: OrgSigSet {
                sig_set_v: 1,
                org_id: org_id(),
                subject: "00".repeat(32),
                policy_hash: "00".repeat(32),
                roster: RosterCommitment {
                    member_set_hash: "00".repeat(32),
                    anchor: RosterAnchor {
                        org_id: org_id(),
                        anchor_root: "00".repeat(32),
                        ts: 0,
                    },
                    snapshot: None,
                },
                signed_at: 0,
                signatures: vec![],
            },
        };
        let mut entry = RevocationEntry {
            rev_v: 1,
            issuer: identity_of(&issuer),
            seq: 1,
            prev_hash: None,
            cred_id: "ff".repeat(32),
            revoked_at: NOW,
            reason: None,
            sig: String::new(),
        };
        entry.sig = sign(&issuer, &revocation_sign_payload(&entry).expect("payload"));
        let mut head = RevocationHead {
            rev_head_v: 1,
            issuer: identity_of(&issuer),
            head_seq: 1,
            head_hash: revocation_entry_hash(&entry).expect("entryHash"),
            as_of: NOW,
            sig: String::new(),
        };
        head.sig = sign(&issuer, &revocation_head_payload(&head).expect("payload"));
        Fixture {
            holder,
            cred,
            cred_id,
            trust_decl,
            entries: vec![entry],
            head,
        }
    }

    fn proof_of(f: &Fixture) -> HolderProof {
        HolderProof {
            cred_id: f.cred_id.clone(),
            sig: sign(
                &f.holder,
                &holder_proof_payload(&f.cred_id, REQUEST_ID, &org_id(), COLLECTION, NOW),
            ),
        }
    }

    fn verify(f: &Fixture, read_auth: &ReadAuth) -> Result<()> {
        verify_with_roster(f, read_auth, Some(true))
    }

    fn verify_with_roster(f: &Fixture, read_auth: &ReadAuth, in_roster: Option<bool>) -> Result<()> {
        let policy = CredentialReadPolicy {
            cred_types: vec!["member".to_string()],
            verifier_domain: org_id(),
        };
        let decls: Vec<&TrustDecl> = vec![&f.trust_decl];
        let revocation_for = |_issuer: &str| Some((f.entries.clone(), f.head.clone()));
        let roster_lookup = move |_domain: &str, _identity: &str| in_roster;
        verify_read_auth(
            read_auth,
            REQUEST_ID,
            COLLECTION,
            &policy,
            &decls,
            &revocation_for,
            &roster_lookup,
            NOW,
        )
    }

    #[test]
    fn single_presentation_passes() {
        let f = fixture();
        let read_auth = ReadAuth {
            gate_v: 1,
            credentials: vec![f.cred.clone()],
            holder_proofs: vec![proof_of(&f)],
            presented_at: NOW,
        };
        assert!(verify(&f, &read_auth).is_ok());
    }

    #[test]
    fn duplicate_credential_presentation_rejected_with_dedicated_kind() {
        let f = fixture();
        let read_auth = ReadAuth {
            gate_v: 1,
            credentials: vec![f.cred.clone(), f.cred.clone()],
            holder_proofs: vec![proof_of(&f)],
            presented_at: NOW,
        };
        // 钉住专用错误名（此前误报 holder-proof-missing，与真实原因不符）
        let err = verify(&f, &read_auth).unwrap_err();
        assert_eq!(err.kind(), "duplicate-credential");
    }

    /// 城门名册回查（A15 membership §4.3）：验证链全过但持有者已退队 →
    /// 拒（零密钥轮换）；名册数据不可用同样 fail-closed。
    #[test]
    fn gate_roster_recheck_fail_closed() {
        let f = fixture();
        let read_auth = ReadAuth {
            gate_v: 1,
            credentials: vec![f.cred.clone()],
            holder_proofs: vec![proof_of(&f)],
            presented_at: NOW,
        };
        // 退队（Some(false)）→ not-subject-domain-member
        let err = verify_with_roster(&f, &read_auth, Some(false)).unwrap_err();
        assert_eq!(err.kind(), "not-subject-domain-member");
        // 名册不可用（None）→ 同码 fail-closed
        let err = verify_with_roster(&f, &read_auth, None).unwrap_err();
        assert_eq!(err.kind(), "not-subject-domain-member");
        // 在册（Some(true)）→ 通过
        assert!(verify_with_roster(&f, &read_auth, Some(true)).is_ok());
    }
}
