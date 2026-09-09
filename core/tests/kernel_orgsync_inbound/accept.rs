//! A17 准入策略声明（`org:accept:`，membership §4.5 / policy §9）orgsync
//! 入站接线测试：键域过 B3 白名单（org:structure 内建键域）→ 入站合入裁决
//! （`adjudicate_incoming_accept_policy` 五步链把关）——无签名包/键 orgId
//! 错位的发布件拒收不落库（fail-closed）；他组织键域整批拒收
//! （key-out-of-collection）。
//!
//! Accept 路径（合法 OrgSigSet 发布件）由 kernel 级集成测试覆盖
//! （kernel_credential_policy_ops.rs `accept_policy_publish_pub_delay_and_merge`
//! ——发布 → 同函数裁决 Accept/KeepCurrent/Rejected 全链）。

use super::*;

const COL_FULL: &str = "org:structure@v1";

fn accept_policy_json(org_id: &str) -> serde_json::Value {
    serde_json::to_value(spark_core::policy::AcceptPolicyRecord {
        accept_v: spark_core::policy::ACCEPT_POLICY_V,
        org_id: org_id.to_string(),
        accept_credentials: vec![spark_core::policy::AcceptCredentialRule {
            cred_type: "member".to_string(),
            issuer_trust: "org_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
                .to_string(),
        }],
        version: 1,
        updated_at: NOW,
        effective_at: NOW,
        sig_set: None, // 发布件必须携带 OrgSigSet——None 即拒收
    })
    .unwrap()
}

fn setup() -> (MemoryStorage, SigningKey, String, String) {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut b = MemoryStorage::new();
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();
    (b, a_key, a_root, b_root)
}

fn deliver_structure_data(
    b: &mut MemoryStorage,
    a_key: &SigningKey,
    a_root: &str,
    b_root: &str,
    records: &[spark_core::sync::orgsync::OrgsyncRecord],
) -> spark_core::kernel::InboundDmResult {
    let body = build_orgsync_data_batch(ORG_ID, COL_FULL, records, 0, 1);
    deliver_orgsync(
        b,
        b_root,
        "B",
        a_key,
        a_root,
        b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        body,
        "peer-a",
        "node-b",
    )
}

fn record_of(key: &str, value: serde_json::Value) -> spark_core::sync::orgsync::OrgsyncRecord {
    spark_core::sync::orgsync::OrgsyncRecord {
        key: key.to_string(),
        value,
        meta: DocMeta {
            vv: [("node-a".to_string(), 1)].into_iter().collect(),
            ts: NOW,
            node_id: Some("node-a".to_string()),
            tombstone: None,
        },
        dseq: None,
    }
}

/// 无签名包的「发布件」入站：键域过白名单（应答 ok）但合入裁决拒收
/// （不落库）——发布件必须携带 OrgSigSet 且 subject 绑定记录哈希。
#[test]
fn accept_policy_inbound_unsigned_rejected_not_stored() {
    let (mut b, a_key, a_root, b_root) = setup();
    let key = spark_core::policy::accept_policy_key(ORG_ID);
    let records = [record_of(&key, accept_policy_json(ORG_ID))];
    let r = deliver_structure_data(&mut b, &a_key, &a_root, &b_root, &records);
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "org:accept: 键域过 B3 白名单（org:structure 内建键域）"
    );
    assert!(
        b.get(&key).unwrap().is_none(),
        "无 OrgSigSet 的发布件拒收不落库（fail-closed）"
    );
}

/// 键 orgId 与本组织不符（他组织的 accept 键混入本组织 org:structure
/// 流量）→ 键域越界整批拒收（16hex 短 orgId 前缀可被 64hex 键
/// starts_with 命中——合入分支再按精确相等拒收，双保险口径见分支注释）。
#[test]
fn accept_policy_inbound_other_org_key_rejected_batch() {
    let (mut b, a_key, a_root, b_root) = setup();
    let other = "org_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    let key = spark_core::policy::accept_policy_key(other);
    let records = [record_of(&key, accept_policy_json(other))];
    let r = deliver_structure_data(&mut b, &a_key, &a_root, &b_root, &records);
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("key-out-of-collection"));
    assert!(b.get(&key).unwrap().is_none());
}
