//! 回填 `code/spec/vectors/community.json` 中 C3（组织作为成员）落地的
//! 占位 case 组（规格登记：org-genesis §7 / org-signature §6）：
//!
//! - `cycleCheck`：固定公开成员关系图 → 成环拒绝/无环放行用例集
//! - `memberKindEnforce`：域类型 × 成员 kind 匹配矩阵（内核硬规则）
//! - `legacyDegraded`：legacy orgId 验证通过但标注 `degraded` 的判定用例
//!   （含创世哈希型对照组：同形态签名包不降级）
//!
//! 其余 case 组一字节不动（read-modify-write upsert 只动这三组 +
//! meta.placeholders 登记回写）。密钥与 meta.actors 同源
//! （orgRoot 0x31、admin1 0x41 / admin2 0x42 / admin3 0x43）。
//!
//! 用法：`cargo run --example gen_community_orgmember_vectors -- [community.json 路径]`
//! 自检：每组以 core 实现实跑（membership_would_cycle / enforce_member_kind /
//! OrgSigSetVerifyContext）比对 expect，任一失败 panic（非零退出）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::credential::{
    ComponentSignature, OrgSigSet, OrgSigSetVerifier, RosterAnchor, RosterCommitment, RosterMember,
};
use spark_core::evidence::normalize_object;
use spark_core::org::{
    DEGRADED_LEGACY_ORG_ID, DomainType, GenesisPolicyRecord, OrgSigSetVerifyContext, PolicyVersion,
    SigningPolicy, TransitionDecl, VetoThreshold, component_sign_payload, enforce_member_kind,
    genesis_org_id, genesis_policy_hash, membership_would_cycle, roster_member_set_hash,
    sign_genesis_record,
};

const NOW: i64 = 1_720_000_000_000;

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

fn identity_of(k: &SigningKey) -> String {
    hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
}

fn admin_roster(admins: &[SigningKey]) -> Vec<RosterMember> {
    admins
        .iter()
        .map(|k| RosterMember {
            identity: identity_of(k),
            role: "admin".to_string(),
            org_user_id: None,
        })
        .collect()
}

/// any-admin 签名包（分量载荷绑定 §2.1 八键）；anchorRoot 为固定替身
/// （与 C2 向量同口径：锚根复算不可由向量自证）。
fn make_sig_set(
    org_id: &str,
    subject: &str,
    policy_hash: &str,
    snapshot: &[RosterMember],
    anchor_root: &str,
    signer: &SigningKey,
) -> OrgSigSet {
    let mut sig_set = OrgSigSet {
        sig_set_v: 1,
        org_id: org_id.to_string(),
        subject: subject.to_string(),
        policy_hash: policy_hash.to_string(),
        roster: RosterCommitment {
            member_set_hash: roster_member_set_hash(snapshot),
            anchor: RosterAnchor {
                org_id: org_id.to_string(),
                anchor_root: anchor_root.to_string(),
                ts: NOW,
            },
            snapshot: Some(snapshot.to_vec()),
        },
        signed_at: NOW,
        signatures: Vec::new(),
    };
    let payload = component_sign_payload(&sig_set);
    sig_set.signatures = vec![ComponentSignature {
        signer: identity_of(signer),
        public_key: b64(&signer.verifying_key().to_bytes()),
        sig: b64(&signer.sign(payload.as_bytes()).to_bytes()),
    }];
    sig_set
}

/// 共同体创世策略记录（域类型 community，any-admin 默认策略）。
fn community_genesis(
    root: &SigningKey,
    org_address: &str,
    created_by: &str,
) -> GenesisPolicyRecord {
    let mut record = GenesisPolicyRecord {
        genesis_v: 1,
        name: "阳光小区".to_string(),
        description: "共同体域成员 kind 与组织签名验证用例".to_string(),
        domain_type: DomainType::Community,
        root_public_key: b64(&root.verifying_key().to_bytes()),
        org_address: org_address.to_string(),
        signing_policy: SigningPolicy::AnyAdmin,
        transition: Some(TransitionDecl {
            kind: "delayed-veto".to_string(),
            delay_ms: 259_200_000,
            veto_threshold: VetoThreshold { count: 1 },
            when_members_exceed: 1,
        }),
        born_of: None,
        created_by: created_by.to_string(),
        created_at: NOW,
        sig: String::new(),
    };
    sign_genesis_record(&mut record, root);
    record
}

fn gen_cycle_check() -> Value {
    // 公开名册线形（域 → 成员组织 id 列表；org-genesis §3.3 关系公开于域名册）：
    //   alpha  名册 = [beta, epsilon]
    //   beta   名册 = [gamma, epsilon]
    //   gamma  名册 = []
    //   delta  名册 = []     （独立组织）
    //   epsilon 名册 = []     （多父：同时加入 alpha 与 beta）
    // 成员→上级边：beta→alpha、epsilon→alpha、gamma→beta、epsilon→beta。
    let alpha = "org_a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a";
    let beta = "org_b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b";
    let gamma = "org_c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c";
    let delta = "org_d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d";
    let epsilon = "org_e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5";
    let roster = json!({
        alpha: [beta, epsilon],
        beta: [gamma, epsilon],
        gamma: [],
        delta: [],
        epsilon: [],
    });
    let cases = [
        // 无环：delta 与 alpha 无任何关系
        ("unrelated-join", delta, alpha, "accept"),
        // 自环：alpha 加入自身
        ("self-join", alpha, alpha, "reject"),
        // 直接环：beta 已加入 alpha，alpha 再加入 beta（A↔B 两环）
        ("direct-cycle", alpha, beta, "reject"),
        // 直接环（镜像）：gamma 已加入 beta，beta 再加入 gamma
        ("direct-cycle-mirrored", beta, gamma, "reject"),
        // 传递环：gamma 的可达祖先集 = {beta, alpha}
        ("transitive-cycle", alpha, gamma, "reject"),
        // 多父无环：epsilon 已加入 alpha/beta；gamma 的祖先集不含 epsilon
        ("multi-parent-dag", epsilon, gamma, "accept"),
    ];
    // 自检：core 实现实跑与 expect 一致
    let roster_map: serde_json::Map<String, Value> =
        serde_json::from_value(roster.clone()).unwrap();
    let parents = |org: &str| -> Vec<String> {
        roster_map
            .iter()
            .filter(|(_, members)| {
                members
                    .as_array()
                    .is_some_and(|a| a.iter().any(|m| m.as_str() == Some(org)))
            })
            .map(|(domain, _)| domain.clone())
            .collect()
    };
    let cases: Vec<Value> = cases
        .into_iter()
        .map(|(name, joiner, target, expect)| {
            let rejected = membership_would_cycle(joiner, target, &parents);
            assert_eq!(
                rejected,
                expect == "reject",
                "cycleCheck self-check failed: {name}"
            );
            json!({ "name": name, "joiner": joiner, "target": target, "expect": expect })
        })
        .collect();
    json!({
        "desc": "固定公开成员关系图（名册线形：域→成员组织）→ 成环拒绝/无环放行用例集。判定口径（org-genesis §3.3）：待加入组织出现在目标域的可达祖先集（沿成员→上级边，含目标域本身）即成环；图无单父约束。消费侧由名册反查上级域闭包后实跑 membership_would_cycle",
        "expect": { "roster": roster, "cases": cases }
    })
}

fn gen_member_kind_enforce() -> Value {
    let cases = [
        (
            "community-accepts-org",
            "community",
            "org",
            "ok",
            Value::Null,
        ),
        ("leaf-accepts-person", "leaf", "person", "ok", Value::Null),
        (
            "community-rejects-person",
            "community",
            "person",
            "reject",
            json!("Community domain only accepts organization members"),
        ),
        (
            "leaf-rejects-org",
            "leaf",
            "org",
            "reject",
            json!("Leaf domain only accepts person members"),
        ),
    ];
    let cases: Vec<Value> = cases
        .into_iter()
        .map(|(name, domain, kind, expect, message)| {
            let domain = match domain {
                "community" => DomainType::Community,
                _ => DomainType::Leaf,
            };
            let kind = match kind {
                "org" => spark_core::org::MemberKind::Org,
                _ => spark_core::org::MemberKind::Person,
            };
            // 自检：core 实现实跑与 expect 一致
            match enforce_member_kind(domain, kind) {
                Ok(()) => assert_eq!(expect, "ok", "memberKindEnforce self-check: {name}"),
                Err(e) => {
                    assert_eq!(expect, "reject", "memberKindEnforce self-check: {name}");
                    assert_eq!(e.to_string(), message.as_str().unwrap());
                }
            }
            let mut case = json!({ "name": name, "domain": domain.as_str(), "kind": kind.as_str(), "expect": expect });
            if expect == "reject" {
                case["message"] = message;
            }
            case
        })
        .collect();
    json!({
        "desc": "域类型 × 成员 kind 匹配矩阵（org-genesis §3.2 内核硬规则：community 域只接受组织成员、leaf 域只接受个人成员；加入验证时强制，任何组织策略不得覆盖）。缺省向后兼容：成员条目无 kind 键 = person、组织记录无 domainType 键 = leaf",
        "expect": { "cases": cases }
    })
}

/// legacy 组织：orgId 保持 16hex 随机形态，自愿发布创世策略记录
/// （org-genesis §2.2：orgId 不变，记录仅作策略公开锚）。
fn gen_legacy_degraded(v: &Value) -> Value {
    let org_root = key(0x31);
    let admins = [key(0x41), key(0x42), key(0x43)];
    let created_by = v["meta"]["actors"]["personA"]["identity"]
        .as_str()
        .expect("personA identity")
        .to_string();
    let genesis = community_genesis(&org_root, &"a".repeat(55), &created_by);
    let policy_hash0 = genesis_policy_hash(&genesis).unwrap();
    let snapshot = admin_roster(&admins);
    let subject = hex::encode(Sha256::digest(
        normalize_object(&json!({ "demo": "legacy-degraded-subject" })).as_bytes(),
    ));
    let anchor_root = hex::encode(Sha256::digest(b"spark:legacy-degraded-anchor"));

    // legacy 组：orgId = org_<16hex>；同形态签名包验证通过但标注降级
    let legacy_org_id = format!("org_{}", "5e".repeat(8));
    let legacy_sig_set = make_sig_set(
        &legacy_org_id,
        &subject,
        &policy_hash0,
        &snapshot,
        &anchor_root,
        &admins[0],
    );

    // 创世哈希型对照组：同一创世记录，orgId = org_<64hex>（= policyHash₀ 前缀
    // org_）；同形态签名包不降级（自认证闭环成立）
    let genesis_org_id = genesis_org_id(&genesis).unwrap();
    let genesis_sig_set = make_sig_set(
        &genesis_org_id,
        &subject,
        &policy_hash0,
        &snapshot,
        &anchor_root,
        &admins[0],
    );

    // 自检：core 线上验证器实跑与 expect 一致（锚根为固定替身，按已知值放行）
    let policies = [PolicyVersion::Genesis(genesis.clone())];
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &|_| true,
        roster_lookup: &|_| None,
    };
    let legacy_verdict = ctx
        .verify_detailed(&legacy_sig_set)
        .expect("legacy verifies");
    assert!(legacy_verdict.degraded);
    assert_eq!(legacy_verdict.degraded_reason, Some(DEGRADED_LEGACY_ORG_ID));
    assert!(ctx.verify_org_sig_set(&legacy_sig_set));
    let genesis_verdict = ctx
        .verify_detailed(&genesis_sig_set)
        .expect("genesis verifies");
    assert!(!genesis_verdict.degraded);

    json!({
        "desc": "legacy orgId（org_<16hex>）验证结果标注 degraded 的判定用例（org-signature §5.1）：legacy orgId 无自认证性，五步验证链可完整通过（名册承诺的可信锚退化为传输层可信锚），裁决须如实标注 degraded=\"legacy-orgId\"，接受与否归消费方；创世哈希型对照组同形态签名包闭环至创世记录（orgId 哈希段 == policyHash₀），不降级",
        "expect": {
            "genesis": serde_json::to_value(&genesis).unwrap(),
            "policyHash0": policy_hash0,
            "subject": subject,
            "anchorRoot": anchor_root,
            "signedAt": NOW,
            "cases": [
                {
                    "name": "legacy-orgId-degraded",
                    "orgId": legacy_org_id,
                    "sigSet": serde_json::to_value(&legacy_sig_set).unwrap(),
                    "expect": { "verifies": true, "degraded": "legacy-orgId" }
                },
                {
                    "name": "genesis-form-clean",
                    "orgId": genesis_org_id,
                    "sigSet": serde_json::to_value(&genesis_sig_set).unwrap(),
                    "expect": { "verifies": true, "degraded": null }
                }
            ]
        }
    })
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

    let legacy_degraded = gen_legacy_degraded(&doc);
    let out = doc.as_object_mut().expect("top-level object");
    out.insert("cycleCheck".to_string(), gen_cycle_check());
    out.insert("memberKindEnforce".to_string(), gen_member_kind_enforce());
    out.insert("legacyDegraded".to_string(), legacy_degraded);
    // 占位登记回写：C3 三组已产出
    out.entry("meta").and_modify(|meta| {
        meta["placeholders"]["desc"] = json!(
            "依赖实现的 case 组（登记于规格文档末节）：ladderDerive（affair §12，C6）；readGate.verifyChain 的 policyRef 求值（read-gate §6，C5）。staticCheck / resolutionReplay（affair §12）与 metaBasisVerify / metaArbitrate（affair-metadata §7）已由 C1 core/examples/gen_community_affair_vectors.rs 自产回填；credential.trustTimeline 与 readGate.verifyChain（第 1–4 步）已由 C2 core/examples/gen_credential_vectors.rs 自产回填；cycleCheck / memberKindEnforce（org-genesis §7）与 legacyDegraded（org-signature §6）已由 C3 core/examples/gen_community_orgmember_vectors.rs 自产回填。"
        );
    });
    doc["_comment"] = json!(
        "community-affairs golden vectors（wiki/protocol/community/ 协议）。C0 组由 code/spec/gen-community-vectors.mjs（Node 参考实现）自产；C1 组（staticCheck/resolutionReplay/metaBasisVerify/metaArbitrate）由 code/core/examples/gen_community_affair_vectors.rs 自产并逐字节复核 C0 affair 组；C2 组由 code/core/examples/gen_credential_vectors.rs 自产；C3 组（cycleCheck/memberKindEnforce/legacyDegraded）由 code/core/examples/gen_community_orgmember_vectors.rs 自产。消费：core/tests/community_affair_vectors.rs（C1）、community_credential_vectors.rs（C2）、community_orgmember_vectors.rs（C3）。"
    );

    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap() + "\n")
        .expect("write community.json");
    println!(
        "OK: community.json updated (C3 groups: cycleCheck / memberKindEnforce / legacyDegraded), other groups untouched."
    );
}
