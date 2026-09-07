//! community-affairs C0（组织创世 / 域身份 / 策略链）golden vectors 消费测试：
//! 加载 `../spec/vectors/community.json` 中以下 case 组逐字节断言：
//! - `orgGenesis`（org-genesis §1/§2/§15：创世 canonical 载荷、新 orgId、
//!   签名验证、orgAddress 互绑复算）
//! - `orgDomainIdentity`（org-genesis §4：HMAC-SHA512 域身份派生固定值，
//!   跨域不可关联）
//! - `policyChain`（org-genesis §5：policyHash0 → 修订链 policyHash1 逐字节）
//!
//! 向量来源：`code/spec/gen-community-vectors.mjs` 自产回填（生成器内已自检
//! 一遍，本测试为消费侧独立复算，与 community_orgmember_vectors.rs 同口径）。
//! 规格权威：wiki/protocol/community/org-genesis.md。

use ed25519_dalek::SigningKey;
use serde_json::Value;
use spark_core::org::{
    GenesisPolicyRecord, OrgDomainIdentity, PolicyRevisionRecord, PolicyVersion, SigningPolicy,
    genesis_org_id, genesis_policy_hash, genesis_sign_payload, is_genesis_form_org_id,
    org_address_from_public_key, policy_revision_hash, policy_revision_payload,
    sign_genesis_record, verify_genesis_signature, verify_org_address_binding,
};
use spark_core::org::{TransitionDecl, VetoThreshold};

/// 组织根密钥种子字节（与生成器 `keyFromSeed(0x31)` 同口径：32 字节全填充）。
const ORG_ROOT_SEED_BYTE: u8 = 0x31;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

fn org_root_key() -> SigningKey {
    SigningKey::from_bytes(&[ORG_ROOT_SEED_BYTE; 32])
}

/// orgGenesis 组：向量 record 反序列化 → 载荷/orgId/policyHash0/签名/互绑
/// 逐项复算逐字节一致；并按 input（种子字节 + createdAt）从零重建记录，
/// 重签后与向量 record 完全相等。
#[test]
fn org_genesis_vectors() {
    let v = vectors();
    let group = &v["orgGenesis"];
    let expect = &group["expect"];
    let record: GenesisPolicyRecord =
        serde_json::from_value(expect["record"].clone()).expect("parse genesis record");

    // canonical 载荷（剔除 sig）逐字节
    assert_eq!(
        genesis_sign_payload(&record).unwrap(),
        expect["payload"].as_str().unwrap()
    );
    // 新 orgId = org_ + policyHash0（自认证形态）
    assert_eq!(
        genesis_policy_hash(&record).unwrap(),
        expect["policyHash0"].as_str().unwrap()
    );
    assert_eq!(
        genesis_org_id(&record).unwrap(),
        expect["orgId"].as_str().unwrap()
    );
    assert!(is_genesis_form_org_id(expect["orgId"].as_str().unwrap()));
    // 根签名验证 + orgAddress 互绑（C1）复算
    assert!(verify_genesis_signature(&record), "创世签名须验证通过");
    assert!(verify_org_address_binding(&record), "orgAddress 互绑须成立");
    assert_eq!(record.org_address.len(), 55, "org-address §15：55 字符");
    assert_eq!(
        record.org_address,
        expect["orgAddress"].as_str().unwrap()
    );

    // 从 input 从零重建：种子派生根密钥 → 地址互绑 → 签名 → 与向量 record 相等
    assert_eq!(
        group["input"]["orgRootSeedByte"].as_u64().unwrap(),
        ORG_ROOT_SEED_BYTE as u64
    );
    let created_at = group["input"]["createdAt"].as_i64().unwrap();
    let root_key = org_root_key();
    let root_public_key = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        root_key.verifying_key().to_bytes(),
    );
    assert_eq!(root_public_key, record.root_public_key, "根公钥固定值");
    let mut rebuilt = GenesisPolicyRecord {
        genesis_v: 1,
        name: record.name.clone(),
        description: record.description.clone(),
        domain_type: record.domain_type.clone(),
        root_public_key,
        org_address: org_address_from_public_key(&root_key.verifying_key().to_bytes()),
        signing_policy: SigningPolicy::AnyAdmin,
        transition: Some(TransitionDecl {
            kind: "delayed-veto".to_string(),
            delay_ms: 259_200_000,
            veto_threshold: VetoThreshold { count: 1 },
            when_members_exceed: 1,
        }),
        born_of: None,
        created_by: record.created_by.clone(),
        created_at,
        sig: String::new(),
    };
    sign_genesis_record(&mut rebuilt, &root_key);
    assert_eq!(rebuilt, record, "重建记录与向量 record 逐字段一致");
}

/// orgDomainIdentity 组（org-genesis §4）：固定根私钥 + 域串 → 域公钥/身份
/// 固定值；两域派生身份互不相同（跨域不可关联）。
#[test]
fn org_domain_identity_vectors() {
    let v = vectors();
    let expect = &v["orgDomainIdentity"]["expect"];
    let root_key = org_root_key();
    let mut identities = Vec::new();
    for case in ["community", "affair"] {
        let domain = expect[case]["domain"].as_str().unwrap();
        let derived = OrgDomainIdentity::derive(&root_key, domain);
        assert_eq!(
            derived.public_key_b64(),
            expect[case]["publicKey"].as_str().unwrap(),
            "{case} 域公钥固定值"
        );
        assert_eq!(
            derived.identity(),
            expect[case]["identity"].as_str().unwrap(),
            "{case} 域身份固定值"
        );
        identities.push(derived.identity());
    }
    assert_ne!(identities[0], identities[1], "跨域身份不可关联");
}

/// policyChain 组（org-genesis §5）：创世 policyHash0 与 orgGenesis 组互链，
/// 修订载荷/policyHash1 逐字节，sigSet.subject 绑定新哈希。
#[test]
fn policy_chain_vectors() {
    let v = vectors();
    let expect = &v["policyChain"]["expect"];
    let revision: PolicyRevisionRecord =
        serde_json::from_value(expect["revision"].clone()).expect("parse policy revision");

    // 修订载荷（剔除 sigSet）与 policyHash1 逐字节
    assert_eq!(
        policy_revision_payload(&revision).unwrap(),
        expect["revisionPayload"].as_str().unwrap()
    );
    assert_eq!(
        policy_revision_hash(&revision).unwrap(),
        expect["policyHash1"].as_str().unwrap()
    );
    // sigSet 承诺绑定：subject = 新策略文档哈希，policyHash = 修订前策略哈希
    assert_eq!(
        revision.sig_set.subject,
        expect["policyHash1"].as_str().unwrap()
    );
    assert_eq!(
        revision.sig_set.policy_hash,
        expect["policyHash0"].as_str().unwrap()
    );

    // 与 orgGenesis 组互链：prevPolicyHash = 创世 policyHash0（独立复算）
    let genesis: GenesisPolicyRecord =
        serde_json::from_value(v["orgGenesis"]["expect"]["record"].clone())
            .expect("parse genesis record");
    assert_eq!(
        revision.prev_policy_hash,
        genesis_policy_hash(&genesis).unwrap()
    );
    assert_eq!(
        revision.prev_policy_hash,
        expect["policyHash0"].as_str().unwrap()
    );

    // PolicyVersion 枚举口径：创世为 seq 0 链尾，修订挂接到链上
    let versions = [
        PolicyVersion::Genesis(genesis),
        PolicyVersion::Revision(revision),
    ];
    assert_eq!(versions[0].seq(), 0);
    assert_eq!(versions[0].prev_policy_hash(), None);
    assert_eq!(versions[1].seq(), 1);
    assert_eq!(
        versions[1].prev_policy_hash().unwrap(),
        versions[0].policy_hash().unwrap()
    );
    assert_eq!(
        versions[1].policy_hash().unwrap(),
        expect["policyHash1"].as_str().unwrap()
    );
    assert!(matches!(
        versions[1].signing_policy(),
        SigningPolicy::MOfN { m: 2, n: 3 }
    ));
}
