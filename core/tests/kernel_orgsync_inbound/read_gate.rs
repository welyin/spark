//! read-gate（读授权门禁，community read-gate §4）orgq 查询面集成测试：
//! `readPolicy.kind == "credential"` 集合的查询方 readAuth 校验
//! （`verify_read_auth`，§4 第 1–4 步 + A15 城门名册回查）+ policyRef 存在时
//! 开放声明求值（§4 第 5 步，A15 城门口径：`org:disclosure:` 生效记录覆盖
//! 本集合才放行，旧 B1 文档求值已随口径一次性切换下线，membership §五.2）
//! 正/反例；`public` 种类与 members 缺省（向后兼容）分流。
//!
//! 夹具用真实 ed25519 密钥签名（凭证/注销链/头承诺/holderProof），不 mock
//! （kernel_credential_policy_ops.rs / community_credential_vectors.rs 同模式）。

use super::orgq::deliver_orgq_req_with_hook;
use super::*;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::Signer;
use spark_core::credential::{
    Credential, HolderKind, HolderProof, HolderRef, IdentityRef, OrgSigSet, ReadAuth,
    RevocationEntry, RevocationHead, RevocationSnapshot, RosterAnchor, RosterCommitment, TrustDecl,
    VerifierGrant, credential_id, credential_sign_payload, held_credential_key,
    holder_proof_payload, revocation_entry_hash, revocation_head_payload, revocation_sign_payload,
    revocation_snapshot_key, trust_decl_key,
};
use spark_core::kernel::OrgqPermHook;
use spark_core::org::types::OrganizationAccessKey;
use spark_core::plugindata::{ReadPolicy, ReadPolicyKind};
use spark_core::policy::{
    DISCLOSURE_PUB_PERIOD_MS, DISCLOSURE_V, DisclosureRecord, RosterTier, disclosure_key,
    eval_disclosure,
};
use spark_core::sync::orgsync::{build_orgq_query_req, build_orgq_write_req};

/// 验证人信任声明所在域（readPolicy.verifierDomain；创世哈希型 orgId）。
const GATE_ORG: &str = "org_abababababababababababababababababababababababababababababababab";

fn col_full() -> String {
    format!("{NAME}@v{VERSION}")
}

fn b64_pk(key: &SigningKey) -> String {
    B64.encode(key.verifying_key().to_bytes())
}

fn sign(key: &SigningKey, payload: &str) -> String {
    B64.encode(key.sign(payload.as_bytes()).to_bytes())
}

/// 占位 OrgSigSet（read_gate 验证链不消费 sigSet 内容，结构占位即可——
/// 与 kernel_credential_policy_ops.rs 夹具同口径）。
fn placeholder_sig_set() -> OrgSigSet {
    OrgSigSet {
        sig_set_v: 1,
        org_id: GATE_ORG.to_string(),
        subject: "00".repeat(32),
        policy_hash: "00".repeat(32),
        roster: RosterCommitment {
            member_set_hash: "00".repeat(32),
            anchor: RosterAnchor {
                org_id: GATE_ORG.to_string(),
                anchor_root: "00".repeat(32),
                ts: 0,
            },
            snapshot: None,
        },
        signed_at: 0,
        signatures: vec![],
    }
}

/// credential 门禁声明（credTypes=["member"]，verifierDomain=GATE_ORG）。
fn gate_policy() -> ReadPolicy {
    ReadPolicy {
        kind: ReadPolicyKind::Credential,
        cred_types: vec!["member".to_string()],
        verifier_domain: GATE_ORG.to_string(),
        policy_ref: None,
    }
}

/// 声明带 readPolicy 的 org data-accounts 集合。
fn declare_gated(s: &mut MemoryStorage, org_id: &str, read_policy: ReadPolicy) {
    declare(
        s,
        "ai-chat",
        DeclareInput {
            name: NAME.to_string(),
            version: Some(VERSION.to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::DataAccounts),
            declared_by: Some("creator".to_string()),
            read_policy: Some(read_policy),
            ..Default::default()
        },
        NOW,
        Some(org_id),
    )
    .unwrap();
}

/// 信任声明：issuer(self_identity(7)) 授权 member+resident 两类凭证。
/// （resident 一并授权是为「credType 不符」反例隔离第 3 步——信任匹配能过、
/// 只在 readPolicy.credTypes 匹配上失败。）
fn setup_gate_trust(s: &mut MemoryStorage) {
    let (issuer_key, issuer_id) = self_identity(7);
    let decl = TrustDecl {
        trust_v: 1,
        org_id: GATE_ORG.to_string(),
        verifiers: vec![VerifierGrant {
            identity: issuer_id,
            public_key: b64_pk(&issuer_key),
            cred_types: vec!["member".to_string(), "resident".to_string()],
            methods: vec!["plugin:test:*".to_string()],
        }],
        effective_from: 0,
        seq: 1,
        updated_at: 0,
        sig_set: placeholder_sig_set(),
    };
    s.put(
        &trust_decl_key(GATE_ORG),
        &serde_json::to_string(&decl).unwrap(),
    )
    .unwrap();
}

/// 注销快照（cred:rev: 本地键域）：单条目链 + 头承诺，注销指定 credId。
fn setup_gate_revocation(s: &mut MemoryStorage, revoked_cred_id: &str) {
    let (issuer_key, issuer_id) = self_identity(7);
    let mut entry = RevocationEntry {
        rev_v: 1,
        issuer: issuer_id.clone(),
        seq: 1,
        prev_hash: None,
        cred_id: revoked_cred_id.to_string(),
        revoked_at: NOW,
        reason: None,
        sig: String::new(),
    };
    entry.sig = sign(&issuer_key, &revocation_sign_payload(&entry).unwrap());
    let mut head = RevocationHead {
        rev_head_v: 1,
        issuer: issuer_id.clone(),
        head_seq: 1,
        head_hash: revocation_entry_hash(&entry).unwrap(),
        as_of: NOW,
        sig: String::new(),
    };
    head.sig = sign(&issuer_key, &revocation_head_payload(&head).unwrap());
    let snap = RevocationSnapshot {
        entries: vec![entry],
        head,
    };
    s.put(
        &revocation_snapshot_key(&issuer_id),
        &serde_json::to_string(&snap).unwrap(),
    )
    .unwrap();
}

/// 门禁正例夹具：issuer(7) 签发的 member 凭证，holder = self_identity(9)。
struct GateFixture {
    holder_key: SigningKey,
    holder_root: String,
    cred: Credential,
    cred_id: String,
}

fn gate_fixture() -> GateFixture {
    let (issuer_key, issuer_id) = self_identity(7);
    let (holder_key, holder_root) = self_identity(9);
    let mut cred = Credential {
        cred_v: 1,
        cred_type: "member".to_string(),
        issuer: IdentityRef {
            identity: issuer_id,
            public_key: b64_pk(&issuer_key),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: holder_root.clone(),
            public_key: b64_pk(&holder_key),
        },
        subject_domain: GATE_ORG.to_string(),
        claims: serde_json::Map::new(),
        method: "plugin:test:manual".to_string(),
        link_ref: None,
        issued_at: NOW,
        sig: String::new(),
    };
    cred.sig = sign(&issuer_key, &credential_sign_payload(&cred).unwrap());
    let cred_id = credential_id(&cred).unwrap();
    GateFixture {
        holder_key,
        holder_root,
        cred,
        cred_id,
    }
}

/// 构造 readAuth 呈现段：holderProof 载荷绑定 (requestId, GATE_ORG,
/// collection, presentedAt)（read-gate §3 五键）。
fn read_auth_for(f: &GateFixture, request_id: &str) -> ReadAuth {
    let payload = holder_proof_payload(&f.cred_id, request_id, GATE_ORG, &col_full(), NOW);
    ReadAuth {
        gate_v: 1,
        credentials: vec![f.cred.clone()],
        holder_proofs: vec![HolderProof {
            cred_id: f.cred_id.clone(),
            sig: sign(&f.holder_key, &payload),
        }],
        presented_at: NOW,
    }
}

/// 构造携带 readAuth 的 orgq-req 查询 body（经 build_orgq_query_req 的
/// readAuth 入参上线形——查询发起侧接线后的正式路径）。
fn gate_query_body(request_id: &str, read_auth: Option<&ReadAuth>) -> serde_json::Value {
    build_orgq_query_req(ORG_ID, &col_full(), None, 10, None, request_id, read_auth)
}

/// 永不放行的假钩子：证明 credential 门禁路径不经过插件钩子
/// （has_runtime=false 在 members 缺省下必降级 denied）。
struct NeverHook;

impl OrgqPermHook for NeverHook {
    fn has_runtime(&self, _collection: &str, _kind: &str) -> bool {
        false
    }
    fn can_read(&self, _member: &str, _collection: &str, _key: &str) -> bool {
        false
    }
    fn can_write(
        &self,
        _member: &str,
        _collection: &str,
        _key: &str,
        _value: &serde_json::Value,
    ) -> bool {
        false
    }
}

/// credential 门禁集合公共 setup：组织（self 为数据账号）+ 门禁声明 +
/// 信任声明 + 注销快照（未注销任何呈现凭证）+ 两条驻留记录。
fn setup_gated_collection(
    self_root: &str,
    members: Vec<(&str, OrganizationRole)>,
    read_policy: ReadPolicy,
) -> MemoryStorage {
    let mut s = MemoryStorage::new();
    let mut all_members = members;
    all_members.push((self_root, OrganizationRole::Admin));
    save_org(&mut s, ORG_ID, all_members, &[self_root]);
    declare_gated(&mut s, ORG_ID, read_policy);
    setup_gate_trust(&mut s);
    setup_gate_revocation(&mut s, &"ff".repeat(32));
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    write_org_data(
        &mut s,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        "k2",
        "\"v2\"",
        NOW + 1,
    );
    s
}

/// 城门名册（A15 membership §4.3 名册回查）：GATE_ORG（凭证 subjectDomain /
/// readPolicy.verifierDomain）的组织记录——持有者**当时确为该域成员**城门
/// 才放行（退队即拒，零密钥轮换；记录缺失 fail-closed）。
fn setup_gate_roster(s: &mut MemoryStorage, members: Vec<(&str, OrganizationRole)>) {
    save_org(s, GATE_ORG, members, &[]);
}

/// 属主组织（ORG_ID）对 GATE_ORG 的开放声明（A15：`org:disclosure:` 记录，
/// 直接落键域——读路径消费的是合入后的发布件，合入侧五步链把关见
/// `adjudicate_incoming_disclosure`，不在本组覆盖面）。`effective_at` 注入
/// 以覆盖「公示延迟窗口内未生效」用例。
fn setup_disclosure(s: &mut MemoryStorage, collections: Vec<String>, effective_at: i64) {
    let record = DisclosureRecord {
        disclosure_v: DISCLOSURE_V,
        org_id: ORG_ID.to_string(),
        target_domain: GATE_ORG.to_string(),
        tier: RosterTier::OrgOnly,
        fields: vec![],
        collections,
        version: 1,
        updated_at: NOW,
        effective_at,
        sig_set: None,
    };
    s.put(
        &disclosure_key(ORG_ID, GATE_ORG),
        &serde_json::to_string(&record).unwrap(),
    )
    .unwrap();
}

// ── 正例 ────────────────────────────────────────────────────────────────

/// 正例（read-gate §5 核心场景）：**非组织成员**持 GATE_ORG 成员资格凭证
/// 查询 credential 门禁集合 → 验证链全过 + 城门名册回查在册（A15：持有者是
/// GATE_ORG 在册成员）→ 放行服务（无需加入来源组织，插件钩子未运行也不
/// 影响——钩子已换为凭证校验）。
#[test]
fn read_gate_non_member_with_valid_credential_served() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    // holder(f.holder_root) 不在来源组织（ORG_ID）成员表，但在 GATE_ORG 名册
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );

    let read_auth = read_auth_for(&f, "req-g1");
    let body = gate_query_body("req-g1", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_eq!(r.response["ok"], true, "数据账号受理（应答走 orgsync_out）");
    assert_eq!(r.orgsync_out.len(), 1, "回一条 orgq-resp");
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "门禁通过 → 非降级");
    let records = resp["records"].as_array().unwrap();
    assert_eq!(records.len(), 2, "门禁通过即全集合放行（不过 canRead）");
}

/// 正例：组织成员持合法凭证同样放行（成员资格与凭证双轨互不冲突）。
#[test]
fn read_gate_member_with_valid_credential_served() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(
        &self_root,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
        gate_policy(),
    );
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );

    let read_auth = read_auth_for(&f, "req-g2");
    let body = gate_query_body("req-g2", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    assert_eq!(resp["records"].as_array().unwrap().len(), 2);
}

/// 正例（§4 第 5 步，A15 城门口径）：policyRef 存在（线形槽位不变，充当
/// 「本集合受开放声明约束」开关）→ 按**开放声明**求值——属主组织对凭证
/// subjectDomain（GATE_ORG）的生效 disclosure 记录 collections 覆盖本集合
/// → 放行（旧 B1 向上开放矩阵语义由 disclosure.collections 吸收，
/// membership §五.2 一次性切换）。
#[test]
fn read_gate_disclosure_covered_served() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut policy = gate_policy();
    policy.policy_ref = Some("aa".repeat(32));
    let mut s = setup_gated_collection(&self_root, vec![], policy);
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );
    setup_disclosure(&mut s, vec![col_full()], NOW);

    let read_auth = read_auth_for(&f, "req-g3");
    let body = gate_query_body("req-g3", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "生效开放声明覆盖本集合 → 放行");
    assert_eq!(resp["records"].as_array().unwrap().len(), 2);
}

// ── 反例（任一环节失败 → denied 空集，fail-closed） ──────────────────────

/// 门禁断言助手：投递后断言 denied 空集应答（协议语义：非授权者连元数据
/// 都不给）。
fn assert_gate_denied(r: &spark_core::kernel::InboundDmResult) {
    assert_eq!(r.response["ok"], true, "受理但门禁拒绝（应答走 orgsync_out）");
    assert_eq!(r.orgsync_out.len(), 1, "回一条 orgq-resp");
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(true), "门禁拒绝 → denied");
    assert_eq!(resp["records"], json!([]), "denied 时空集");
}

/// 反例：credential 门禁集合不带 readAuth → denied（组织成员也一样——
/// 该种类的授权口径是凭证，不是成员资格）。
#[test]
fn read_gate_missing_read_auth_denied() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(
        &self_root,
        vec![(m_root.as_str(), OrganizationRole::Member)],
        gate_policy(),
    );

    let body = gate_query_body("req-d1", None);
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body, &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例：credType 不在 readPolicy.credTypes 内 → denied（§4 第 3 步；
/// 信任声明授权 resident，隔离为纯类型匹配失败）。
#[test]
fn read_gate_wrong_cred_type_denied() {
    let mut f = gate_fixture();
    // 重签一个 resident 类型凭证（issuer 信任覆盖，但不在 readPolicy.credTypes）
    let (issuer_key, _issuer_id) = self_identity(7);
    f.cred.cred_type = "resident".to_string();
    f.cred.sig = sign(&issuer_key, &credential_sign_payload(&f.cred).unwrap());
    f.cred_id = credential_id(&f.cred).unwrap();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());

    let read_auth = read_auth_for(&f, "req-d2");
    let body = gate_query_body("req-d2", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例：凭证已注销（credId 出现在 issuer 注销链内）→ denied（§4 第 2 步
/// 注销检查，成员退出即凭证注销的产品语义）。
#[test]
fn read_gate_revoked_credential_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    // 覆盖注销快照：注销本次呈现的 credId
    setup_gate_revocation(&mut s, &f.cred_id);

    let read_auth = read_auth_for(&f, "req-d3");
    let body = gate_query_body("req-d3", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例：holderProof 绑定别的 requestId（重放转投）→ denied（§4 第 4 步）。
#[test]
fn read_gate_holder_proof_replay_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());

    // 名册在册（城门前提）——失败隔离在 §4 第 4 步 holderProof 绑定
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );
    // proof 按 req-old 签，请求用 req-new（载荷绑定不符）
    let read_auth = read_auth_for(&f, "req-old");
    let body = gate_query_body("req-new", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例：注销证明数据缺失（无 cred:rev: 快照）→ denied（fail-closed：
/// revocation-unavailable，不可证「未注销」即不可放行）。
#[test]
fn read_gate_revocation_unavailable_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    // 删除注销快照（模拟分发渠道未落地/数据缺失）
    let (_issuer_key, issuer_id) = self_identity(7);
    s.delete(&revocation_snapshot_key(&issuer_id)).unwrap();

    let read_auth = read_auth_for(&f, "req-d4");
    let body = gate_query_body("req-d4", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例（§4 第 5 步，A15 城门口径）：生效 disclosure 未覆盖本集合 →
/// denied（fail-closed；未声明即「仅组织」默认档，存量组织默认全隐）。
#[test]
fn read_gate_disclosure_not_covered_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut policy = gate_policy();
    policy.policy_ref = Some("aa".repeat(32));
    let mut s = setup_gated_collection(&self_root, vec![], policy);
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );
    setup_disclosure(&mut s, vec![], NOW); // 生效但不开放任何集合

    let read_auth = read_auth_for(&f, "req-d5");
    let body = gate_query_body("req-d5", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例（§4 第 5 步，A15 公示延迟）：disclosure 已发布但 effectiveAt 未到
/// （扩大方向公示延迟窗口内）→ denied——发布即公示不等于即时生效，窗口内
/// 读取点仍按无声明处置（fail-closed）。
#[test]
fn read_gate_disclosure_pending_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut policy = gate_policy();
    policy.policy_ref = Some("aa".repeat(32));
    let mut s = setup_gated_collection(&self_root, vec![], policy);
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );
    setup_disclosure(&mut s, vec![col_full()], NOW + DISCLOSURE_PUB_PERIOD_MS);

    let read_auth = read_auth_for(&f, "req-d6");
    let body = gate_query_body("req-d6", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例（A15 城门真值表）：验证链全过但持有者**当时不是** subjectDomain
/// 成员 → denied（退队即失效，零密钥轮换）；GATE_ORG 名册记录缺失（不可
/// 用）同样 fail-closed denied。
#[test]
fn read_gate_roster_non_member_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);

    // 情形一：名册在但 holder 不在册（非成员/已退队）
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    setup_gate_roster(&mut s, vec![("someone-else", OrganizationRole::Member)]);
    let read_auth = read_auth_for(&f, "req-d7");
    let body = gate_query_body("req-d7", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);

    // 情形二：GATE_ORG 名册记录缺失（不可用）→ fail-closed
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    let read_auth = read_auth_for(&f, "req-d8");
    let body = gate_query_body("req-d8", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 反例（A15 零密钥轮换）：名册移除持有者后**立即**拒读——同一凭证同一
/// 请求，名册回查时刻语义（无轮换窗、无缓存宽限）。
#[test]
fn read_gate_member_leave_immediately_denied() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );

    // 退楼前：放行
    let read_auth = read_auth_for(&f, "req-l1");
    let body = gate_query_body("req-l1", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body.clone(),
        &NeverHook,
    );
    assert_eq!(r.orgsync_out[0].body()["denied"], json!(false), "在册放行");

    // 退楼（名册移除持有者）→ 同一 readAuth 立即拒读
    setup_gate_roster(&mut s, vec![]);
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 正例（A16 双键兼容）：名册成员只携带 org_user_id（accessKey 域公钥），
/// 凭证 holder.identity 按 org_user_id 命中名册 → 城门放行（rootId 槽位
/// 不出示也能回查）。
#[test]
fn read_gate_dual_key_org_user_id_served() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    // 名册条目 rootId 与 holder 无关，但 accessKey 域公钥 = holder 公钥 →
    // org_user_id = sha256hex(holder 公钥) = cred.holder.identity
    setup_gate_roster(
        &mut s,
        vec![("m-org-user-id-only", OrganizationRole::Member)],
    );
    let mut record = spark_core::org::OrganizationService::get_record(&s, GATE_ORG)
        .unwrap()
        .expect("gate roster");
    record.members[0].access_key = Some(OrganizationAccessKey {
        public_key: b64_pk(&f.holder_key),
        bind_sig: String::new(), // 名册回查只派生 org_user_id，验绑归合入侧
        root_pubkey: None,
    });
    spark_core::org::OrganizationService::save_record(&mut s, &record).unwrap();

    let read_auth = read_auth_for(&f, "req-g4");
    let body = gate_query_body("req-g4", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "org_user_id 命中名册 → 放行");
    assert_eq!(resp["records"].as_array().unwrap().len(), 2);
}

// ── public 种类与写路径 / 向后兼容 ───────────────────────────────────────

/// public 种类：公开发布——非成员、无凭证、插件未运行也直接服务。
#[test]
fn read_gate_public_kind_serves_outsider_without_auth() {
    let (x_key, x_root) = self_identity(9); // 非成员
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(
        &self_root,
        vec![],
        ReadPolicy {
            kind: ReadPolicyKind::Public,
            cred_types: vec![],
            verifier_domain: String::new(),
            policy_ref: None,
        },
    );

    let body = gate_query_body("req-p1", None);
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &x_key, &x_root, &self_root, body, &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "public 集合直接服务");
    assert_eq!(resp["records"].as_array().unwrap().len(), 2);
}

/// 写路径不受 readPolicy 影响：credential 门禁集合的写入仍要求组织成员
/// （非成员写 → rejected；门禁只管读）。
#[test]
fn read_gate_write_path_still_requires_membership() {
    let f = gate_fixture(); // holder 非成员
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());

    let body = build_orgq_write_req(
        ORG_ID,
        &col_full(),
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k1".to_string(),
            value: json!({"a": 1}),
        }],
        "req-w1",
    );
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("rejected"), "非成员写仍拒绝");
    assert!(r.orgsync_out.is_empty());
}

/// 向后兼容：无 readPolicy 的集合维持 members 缺省——成员查询在插件未
/// 运行时照旧降级 denied（filtered fail-closed 现状不变）。
#[test]
fn read_gate_absent_policy_keeps_members_default() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[self_root.as_str()],
    );
    declare_org_collection(
        &mut s,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::DataAccounts,
        &self_root,
        NOW,
    );

    let body = gate_query_body("req-c1", None);
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body, &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(true), "无 readPolicy → 现状降级语义不变");
    assert_eq!(resp["records"], json!([]));
}

// ── 查询发起侧（read-gate §3）：build_query_read_auth 端到端 ─────────────
//
// 以上用例手工构造 readAuth 验证服务端门禁；本组用查询发起侧的正式构造
// 路径（`kernel::build_query_read_auth`：cred:held: 本地凭证存储 + 种子
// 派生的调用方插件域身份签 holderProof）产出 readAuth 随 orgq-req 上线形，
// 验证两端线形/签名口径咬合（发起侧构造物服务端门禁可验）。

/// 发起方持有凭证夹具：BIP39 种子 + 插件域身份为 holder 的 member 凭证
/// （issuer(7) 签发，与 setup_gate_trust 信任声明匹配）。返回（种子，域，
/// 凭证，credId）。
fn held_credential_fixture() -> ([u8; 64], String, Credential, String) {
    let seed = [42u8; 64];
    let domain = "plugin:ai-chat".to_string();
    let holder = spark_core::identity::derive_domain_identity(&seed, &domain);
    let (issuer_key, issuer_id) = self_identity(7);
    let mut cred = Credential {
        cred_v: 1,
        cred_type: "member".to_string(),
        issuer: IdentityRef {
            identity: issuer_id,
            public_key: b64_pk(&issuer_key),
        },
        holder: HolderRef {
            kind: HolderKind::Person,
            identity: holder.id(),
            public_key: B64.encode(holder.public_key()),
        },
        subject_domain: GATE_ORG.to_string(),
        claims: serde_json::Map::new(),
        method: "plugin:test:manual".to_string(),
        link_ref: None,
        issued_at: NOW,
        sig: String::new(),
    };
    cred.sig = sign(&issuer_key, &credential_sign_payload(&cred).unwrap());
    let cred_id = credential_id(&cred).unwrap();
    (seed, domain, cred, cred_id)
}

/// 正例（两端接通）：发起方本地持有匹配 readPolicy 的凭证（cred:held: 键域）
/// → `build_query_read_auth` 以插件域身份构造 readAuth 随 orgq-req 发出 →
/// 服务端 credential 门禁验证链全过 → 放行（持有证明与信封 root 身份解耦：
/// 信封由非成员 root 签，holderProof 由域身份签）。
#[test]
fn read_gate_query_side_built_read_auth_served() {
    let (seed, domain, cred, cred_id) = held_credential_fixture();
    // 发起方本地凭证存储（cred:held: 键域）
    let mut initiator = MemoryStorage::new();
    initiator
        .put(
            &held_credential_key(&cred_id),
            &serde_json::to_string(&cred).unwrap(),
        )
        .unwrap();
    // 数据账号侧：门禁集合（查询者非组织成员）；城门名册 = GATE_ORG 含
    // 持有者的插件域身份（持有证明与信封 root 身份解耦，名册回查按凭证
    // holder.identity 命中）
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    setup_gate_roster(
        &mut s,
        vec![(cred.holder.identity.as_str(), OrganizationRole::Member)],
    );

    let read_auth = spark_core::kernel::build_query_read_auth(
        &initiator,
        &seed,
        &domain,
        &gate_policy(),
        "req-q1",
        &col_full(),
        NOW,
    )
    .expect("持有匹配凭证 → 构造 readAuth");
    assert_eq!(read_auth.gate_v, 1);
    assert_eq!(read_auth.credentials.len(), 1);
    assert_eq!(read_auth.holder_proofs.len(), 1);
    assert_eq!(read_auth.presented_at, NOW);

    let body = gate_query_body("req-q1", Some(&read_auth));
    assert!(body.get("readAuth").is_some(), "orgq-req 线形携带 readAuth 段");
    let (x_key, x_root) = self_identity(9); // 信封身份（非成员，与持有证明解耦）
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &x_key, &x_root, &self_root, body, &NeverHook,
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "发起侧构造的 readAuth 过门禁");
    assert_eq!(resp["records"].as_array().unwrap().len(), 2);
}

/// 反例（两端接通）：本地无匹配凭证（空仓 / holder 非调用方域身份——签不出
/// 持有证明）→ `build_query_read_auth` 返回 None，查询不附 readAuth（维持
/// 现状语义）→ 服务端 credential 门禁 fail-closed denied。
#[test]
fn read_gate_query_side_without_matching_credential_denied() {
    let (seed, domain, _cred, _cred_id) = held_credential_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());

    // 空仓 → None
    let empty = MemoryStorage::new();
    assert!(
        spark_core::kernel::build_query_read_auth(
            &empty,
            &seed,
            &domain,
            &gate_policy(),
            "req-q2",
            &col_full(),
            NOW,
        )
        .is_none(),
        "无持有凭证 → 不构造 readAuth"
    );
    // 持有他域身份的凭证（holder = self_identity(9)，非插件域身份）→ None
    let f = gate_fixture();
    let mut other_holder = MemoryStorage::new();
    other_holder
        .put(
            &held_credential_key(&f.cred_id),
            &serde_json::to_string(&f.cred).unwrap(),
        )
        .unwrap();
    assert!(
        spark_core::kernel::build_query_read_auth(
            &other_holder,
            &seed,
            &domain,
            &gate_policy(),
            "req-q2",
            &col_full(),
            NOW,
        )
        .is_none(),
        "holder 域身份不符 → 不构造 readAuth"
    );

    // 不附 readAuth 的查询（维持现状语义）→ 门禁 fail-closed denied
    let body = gate_query_body("req-q2", None);
    assert!(body.get("readAuth").is_none(), "无匹配凭证 → 线形不带 readAuth");
    let (x_key, x_root) = self_identity(9);
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &x_key, &x_root, &self_root, body, &NeverHook,
    );
    assert_gate_denied(&r);
}

// ── 三组织嵌套（membership §六）：开放声明装配视图 + 城门退队拒读 ─────────

/// 街道域（嵌套第三层；楼栋⇂小区⇂街道）。
const STREET_ORG: &str =
    "org_cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

/// 三组织嵌套集成（membership §六验收）：
/// 1. 楼栋（GATE_ORG）名册默认不上移——对街道（STREET_ORG）无声明 →
///    装配视图恒「仅组织」默认档；
/// 2. 楼栋授权「代表可见」（对小区 ORG_ID 的 disclosure tier=representatives
///    生效）→ 小区层装配视图可见代表档；
/// 3. 小区层城门：楼栋成员持楼栋域成员资格凭证读小区托管集合 → 放行；
///    成员退楼（楼栋名册移除）→ 小区层城门**立即**拒读（零轮换）。
#[test]
fn nested_orgs_building_disclosure_view_and_gate() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    setup_gate_roster(
        &mut s,
        vec![(f.holder_root.as_str(), OrganizationRole::Member)],
    );

    // 1. 名册不上移：楼栋对街道无声明 → 仅组织默认档（最保守，fail-closed）
    let view = eval_disclosure(&[], STREET_ORG, NOW);
    assert_eq!(view.tier, RosterTier::OrgOnly, "无声明 = 仅组织默认档");
    assert!(view.fields.is_empty() && view.collections.is_empty());

    // 2. 楼栋 → 小区授权「代表可见」（已生效声明）→ 小区层视图 = representatives
    let disclosure = DisclosureRecord {
        disclosure_v: DISCLOSURE_V,
        org_id: GATE_ORG.to_string(),
        target_domain: ORG_ID.to_string(),
        tier: RosterTier::Representatives,
        fields: vec![],
        collections: vec![],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None,
    };
    let view = eval_disclosure(&[&disclosure], ORG_ID, NOW);
    assert_eq!(
        view.tier,
        RosterTier::Representatives,
        "授权代表可见后小区层可见代表档"
    );
    // 公示延迟窗口内的同键声明不生效（effectiveAt 未到 → 维持默认档）
    let pending = DisclosureRecord {
        effective_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
        ..disclosure.clone()
    };
    let view = eval_disclosure(&[&pending], ORG_ID, NOW);
    assert_eq!(view.tier, RosterTier::OrgOnly, "未生效声明不装配");

    // 3a. 小区层城门：楼栋成员凭证 → 放行
    let read_auth = read_auth_for(&f, "req-n1");
    let body = gate_query_body("req-n1", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body.clone(),
        &NeverHook,
    );
    assert_eq!(
        r.orgsync_out[0].body()["denied"],
        json!(false),
        "楼栋成员在册 → 小区层城门放行"
    );

    // 3b. 成员退楼 → 小区层城门立即拒读（零密钥轮换）
    setup_gate_roster(&mut s, vec![]);
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}

/// 三组织嵌套（membership §六，A16 切片三 org_user_id 键面扩展）：楼栋名册
/// 条目**只经 org_user_id 可解析**（rootId 槽位不出示成员身份——标识面
/// 切换后的名册形态）——名册不上移 / 代表可见装配 / 城门 org_user_id 命中
/// 放行 / 退楼立即拒读（零轮换），与骨架用例同链路、换键面。
#[test]
fn nested_orgs_org_user_id_key_face() {
    let f = gate_fixture();
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_gated_collection(&self_root, vec![], gate_policy());
    // 楼栋名册：成员 rootId 槽位与 holder 无关；accessKey 域公钥 = holder
    // 公钥 → org_user_id = cred.holder.identity（名册回查只经 org_user_id
    // 命中，rootId 槽位不出示成员身份）
    setup_gate_roster(
        &mut s,
        vec![("m-building-member", OrganizationRole::Member)],
    );
    let mut record = spark_core::org::OrganizationService::get_record(&s, GATE_ORG)
        .unwrap()
        .expect("gate roster");
    record.members[0].access_key = Some(OrganizationAccessKey {
        public_key: b64_pk(&f.holder_key),
        bind_sig: String::new(), // 名册回查只派生 org_user_id，验绑归合入侧
        root_pubkey: None,
    });
    spark_core::org::OrganizationService::save_record(&mut s, &record).unwrap();
    assert_ne!(
        record.members[0].root_id, f.holder_root,
        "名册 rootId 槽位不出示 holder 身份"
    );

    // 1. 名册不上移：楼栋对街道无声明 → 仅组织默认档（fail-closed）
    let view = eval_disclosure(&[], STREET_ORG, NOW);
    assert_eq!(view.tier, RosterTier::OrgOnly, "无声明 = 仅组织默认档");
    assert!(view.fields.is_empty() && view.collections.is_empty());

    // 2. 楼栋 → 小区授权「代表可见」（已生效声明）→ 小区层视图 = representatives
    let disclosure = DisclosureRecord {
        disclosure_v: DISCLOSURE_V,
        org_id: GATE_ORG.to_string(),
        target_domain: ORG_ID.to_string(),
        tier: RosterTier::Representatives,
        fields: vec![],
        collections: vec![],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None,
    };
    let view = eval_disclosure(&[&disclosure], ORG_ID, NOW);
    assert_eq!(
        view.tier,
        RosterTier::Representatives,
        "授权代表可见后小区层可见代表档"
    );

    // 3a. 小区层城门：楼栋成员凭证（holder identity = org_user_id 形态）
    // 命中楼栋名册 org_user_id → 放行（名册键切换后 rootId 不出示也能回查）
    let read_auth = read_auth_for(&f, "req-u1");
    let body = gate_query_body("req-u1", Some(&read_auth));
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body.clone(),
        &NeverHook,
    );
    assert_eq!(
        r.orgsync_out[0].body()["denied"],
        json!(false),
        "org_user_id 命中楼栋名册 → 小区层城门放行"
    );

    // 3b. 成员退楼（名册移除）→ 小区层城门立即拒读（零密钥轮换，键面无关）
    setup_gate_roster(&mut s, vec![]);
    let r = deliver_orgq_req_with_hook(
        &mut s,
        &self_root,
        &f.holder_key,
        &f.holder_root,
        &self_root,
        body,
        &NeverHook,
    );
    assert_gate_denied(&r);
}
