//! `affair_evi_ops.rs` 的内联单元测试（affair_ops_tests.rs 同款拆分先例）。
//!
//! 覆盖（affair-model §六验收）：
//! - 效力相关组织反查（声明面）：现行非撤销 / 键-文一致 / 同事务过滤；
//! - 生效判定路径「先条目后锚」顺序：Apply 生效决议 → 本机链 evi:resolution
//!   条目 → 锚记录覆盖新条目；幂等（二次消费不重写、链高不变）；
//! - 集成：两节点相同锚定输入下本机链确定性出现逐字节相同条目；凭条目 +
//!   conclusionHash 对决议原文独立证明存在（内容绑定）。
//!
//! 夹具模式同 affair_ops_tests.rs：固定密钥/时间戳直写存储键与存证锚定，
//! 绕过复制面入站（读/编排路径只消费本地副本与存证链）。

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::affair::{
    RESOLUTION_ENTRY_KIND, affair_op_key, affair_record_key, compute_affair_id, compute_op_hash,
    conclusion_hash, genesis_sign_payload, op_sign_payload, parse_effect_grant, rules_hash,
};
use crate::kernel::KernelConfig;

const PASSWORD: &str = "correct-horse-battery";
/// 固定历史时刻（真实 system_now_ms 远大于此 → 公示期必满，决议必生效）。
const T0: i64 = 1_720_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

struct FixedKey {
    signing_key: SigningKey,
    public_key: String,
    identity: String,
}

fn fixed_key(seed: u8) -> FixedKey {
    let signing_key = SigningKey::from_bytes(&[seed; 32]);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    FixedKey {
        signing_key,
        public_key: base64::engine::general_purpose::STANDARD.encode(public_key_bytes),
        identity: hex::encode(Sha256::digest(public_key_bytes)),
    }
}

fn sign(key: &FixedKey, payload: &str) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(key.signing_key.sign(payload.as_bytes()).to_bytes())
}

/// 基线规则：op-count content 1 关闭条件 + 24h 公示期 + delayed-veto 规则修改。
fn baseline_rules() -> Value {
    json!({
        "engine": "b1",
        "closeConditions": [{ "type": "op-count", "opType": "content", "count": 1 }],
        "pubPeriod": { "delayMs": DAY, "vetoThreshold": { "count": 1 } },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
        "exec": null,
    })
}

fn make_genesis(initiator: &FixedKey) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": "换届事务", "summary": "", "tags": [],
        "initiator": { "kind": "person", "identity": initiator.identity, "publicKey": initiator.public_key },
        "rules": baseline_rules(),
        "initialVoters": [initiator.identity], "refs": [], "createdAt": T0,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign(initiator, &payload));
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

/// 通用操作构造；`org_sig` 非空时 actor 以 kind=org 携带（组织决议形态）。
fn make_op(
    affair_id: &str,
    key: &FixedKey,
    op_type: &str,
    payload: Value,
    prev_op_hash: &str,
    declared_at: i64,
    org_sig: Option<&Value>,
) -> (Value, String) {
    let actor = match org_sig {
        Some(sig_set) => json!({ "kind": "org", "identity": key.identity, "publicKey": key.public_key, "orgSig": sig_set }),
        None => json!({ "kind": "person", "identity": key.identity, "publicKey": key.public_key }),
    };
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": op_type, "payload": payload,
        "actor": actor, "declaredAt": declared_at,
    });
    let sig_payload = op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(sign(key, &sig_payload));
    let op_hash = compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

fn grant_value(org_id: &str, affair_id: &str, scope: &str, revoked: bool) -> Value {
    let mut value = json!({
        "grantV": 1, "orgId": org_id, "affairId": affair_id, "scope": scope,
        "declaredAt": T0, "sigSet": { "signatures": [] },
    });
    if revoked {
        value["revoked"] = json!(true);
    }
    value
}

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

fn anchor(kernel: &mut Kernel, domain: &str, collection: &str, id: &str, payload: &Value, ts: i64) {
    let storage = kernel.require_storage_raw_mut().unwrap();
    append_evidence(
        storage,
        NewEvidenceEntry::from_parts(
            domain,
            collection,
            id,
            EvidenceOp::Put,
            Some(payload),
            None,
            ts,
            "test-node",
        ),
    )
    .unwrap();
}

fn anchor_op(kernel: &mut Kernel, affair_id: &str, op_hash: &str, op: &Value, ts: i64) {
    let domain = format!("affair:{affair_id}");
    anchor(kernel, &domain, "ops", op_hash, op, ts);
}

/// 换届事务夹具产物：创世 + content 操作 + 组织决议（org actor 携带 orgSig）。
struct AffairFixture {
    genesis: Value,
    content: Value,
    content_hash: String,
    resolution: Value,
    resolution_hash: String,
    org_sig: Value,
}

fn affair_fixture(initiator: &FixedKey, affair_id: &str, genesis: &Value) -> AffairFixture {
    let org_sig = json!({
        "sigSetV": 1, "orgId": format!("org_{}", "ab".repeat(32)),
        "subject": "77".repeat(32), "policyHash": "66".repeat(32),
        "signatures": [ { "identity": initiator.identity, "sig": "c3R1Yg==" } ],
    });
    let (content, content_hash) = make_op(
        affair_id,
        initiator,
        "content",
        json!({ "kind": "post", "text": "换届提名" }),
        affair_id,
        T0 + 500,
        None,
    );
    let (resolution, resolution_hash) = make_op(
        affair_id,
        initiator,
        "resolution",
        json!({
            "result": "passed",
            "condition": { "type": "op-count", "opType": "content", "count": 1 },
            "countedOps": [content_hash.clone()],
            "rulesHash": rules_hash(&genesis["rules"]),
            "pubPeriod": { "delayMs": DAY },
        }),
        &content_hash,
        T0 + 1_000,
        Some(&org_sig),
    );
    AffairFixture {
        genesis: genesis.clone(),
        content,
        content_hash,
        resolution,
        resolution_hash,
        org_sig,
    }
}

/// 向内核直写夹具（同一组锚定时刻 → 各节点条目逐字节相同的前提）。
fn write_fixture(kernel: &mut Kernel, affair_id: &str, org_id: &str, grant: &Value, f: &AffairFixture) {
    let storage = kernel.require_storage_raw_mut().unwrap();
    storage
        .put(&affair_record_key(affair_id), &f.genesis.to_string())
        .unwrap();
    storage
        .put(&affair_op_key(affair_id, &f.content_hash), &f.content.to_string())
        .unwrap();
    storage
        .put(
            &affair_op_key(affair_id, &f.resolution_hash),
            &f.resolution.to_string(),
        )
        .unwrap();
    let parsed = parse_effect_grant(grant).unwrap();
    storage.put(&parsed.key, &grant.to_string()).unwrap();
    let domain = format!("affair:{affair_id}");
    anchor(kernel, &domain, "genesis", affair_id, &f.genesis, T0);
    anchor_op(kernel, affair_id, &f.content_hash, &f.content, T0 + 500);
    anchor_op(kernel, affair_id, &f.resolution_hash, &f.resolution, T0 + 10_000);
    // 声明锚定严格早于决议锚定（事先性）
    anchor(kernel, org_id, "effectgrant", "roster", grant, T0 + 5_000);
}

/// 扫描本机链上的 evi:resolution 条目（domain=orgId、collection=resolution）。
fn resolution_entries(kernel: &Kernel, org_id: &str) -> Vec<crate::evidence::EvidenceEntry> {
    let storage = kernel.require_storage().unwrap();
    let height = get_evidence_height(storage).unwrap();
    let mut out = Vec::new();
    for seq in 1..=height {
        if let Some(entry) = get_evidence_entry(storage, seq).unwrap()
            && entry.domain == org_id
            && entry.collection == RESOLUTION_ENTRY_COLLECTION
        {
            out.push(entry);
        }
    }
    out
}

/// 效力相关组织反查（声明面）：只回收「现行非撤销 + 键-文一致 + 同事务」
/// 的声明组织；撤销 / 他事务 / 键-文不符 / 损坏记录一律不回收；纯讨论事务
/// （无声明）→ 空集。
#[test]
fn effect_orgs_reverse_lookup_declaration_surface() {
    let (_dir, mut kernel) = unlocked_kernel();
    let affair_a = "aa".repeat(32);
    let affair_b = "bb".repeat(32);
    let org_1 = format!("org_{}", "11".repeat(32));
    let org_2 = format!("org_{}", "22".repeat(32));
    let org_3 = format!("org_{}", "33".repeat(32));

    let g1 = grant_value(&org_1, &affair_a, "roster", false);
    let g1b = grant_value(&org_1, &affair_b, "policy", false); // 他事务
    let g2 = grant_value(&org_2, &affair_a, "policy", false);
    let g3_revoked = grant_value(&org_3, &affair_a, "roster", true); // 已撤销
    {
        let storage = kernel.require_storage_raw_mut().unwrap();
        for grant in [&g1, &g1b, &g2, &g3_revoked] {
            let parsed = parse_effect_grant(grant).unwrap();
            storage.put(&parsed.key, &grant.to_string()).unwrap();
        }
        // 键-文不符（内容属 org_2 的声明挂到别的键下）→ fail-closed 跳过
        storage
            .put(
                &format!("{EFFECT_GRANT_PREFIX}{org_3}:{affair_a}:policy"),
                &g2.to_string(),
            )
            .unwrap();
        // 损坏记录 → 跳过
        storage
            .put(
                &format!("{EFFECT_GRANT_PREFIX}{org_3}:{affair_a}:create"),
                "{not json",
            )
            .unwrap();
    }

    let orgs = kernel.affair_effect_orgs(&affair_a).unwrap();
    assert_eq!(orgs, vec![org_1.clone(), org_2.clone()]);
    // 他事务只见自己的声明组织；纯讨论事务 → 空集（不写 evi:resolution）
    assert_eq!(kernel.affair_effect_orgs(&affair_b).unwrap(), vec![org_1]);
    assert!(
        kernel
            .affair_effect_orgs(&"cc".repeat(32))
            .unwrap()
            .is_empty()
    );
    assert!(kernel.affair_effect_orgs("zz").is_err());
}

/// 生效判定路径「先条目后锚」顺序：Apply 生效决议 → 本机链 evi:resolution
/// 条目（sigSet 原样内嵌、effectiveTs = 决议锚定时刻）→ 锚记录链头承诺覆盖
/// 新条目；二次消费幂等（already-on-chain、链高不变、锚不重复）。
#[test]
fn seal_resolution_entries_entry_then_anchor() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x71);
    let org_id = format!("org_{}", "ab".repeat(32));
    let (genesis, affair_id) = make_genesis(&initiator);
    let f = affair_fixture(&initiator, &affair_id, &genesis);
    let grant = grant_value(&org_id, &affair_id, "roster", false);
    write_fixture(&mut kernel, &affair_id, &org_id, &grant, &f);
    let height_before = get_evidence_height(kernel.require_storage().unwrap()).unwrap();
    assert!(kernel.evidence_anchors(&org_id).unwrap().is_empty());

    let out = kernel.affair_apply_org_effects(&org_id, &affair_id).unwrap();
    let evidence = out["resolutionEvidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["resolutionOpHash"], json!(f.resolution_hash));
    assert_eq!(evidence[0]["action"], "written");
    assert_eq!(
        evidence[0]["conclusionHash"].as_str().unwrap(),
        conclusion_hash(&f.resolution["payload"])
    );

    // 条目落链（domain=orgId、collection=resolution、id=决议 opHash）
    let entries = resolution_entries(&kernel, &org_id);
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.id, f.resolution_hash);
    let height_after = get_evidence_height(kernel.require_storage().unwrap()).unwrap();
    assert_eq!(entry.seq, height_after, "条目是链头（其后无其他写入）");
    assert!(height_after > height_before);
    assert!(verify_chain(&kernel));

    // 载荷字段（§6.3 线形）：条目只承诺 payloadHash，按夹具重建载荷比对
    let expected_payload = resolution_entry_payload(
        &affair_id,
        &f.resolution_hash,
        &f.resolution["payload"],
        Some(&f.org_sig),
        T0 + 10_000,
    );
    assert_entry_payload(entry, &expected_payload);
    assert_eq!(expected_payload["kind"], json!(RESOLUTION_ENTRY_KIND));
    assert_eq!(expected_payload["affairId"], json!(affair_id));
    assert_eq!(expected_payload["subject"], json!(f.resolution_hash));
    assert_eq!(
        expected_payload["conclusionHash"].as_str().unwrap(),
        conclusion_hash(&f.resolution["payload"])
    );
    // sigSet = 决议 actor.orgSig 原样内嵌；effectiveTs = 决议锚定时刻
    assert_eq!(expected_payload["sigSet"], f.org_sig);
    assert_eq!(expected_payload["effectiveTs"], json!(T0 + 10_000));

    // 后锚：锚记录链头承诺覆盖新条目（headSeq == 链高 == 条目 seq）
    let anchors = kernel.evidence_anchors(&org_id).unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].head_seq, height_after);
    assert!(anchors[0].head_seq >= entry.seq);

    // 幂等：二次消费 → already-on-chain，链高与锚不变
    let out = kernel.affair_apply_org_effects(&org_id, &affair_id).unwrap();
    let evidence = out["resolutionEvidence"].as_array().unwrap();
    assert_eq!(evidence[0]["action"], "already-on-chain");
    assert_eq!(
        get_evidence_height(kernel.require_storage().unwrap()).unwrap(),
        height_after
    );
    assert_eq!(resolution_entries(&kernel, &org_id).len(), 1);
    assert_eq!(kernel.evidence_anchors(&org_id).unwrap().len(), 1);
}

/// 集成（affair-model §六）：换届事务生效 → 两成员节点本机链确定性出现
/// 逐字节相同的 evi:resolution 条目（相同锚定输入）→ 各自自锚（锚记录覆盖
/// 条目；锚本身经 orgsync 全员流动为既有通道，不在本测试面）→ 事务本体
/// 消亡后，凭条目 + conclusionHash 对决议原文独立证明决议存在（内容绑定）。
#[test]
fn two_nodes_deterministic_self_write_and_independent_proof() {
    let initiator = fixed_key(0x72);
    let org_id = format!("org_{}", "ab".repeat(32));
    let (genesis, affair_id) = make_genesis(&initiator);
    let f = affair_fixture(&initiator, &affair_id, &genesis);
    let grant = grant_value(&org_id, &affair_id, "roster", false);

    let (_dir_a, mut node_a) = unlocked_kernel();
    let (_dir_b, mut node_b) = unlocked_kernel();
    write_fixture(&mut node_a, &affair_id, &org_id, &grant, &f);
    write_fixture(&mut node_b, &affair_id, &org_id, &grant, &f);

    node_a.affair_apply_org_effects(&org_id, &affair_id).unwrap();
    node_b.affair_apply_org_effects(&org_id, &affair_id).unwrap();

    // 两节点本机链各出现同一 evi:resolution（条目时间戳/nodeId 为本地量，
    // 逐字节相同的是载荷——canonical 一致 ⇒ payloadHash 一致）
    let entry_a = resolution_entries(&node_a, &org_id);
    let entry_b = resolution_entries(&node_b, &org_id);
    assert_eq!(entry_a.len(), 1);
    assert_eq!(entry_b.len(), 1);
    assert_eq!(entry_a[0].payload_hash, entry_b[0].payload_hash);
    let payload_a = resolution_entry_payload(
        &affair_id,
        &f.resolution_hash,
        &f.resolution["payload"],
        Some(&f.org_sig),
        T0 + 10_000,
    );
    assert_entry_payload(&entry_a[0], &payload_a);
    assert_entry_payload(&entry_b[0], &payload_a);
    // 各自自写自锚：锚记录链头承诺覆盖本机条目
    for node in [&node_a, &node_b] {
        let anchors = node.evidence_anchors(&org_id).unwrap();
        assert_eq!(anchors.len(), 1);
        assert!(anchors[0].head_seq >= entry_a[0].seq);
        assert!(verify_chain(node));
    }

    // 本体消亡后的独立证明：只持条目 + 决议原文，重算 conclusionHash 证
    // 内容绑定；条目 subject 定位决议 id；sigSet 内嵌佐证组织签名面
    assert_eq!(payload_a["subject"], json!(f.resolution_hash));
    assert_eq!(
        payload_a["conclusionHash"].as_str().unwrap(),
        conclusion_hash(&f.resolution["payload"]),
        "持决议原文重算 conclusionHash：内容绑定"
    );
    assert_eq!(payload_a["sigSet"], f.org_sig);
}

/// 链校验（verify_evidence_chain 的便捷包装）。
fn verify_chain(kernel: &Kernel) -> bool {
    crate::evidence::verify_evidence_chain(kernel.require_storage().unwrap()).unwrap()
}

/// 条目载荷比对（条目只承诺 payloadHash——按夹具重建载荷，哈希相等即
/// 「链上条目承载该内容」，与生产消费面同口径）。
fn assert_entry_payload(entry: &crate::evidence::EvidenceEntry, expected_payload: &Value) {
    assert_eq!(
        entry.payload_hash.as_deref(),
        crate::evidence::build_evidence_payload_hash(Some(expected_payload)).as_deref(),
        "链上条目 payloadHash 与重建载荷不符"
    );
}
