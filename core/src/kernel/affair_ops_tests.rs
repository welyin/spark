//! `affair_ops.rs` 的内联单元测试（拆分至独立文件，data_ops_tests.rs 同款先例）。
//!
//! 覆盖各生产求值链：规则链 replay（`affair_read_rules`）、决议复算
//! （`affair_read_resolution`）、§9 投票前快照名册线上化（snapshot 产生 +
//! 门槛基数注入）、vote 形态规则修改的快照名册驱动、执行状态机
//! （`affair_read_exec`）与 exec-report 提交校验入口、组织效力钩子的回执
//! 消费编排（`affair_apply_org_effects`）。
//!
//! 夹具模式：固定密钥/时间戳构造创世 + 操作 + 声明记录 + 存证锚定（直写
//! 存储键，绕过复制面入站——读路径只读本地副本与存证链）；提交校验入口走
//! 生产 `affair_follow`/`affair_submit_op` 真实入站。

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::affair::{
    affair_op_key, apply_rule_patch, compute_affair_id, compute_op_hash, effect_grant_key,
    genesis_sign_payload, op_sign_payload, rules_hash,
};
use crate::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence};
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

fn actor_json(key: &FixedKey) -> Value {
    json!({ "kind": "person", "identity": key.identity, "publicKey": key.public_key })
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

fn make_genesis_with(initiator: &FixedKey, title: &str, rules: Value, initial_voters: Value) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": title, "summary": "", "tags": [],
        "initiator": actor_json(initiator),
        "rules": rules,
        "initialVoters": initial_voters, "refs": [], "createdAt": T0,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign(initiator, &payload));
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

fn make_genesis(initiator: &FixedKey) -> (Value, String) {
    make_genesis_with(
        initiator,
        "效力钩子测试事务",
        baseline_rules(),
        json!([initiator.identity]),
    )
}

/// 通用操作构造（签名 + opHash 复算）。
fn make_op(
    affair_id: &str,
    key: &FixedKey,
    op_type: &str,
    payload: Value,
    prev_op_hash: &str,
    declared_at: i64,
) -> (Value, String) {
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": op_type, "payload": payload,
        "actor": actor_json(key), "declaredAt": declared_at,
    });
    let sig_payload = op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(sign(key, &sig_payload));
    let op_hash = compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

fn make_content_op(affair_id: &str, key: &FixedKey, prev: &str, declared_at: i64) -> (Value, String) {
    make_op(
        affair_id,
        key,
        "content",
        json!({ "kind": "post", "text": "内容" }),
        prev,
        declared_at,
    )
}

/// 决议操作：默认条件 = op-count content 1（计入操作由参数给出），
/// rulesHash 取参数（须为 replay 链上真实版本哈希）。
fn make_resolution_op(
    affair_id: &str,
    key: &FixedKey,
    prev: &str,
    condition: Value,
    counted_ops: Vec<String>,
    rules_hash: &str,
    declared_at: i64,
) -> (Value, String) {
    make_op(
        affair_id,
        key,
        "resolution",
        json!({
            "result": "passed",
            "condition": condition,
            "countedOps": counted_ops,
            "rulesHash": rules_hash,
            // 公示期下限 24h（§6.2）：低于下限的 payload 在解析期即拒
            "pubPeriod": { "delayMs": DAY },
        }),
        prev,
        declared_at,
    )
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

/// 直写创世 + 操作 + 指定锚定时刻（绕过复制面入站的读路径夹具）。
struct FixtureWriter<'k> {
    kernel: &'k mut Kernel,
}

impl FixtureWriter<'_> {
    fn put_op(&mut self, affair_id: &str, op_hash: &str, op: &Value) {
        let storage = self.kernel.require_storage_raw_mut().unwrap();
        storage
            .put(&affair_op_key(affair_id, op_hash), &op.to_string())
            .unwrap();
    }

    fn put_genesis(&mut self, affair_id: &str, genesis: &Value) {
        let storage = self.kernel.require_storage_raw_mut().unwrap();
        storage
            .put(&affair_record_key(affair_id), &genesis.to_string())
            .unwrap();
    }

    fn anchor(&mut self, domain: &str, collection: &str, id: &str, payload: &Value, ts: i64) {
        let storage = self.kernel.require_storage_raw_mut().unwrap();
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

    fn anchor_genesis(&mut self, affair_id: &str, genesis: &Value, ts: i64) {
        let domain = format!("affair:{affair_id}");
        self.anchor(&domain, "genesis", affair_id, genesis, ts);
    }

    fn anchor_op(&mut self, affair_id: &str, op_hash: &str, op: &Value, ts: i64) {
        let domain = format!("affair:{affair_id}");
        self.anchor(&domain, "ops", op_hash, op, ts);
    }
}

/// 效力钩子三线判定 + 复算有效决议门控 + 回执标注：apply / notPrior /
/// revoked / grantNotAnchored / resolutionNotEffective 逐 scope × 逐决议覆盖。
#[test]
fn org_effects_three_line_gate() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x11);
    let org_id = format!("org_{}", "ab".repeat(32));
    let (genesis, affair_id) = make_genesis(&initiator);
    let rules_hash = rules_hash(&genesis["rules"]);
    let (c0, c0_hash) = make_content_op(&affair_id, &initiator, &affair_id, T0 + 500);
    let condition = json!({ "type": "op-count", "opType": "content", "count": 1 });
    let (res1, res1_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition.clone(),
        vec![c0_hash.clone()],
        &rules_hash,
        T0 + 1_000,
    );
    let (res2, res2_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition,
        vec![c0_hash.clone()],
        &rules_hash,
        T0 + 2_000,
    );
    let t_res1 = T0 + 10_000;
    let t_res2_far_future = 4_000_000_000_000; // 公示期永不满 → pending

    // 声明记录：roster（事先）/ policy（事后追认）/ create（已撤销）/
    // budget:x（无存证锚定）。
    let grant_roster = grant_value(&org_id, &affair_id, "roster", false);
    let grant_policy = grant_value(&org_id, &affair_id, "policy", false);
    let grant_create_revoked = grant_value(&org_id, &affair_id, "create", true);
    let grant_budget_unanchored = grant_value(&org_id, &affair_id, "budget:x", false);

    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        w.put_genesis(&affair_id, &genesis);
        w.put_op(&affair_id, &c0_hash, &c0);
        w.put_op(&affair_id, &res1_hash, &res1);
        w.put_op(&affair_id, &res2_hash, &res2);
        let put_grant = |w: &mut FixtureWriter, grant: &Value| {
            let parsed = parse_effect_grant(grant).unwrap();
            let storage = w.kernel.require_storage_raw_mut().unwrap();
            storage.put(&parsed.key, &grant.to_string()).unwrap();
        };
        for grant in [
            &grant_roster,
            &grant_policy,
            &grant_create_revoked,
            &grant_budget_unanchored,
        ] {
            put_grant(&mut w, grant);
        }
        w.anchor_genesis(&affair_id, &genesis, T0);
        w.anchor_op(&affair_id, &c0_hash, &c0, T0 + 500);
        w.anchor_op(&affair_id, &res1_hash, &res1, t_res1);
        w.anchor_op(&affair_id, &res2_hash, &res2, t_res2_far_future);
        // 声明锚定（组织侧域）：roster 严格早于 res1；policy 晚于 res1；
        // create 撤销记录本身也早于 res1；budget:x 不锚定。
        w.anchor(&org_id, "effectgrant", "roster", &grant_roster, T0 + 5_000);
        w.anchor(&org_id, "effectgrant", "policy", &grant_policy, t_res1 + 1);
        w.anchor(
            &org_id,
            "effectgrant",
            "create",
            &grant_create_revoked,
            T0 + 6_000,
        );
    }

    let out = kernel.affair_org_effects(&org_id, &affair_id).unwrap();
    let effects = out["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 8, "4 份声明 × 2 条决议");
    assert_eq!(out["invalidResolutions"].as_array().unwrap().len(), 0);
    let find = |scope: &str, res_hash: &str| {
        effects
            .iter()
            .find(|e| e["scope"] == scope && e["resolutionOpHash"] == res_hash)
            .unwrap_or_else(|| panic!("missing effect for {scope}/{res_hash}"))
            .clone()
    };

    // 三线齐备 → 待应用事件（回执尚未记录）
    let apply = find("roster", &res1_hash);
    assert_eq!(apply["outcome"], "apply");
    let pending = &apply["pendingEffect"];
    assert_eq!(pending["orgId"], org_id);
    assert_eq!(pending["affairId"], affair_id);
    assert_eq!(pending["resolutionOpHash"], res1_hash);
    assert_eq!(pending["scope"], "roster");
    assert_eq!(
        pending["grantKey"],
        effect_grant_key(&org_id, &affair_id, "roster").unwrap()
    );
    assert_eq!(apply["receipt"]["state"], "unrecorded");

    // 决议未生效（公示期未满）
    assert_eq!(
        find("roster", &res2_hash)["outcome"],
        "resolutionNotEffective"
    );
    // 事后追认（声明锚定不早于决议锚定）
    assert_eq!(find("policy", &res1_hash)["outcome"], "notPrior");
    // 已撤销
    assert_eq!(find("create", &res1_hash)["outcome"], "revoked");
    // 声明无存证锚定（fail-closed）
    assert_eq!(find("budget:x", &res1_hash)["outcome"], "grantNotAnchored");

    // 未声明的组织 → 空事件集
    let other = kernel
        .affair_org_effects(&format!("org_{}", "cd".repeat(32)), &affair_id)
        .unwrap();
    assert_eq!(other["effects"].as_array().unwrap().len(), 0);
}

/// 规则链 replay 生产驱动（任务 1）：`affair_read_rules` 给出现行版本/逐版本
/// 生效时刻/未生效归宿；`affair_read_resolution` 按 rulesHash 命中版本复算
/// （任务 5），公示期取命中版本。
#[test]
fn rule_chain_replay_drives_facade_reads() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let genesis_rules_hash = rules_hash(&genesis["rules"]);
    let (c0, c0_hash) = make_content_op(&affair_id, &initiator, &affair_id, T0 + 100);

    // rc1：公示期 24h → 48h（锚定 T0，3 天窗口早过 → 生效）
    let (rc1, rc1_hash) = make_op(
        &affair_id,
        &initiator,
        "rule-change",
        json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "change": { "pubPeriod": { "delayMs": 2 * DAY } },
            "proposedAt": T0,
        }),
        &c0_hash,
        T0 + 200,
    );
    // rc2：同一变更但异议达阈值 → 否决
    let (rc2, rc2_hash) = make_op(
        &affair_id,
        &initiator,
        "rule-change",
        json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "change": { "pubPeriod": { "delayMs": 4 * DAY } },
            "proposedAt": T0,
        }),
        &c0_hash,
        T0 + 300,
    );
    let (obj, obj_hash) = make_op(
        &affair_id,
        &initiator,
        "objection",
        json!({ "target": rc2_hash }),
        &rc2_hash,
        T0 + 400,
    );
    // rc3：未锚定 → 待定
    let (rc3, rc3_hash) = make_op(
        &affair_id,
        &initiator,
        "rule-change",
        json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "change": { "pubPeriod": { "delayMs": 5 * DAY } },
            "proposedAt": T0,
        }),
        &c0_hash,
        T0 + 500,
    );

    // v1 规则（rc1 生效后）：公示期 48h
    let v1_rules = apply_rule_patch(&genesis["rules"], &json!({ "pubPeriod": { "delayMs": 2 * DAY } }));
    let v1_hash = rules_hash(&v1_rules);
    let condition = json!({ "type": "op-count", "opType": "content", "count": 1 });
    // r0：创世版本下的决议（公示期 24h）
    let (r0, r0_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition.clone(),
        vec![c0_hash.clone()],
        &genesis_rules_hash,
        T0 + 600,
    );
    // r1：v1 版本下的决议（自声明公示期 48h，与版本一致）
    let (r1, r1_hash) = {
        let (mut op, _) = make_resolution_op(
            &affair_id,
            &initiator,
            &c0_hash,
            condition.clone(),
            vec![c0_hash.clone()],
            &v1_hash,
            T0 + 700,
        );
        op["payload"]["pubPeriod"] = json!({ "delayMs": 2 * DAY });
        let sig_payload = op_sign_payload(&op).unwrap();
        op["sig"] = json!(sign(&initiator, &sig_payload));
        let hash = compute_op_hash(&op).unwrap();
        (op, hash)
    };
    // r_bad_hash：rulesHash 不在链上 → rules-hash-mismatch
    let (r_bad_hash, r_bad_hash_id) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition.clone(),
        vec![c0_hash.clone()],
        &"ff".repeat(32),
        T0 + 800,
    );
    // r_bad_cond：版本命中但条件不在该版本声明内 → condition-not-in-rules
    let (r_bad_cond, r_bad_cond_id) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        json!({ "type": "op-count", "opType": "content", "count": 5 }),
        vec![c0_hash.clone()],
        &genesis_rules_hash,
        T0 + 900,
    );
    // r_bad_counted：countedOps 篡改 → counted-ops-mismatch
    let (r_bad_counted, r_bad_counted_id) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition,
        vec!["00".repeat(32)],
        &genesis_rules_hash,
        T0 + 1_000,
    );

    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        w.put_genesis(&affair_id, &genesis);
        for (hash, op) in [
            (&c0_hash, &c0),
            (&rc1_hash, &rc1),
            (&rc2_hash, &rc2),
            (&obj_hash, &obj),
            (&rc3_hash, &rc3),
            (&r0_hash, &r0),
            (&r1_hash, &r1),
            (&r_bad_hash_id, &r_bad_hash),
            (&r_bad_cond_id, &r_bad_cond),
            (&r_bad_counted_id, &r_bad_counted),
        ] {
            w.put_op(&affair_id, hash, op);
        }
        w.anchor_genesis(&affair_id, &genesis, T0);
        w.anchor_op(&affair_id, &c0_hash, &c0, T0 + 100);
        w.anchor_op(&affair_id, &rc1_hash, &rc1, T0 + 200);
        w.anchor_op(&affair_id, &rc2_hash, &rc2, T0 + 300);
        w.anchor_op(&affair_id, &obj_hash, &obj, T0 + 400);
        // rc3 不锚定
        w.anchor_op(&affair_id, &r0_hash, &r0, T0 + 4 * DAY);
        w.anchor_op(&affair_id, &r1_hash, &r1, T0 + 4 * DAY + 1);
        w.anchor_op(&affair_id, &r_bad_hash_id, &r_bad_hash, T0 + 4 * DAY + 2);
        w.anchor_op(&affair_id, &r_bad_cond_id, &r_bad_cond, T0 + 4 * DAY + 3);
        w.anchor_op(
            &affair_id,
            &r_bad_counted_id,
            &r_bad_counted,
            T0 + 4 * DAY + 4,
        );
    }

    // 规则链读出口
    let rules_out = kernel.affair_read_rules(&affair_id).unwrap();
    assert_eq!(rules_out["current"]["seq"], 1);
    assert_eq!(rules_out["current"]["rulesHash"], json!(v1_hash));
    assert_eq!(
        rules_out["current"]["rules"]["pubPeriod"]["delayMs"],
        json!(2 * DAY)
    );
    let versions = rules_out["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["basisOpHash"], json!(affair_id));
    assert_eq!(versions[1]["basisOpHash"], json!(rc1_hash));
    assert_eq!(versions[1]["effectiveMs"], json!(T0 + 200 + 3 * DAY));
    let changes = rules_out["changes"].as_array().unwrap();
    let fate_of = |hash: &str| {
        changes
            .iter()
            .find(|c| c["opHash"] == hash)
            .unwrap()
            .clone()
    };
    assert_eq!(fate_of(&rc2_hash)["fate"], "rejected");
    assert_eq!(fate_of(&rc2_hash)["reason"], "decision-rejected");
    assert_eq!(fate_of(&rc3_hash)["fate"], "pending");
    assert_eq!(fate_of(&rc3_hash)["reason"], "unanchored");

    // 决议复算读出口
    let out = kernel.affair_read_resolution(&affair_id).unwrap();
    let resolutions = out["resolutions"].as_array().unwrap();
    assert_eq!(resolutions.len(), 5);
    let find = |hash: &str| {
        resolutions
            .iter()
            .find(|r| r["opHash"] == hash)
            .unwrap()
            .clone()
    };
    let r0_out = find(&r0_hash);
    assert_eq!(r0_out["state"], "effective");
    assert_eq!(r0_out["replay"], "ok");
    assert_eq!(r0_out["valid"], true);
    assert_eq!(r0_out["rulesSeq"], 0);
    assert_eq!(r0_out["pubPeriodMs"], json!(DAY));
    let r1_out = find(&r1_hash);
    assert_eq!(r1_out["state"], "effective");
    assert_eq!(r1_out["rulesSeq"], 1);
    // 公示期取命中规则版本（48h），非创世 24h
    assert_eq!(r1_out["pubPeriodMs"], json!(2 * DAY));
    assert_eq!(find(&r_bad_hash_id)["state"], "invalid");
    assert_eq!(find(&r_bad_hash_id)["replay"], "rules-hash-mismatch");
    assert_eq!(find(&r_bad_cond_id)["replay"], "condition-not-in-rules");
    assert_eq!(find(&r_bad_counted_id)["replay"], "counted-ops-mismatch");
}

/// §9 投票前快照名册线上化（任务 2）：snapshot payload 生产助手 → 快照操作
/// 载入 → threshold 关闭条件基数注入（snapshot 形态 + ladder:voters 形态）；
/// 快照承诺篡改 → 基数缺席 fail-closed。
#[test]
fn snapshot_roster_feeds_threshold_replay() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x31);
    // 创世关闭条件为空：threshold 条件经 rule-change 引入（snapshot 基数引用
    // 的 opHash 在创世后才存在）
    let mut rules = baseline_rules();
    rules["closeConditions"] = json!([]);
    let (genesis, affair_id) = make_genesis_with(&initiator, "快照基数测试", rules, json!([initiator.identity.clone()]));
    let (c0, c0_hash) = make_content_op(&affair_id, &initiator, &affair_id, T0 + 100);

    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        w.put_genesis(&affair_id, &genesis);
        w.put_op(&affair_id, &c0_hash, &c0);
        w.anchor_genesis(&affair_id, &genesis, T0);
        w.anchor_op(&affair_id, &c0_hash, &c0, T0 + 100);
    }

    // 快照 payload 生产助手：asOf = c0，名册 = [initiator]
    let snap = kernel
        .affair_snapshot_payload(&affair_id, Some(&c0_hash))
        .unwrap();
    assert_eq!(snap["payload"]["basis"], "ladder");
    assert_eq!(snap["payload"]["asOf"], json!(c0_hash));
    assert_eq!(snap["roster"].as_array().unwrap().len(), 1);

    // 快照操作载入日志
    let (s1, s1_hash) = make_op(
        &affair_id,
        &initiator,
        "snapshot",
        snap["payload"].clone(),
        &c0_hash,
        T0 + 200,
    );
    // 篡改快照（rosterHash 不符 → 复算失败，fail-closed）
    let (s2, s2_hash) = make_op(
        &affair_id,
        &initiator,
        "snapshot",
        json!({ "basis": "ladder", "asOf": c0_hash, "rosterHash": "00".repeat(32) }),
        &c0_hash,
        T0 + 300,
    );
    // rc1：引入两条 threshold 关闭条件（snapshot:s1 与 ladder:voters）
    let (rc1, rc1_hash) = make_op(
        &affair_id,
        &initiator,
        "rule-change",
        json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "change": { "closeConditions": [
                { "type": "threshold", "base": format!("snapshot:{s1_hash}"), "num": 1, "den": 1 },
                { "type": "threshold", "base": "ladder:voters", "num": 1, "den": 1 },
            ] },
            "proposedAt": T0,
        }),
        &c0_hash,
        T0 + 400,
    );
    // rc2：引入指向篡改快照的 threshold 条件
    let (rc2, rc2_hash) = make_op(
        &affair_id,
        &initiator,
        "rule-change",
        json!({
            "mechanism": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
            "change": { "closeConditions": [
                { "type": "threshold", "base": format!("snapshot:{s2_hash}"), "num": 1, "den": 1 },
            ] },
            "proposedAt": T0,
        }),
        &rc1_hash,
        T0 + 500,
    );

    let v1_rules = apply_rule_patch(
        &genesis["rules"],
        &json!({ "closeConditions": [
            { "type": "threshold", "base": format!("snapshot:{s1_hash}"), "num": 1, "den": 1 },
            { "type": "threshold", "base": "ladder:voters", "num": 1, "den": 1 },
        ] }),
    );
    let v1_hash = rules_hash(&v1_rules);
    let v2_rules = apply_rule_patch(
        &v1_rules,
        &json!({ "closeConditions": [
            { "type": "threshold", "base": format!("snapshot:{s2_hash}"), "num": 1, "den": 1 },
        ] }),
    );
    let v2_hash = rules_hash(&v2_rules);

    // r1：snapshot 形态基数；r2：ladder:voters 形态基数；r3：篡改快照基数
    let (r1, r1_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        json!({ "type": "threshold", "base": format!("snapshot:{s1_hash}"), "num": 1, "den": 1 }),
        vec![c0_hash.clone()],
        &v1_hash,
        T0 + 600,
    );
    let (r2, r2_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        json!({ "type": "threshold", "base": "ladder:voters", "num": 1, "den": 1 }),
        vec![c0_hash.clone()],
        &v1_hash,
        T0 + 700,
    );
    let (r3, r3_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        json!({ "type": "threshold", "base": format!("snapshot:{s2_hash}"), "num": 1, "den": 1 }),
        vec![c0_hash.clone()],
        &v2_hash,
        T0 + 800,
    );

    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        for (hash, op, ts) in [
            (&s1_hash, &s1, T0 + 200),
            (&s2_hash, &s2, T0 + 300),
            (&rc1_hash, &rc1, T0 + 400),
            (&rc2_hash, &rc2, T0 + 500),
            (&r1_hash, &r1, T0 + 4 * DAY),
            (&r2_hash, &r2, T0 + 4 * DAY + 1),
            (&r3_hash, &r3, T0 + 4 * DAY + 2),
        ] {
            w.put_op(&affair_id, hash, op);
            w.anchor_op(&affair_id, hash, op, ts);
        }
    }

    // 链上有两条生效修改（v1/v2）
    let rules_out = kernel.affair_read_rules(&affair_id).unwrap();
    assert_eq!(rules_out["versions"].as_array().unwrap().len(), 3);

    let out = kernel.affair_read_resolution(&affair_id).unwrap();
    let resolutions = out["resolutions"].as_array().unwrap();
    let find = |hash: &str| {
        resolutions
            .iter()
            .find(|r| r["opHash"] == hash)
            .unwrap()
            .clone()
    };
    // snapshot 形态：名册复算一致，基数 1，1/1 满足 → 有效生效
    assert_eq!(find(&r1_hash)["replay"], "ok");
    assert_eq!(find(&r1_hash)["state"], "effective");
    // ladder:voters 形态：决议前因果闭包名册 = 初始投票者 1 人 → 有效
    assert_eq!(find(&r2_hash)["replay"], "ok");
    assert_eq!(find(&r2_hash)["state"], "effective");
    // 篡改快照：rosterHash 复算不符 → 基数缺席 → 条件不满足 → 无效
    assert_eq!(find(&r3_hash)["replay"], "condition-not-satisfied");
    assert_eq!(find(&r3_hash)["state"], "invalid");
}

/// vote 形态规则修改的快照名册驱动（任务 1+2 交汇）：无快照 → 恒待定
/// （fail-closed）；载入投票前快照后按名册计票生效，生效时刻 = 越过阈值票
/// 的锚定时刻。
#[test]
fn vote_rule_change_uses_pre_vote_snapshot() {
    let (_dir, mut kernel) = unlocked_kernel();
    let a = fixed_key(0x41);
    let b = fixed_key(0x42);
    let c = fixed_key(0x43);
    let rules = json!({
        "engine": "b1",
        "closeConditions": [],
        "pubPeriod": { "delayMs": DAY },
        "ruleChange": { "kind": "vote", "voterSet": "ladder:voters",
            "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
            "snapshot": "required" },
        "exec": null,
    });
    let voters = json!([a.identity, b.identity, c.identity]);
    // 两个事务：一个载入快照，一个不载（no-roster 对照）
    let (genesis_with, affair_with) = make_genesis_with(&a, "vote 有快照", rules.clone(), voters.clone());
    let (genesis_without, affair_without) = make_genesis_with(&a, "vote 无快照", rules, voters);

    for (affair_id, genesis, with_snapshot) in [
        (&affair_with, &genesis_with, true),
        (&affair_without, &genesis_without, false),
    ] {
        let (c0, c0_hash) = make_content_op(affair_id, &a, affair_id, T0 + 100);
        {
            let mut w = FixtureWriter {
                kernel: &mut kernel,
            };
            w.put_genesis(affair_id, genesis);
            w.put_op(affair_id, &c0_hash, &c0);
            w.anchor_genesis(affair_id, genesis, T0);
            w.anchor_op(affair_id, &c0_hash, &c0, T0 + 100);
        }
        // 经生产助手产出快照 payload 再载入
        let snap = with_snapshot.then(|| {
            kernel
                .affair_snapshot_payload(affair_id, Some(&c0_hash))
                .unwrap()
        });
        if let Some(snap) = &snap {
            assert_eq!(snap["roster"].as_array().unwrap().len(), 3);
        }
        let mut w = FixtureWriter {
            kernel: &mut kernel,
        };
        if let Some(snap) = snap {
            let (s1, s1_hash) = make_op(
                affair_id,
                &a,
                "snapshot",
                snap["payload"].clone(),
                &c0_hash,
                T0 + 200,
            );
            w.put_op(affair_id, &s1_hash, &s1);
            w.anchor_op(affair_id, &s1_hash, &s1, T0 + 200);
        }
        let (rc, rc_hash) = make_op(
            affair_id,
            &a,
            "rule-change",
            json!({
                "mechanism": { "kind": "vote", "voterSet": "ladder:voters",
                    "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                    "snapshot": "required" },
                "change": { "pubPeriod": { "delayMs": 2 * DAY } },
                "proposedAt": T0,
            }),
            &c0_hash,
            T0 + 300,
        );
        let (v1, v1_hash) = make_op(
            affair_id,
            &a,
            "vote",
            json!({ "proposal": rc_hash, "choice": "yes" }),
            &rc_hash,
            T0 + 400,
        );
        let (v2, v2_hash) = make_op(
            affair_id,
            &b,
            "vote",
            json!({ "proposal": rc_hash, "choice": "yes" }),
            &v1_hash,
            T0 + 500,
        );
        w.put_op(affair_id, &rc_hash, &rc);
        w.put_op(affair_id, &v1_hash, &v1);
        w.put_op(affair_id, &v2_hash, &v2);
        w.anchor_op(affair_id, &rc_hash, &rc, T0 + 300);
        w.anchor_op(affair_id, &v1_hash, &v1, T0 + 400);
        w.anchor_op(affair_id, &v2_hash, &v2, T0 + 500);
    }

    // 有快照：2/3 票 ≥ 1/2 且参与 2/3 ≥ 1/2 → 生效，生效时刻 = v2 锚定
    let out = kernel.affair_read_rules(&affair_with).unwrap();
    assert_eq!(out["versions"].as_array().unwrap().len(), 2);
    assert_eq!(out["versions"][1]["effectiveMs"], json!(T0 + 500));
    assert_eq!(
        out["current"]["rules"]["pubPeriod"]["delayMs"],
        json!(2 * DAY)
    );
    // 无快照：名册缺席 → 恒待定（fail-closed）
    let out = kernel.affair_read_rules(&affair_without).unwrap();
    assert_eq!(out["versions"].as_array().unwrap().len(), 1);
    assert_eq!(out["changes"][0]["fate"], "pending");
    assert_eq!(out["changes"][0]["reason"], "no-roster");
}

/// 执行型事务（任务 4）：`affair_read_exec` 状态机读路径 + `affair_submit_op`
/// 的 exec-report 校验入口（非执行方拒 / 未声明 exec 拒 / 未知决议暂存）。
#[test]
fn exec_states_and_submit_gate() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x51);
    let executor = fixed_key(0x52);
    let mut rules = baseline_rules();
    rules["exec"] = json!({
        "executor": { "kind": "person", "identity": executor.identity, "publicKey": executor.public_key },
        "verify": { "kind": "delayed-veto", "delayMs": 3 * DAY, "vetoThreshold": { "count": 1 } },
    });
    let (genesis, affair_id) =
        make_genesis_with(&initiator, "执行型事务", rules, json!([initiator.identity.clone()]));
    // 生产关注路径（真实入站 + 创世锚定）
    let followed = kernel.affair_follow(&genesis).unwrap();
    assert_eq!(followed, affair_id);

    let rules_hash = rules_hash(&genesis["rules"]);
    let (c0, c0_hash) = make_content_op(&affair_id, &initiator, &affair_id, T0 + 100);
    let (res, res_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        json!({ "type": "op-count", "opType": "content", "count": 1 }),
        vec![c0_hash.clone()],
        &rules_hash,
        T0 + 200,
    );
    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        w.put_op(&affair_id, &c0_hash, &c0);
        w.put_op(&affair_id, &res_hash, &res);
        w.anchor_op(&affair_id, &c0_hash, &c0, T0 + 100);
        w.anchor_op(&affair_id, &res_hash, &res, T0 + 200);
    }

    // 决议已生效（锚定 T0+200 + 24h 公示期 << now），无回报 → 待执行
    let out = kernel.affair_read_exec(&affair_id).unwrap();
    assert!(out["exec"].is_object());
    let states = out["states"].as_array().unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["state"], "awaiting-execution");

    let now = system_now_ms();
    // 非执行方提交 → 拒（not-executor）
    let (rogue, _) = make_op(
        &affair_id,
        &initiator,
        "exec-report",
        json!({ "resolution": res_hash, "status": "done", "evidence": ["ev:1"] }),
        &res_hash,
        now,
    );
    let err = kernel.affair_submit_op(&rogue).unwrap_err();
    assert!(format!("{err:?}").contains("not-executor"), "{err:?}");

    // 引用未知决议 → 校验入口放行，入站链按乱序规则暂存（pending）
    let (orphan, orphan_hash) = make_op(
        &affair_id,
        &executor,
        "exec-report",
        json!({ "resolution": "ab".repeat(32), "status": "accepted" }),
        &res_hash,
        now,
    );
    let result = kernel.affair_submit_op(&orphan).unwrap();
    assert_eq!(result["status"], "pending");
    assert_eq!(result["opHash"], json!(orphan_hash));

    // 执行方提交 done 回报 → 接受；状态机进核查中
    let (report, report_hash) = make_op(
        &affair_id,
        &executor,
        "exec-report",
        json!({ "resolution": res_hash, "status": "done", "evidence": ["ev:done"] }),
        &res_hash,
        now,
    );
    let result = kernel.affair_submit_op(&report).unwrap();
    assert_eq!(result["status"], "accepted");
    let out = kernel.affair_read_exec(&affair_id).unwrap();
    let states = out["states"].as_array().unwrap();
    assert_eq!(states[0]["state"], "verifying");
    assert_eq!(states[0]["reportOpHash"], json!(report_hash));

    // 未声明 exec 的事务 → 校验入口拒（exec-not-declared）
    let (plain_genesis, plain_affair) = make_genesis(&initiator);
    kernel.affair_follow(&plain_genesis).unwrap();
    let (report, _) = make_op(
        &plain_affair,
        &executor,
        "exec-report",
        json!({ "resolution": plain_affair, "status": "accepted" }),
        &plain_affair,
        now,
    );
    let err = kernel.affair_submit_op(&report).unwrap_err();
    assert!(format!("{err:?}").contains("exec-not-declared"), "{err:?}");
}

/// 回执消费编排（任务 3）：Apply 事件写回执 + 存证锚定；幂等；新决议生效
/// 取代旧回执（superseded）；`affair_org_effects` 如实标注回执状态。
#[test]
fn effect_receipt_orchestration() {
    let (_dir, mut kernel) = unlocked_kernel();
    let initiator = fixed_key(0x61);
    let org_id = format!("org_{}", "ab".repeat(32));
    let (genesis, affair_id) = make_genesis(&initiator);
    let rules_hash = rules_hash(&genesis["rules"]);
    let (c0, c0_hash) = make_content_op(&affair_id, &initiator, &affair_id, T0 + 500);
    let condition = json!({ "type": "op-count", "opType": "content", "count": 1 });
    let (res1, res1_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition.clone(),
        vec![c0_hash.clone()],
        &rules_hash,
        T0 + 1_000,
    );
    let (res2, res2_hash) = make_resolution_op(
        &affair_id,
        &initiator,
        &c0_hash,
        condition,
        vec![c0_hash.clone()],
        &rules_hash,
        T0 + 2_000,
    );
    let grant = grant_value(&org_id, &affair_id, "roster", false);
    {
        let mut w = FixtureWriter { kernel: &mut kernel };
        w.put_genesis(&affair_id, &genesis);
        w.put_op(&affair_id, &c0_hash, &c0);
        w.put_op(&affair_id, &res1_hash, &res1);
        w.put_op(&affair_id, &res2_hash, &res2);
        let parsed = parse_effect_grant(&grant).unwrap();
        w.kernel
            .require_storage_raw_mut()
            .unwrap()
            .put(&parsed.key, &grant.to_string())
            .unwrap();
        w.anchor_genesis(&affair_id, &genesis, T0);
        w.anchor_op(&affair_id, &c0_hash, &c0, T0 + 500);
        w.anchor_op(&affair_id, &res1_hash, &res1, T0 + 10_000);
        w.anchor_op(&affair_id, &res2_hash, &res2, T0 + 20_000);
        w.anchor(&org_id, "effectgrant", "roster", &grant, T0 + 5_000);
    }

    // 首次消费：同 scope 两条生效决议并存 → 回执只跟踪最新（res2），
    // 较旧的 res1 记 superseded-by-newer 不写回执（防覆写震荡）
    let out = kernel.affair_apply_org_effects(&org_id, &affair_id).unwrap();
    let actions = out["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    let action_of = |res_hash: &str| {
        actions
            .iter()
            .find(|a| a["resolutionOpHash"] == res_hash)
            .unwrap()
            .clone()
    };
    assert_eq!(action_of(&res1_hash)["action"], "skipped");
    assert_eq!(action_of(&res1_hash)["outcome"], "superseded-by-newer");
    assert_eq!(action_of(&res2_hash)["action"], "recorded");
    let receipt_key = effect_receipt_key(&org_id, &affair_id, "roster").unwrap();
    assert_eq!(action_of(&res2_hash)["receiptKey"], json!(receipt_key));

    // 回执落库 + 存证锚定（按 payloadHash 全链扫描可查）
    let stored = kernel
        .require_storage()
        .unwrap()
        .get(&receipt_key)
        .unwrap()
        .expect("receipt stored");
    let receipt = parse_effect_receipt(&serde_json::from_str::<Value>(&stored).unwrap()).unwrap();
    assert_eq!(receipt.resolution_op_hash, res2_hash);
    assert!(
        grant_anchor_ms(kernel.require_storage().unwrap(), &receipt_to_value(&receipt))
            .unwrap()
            .is_some()
    );

    // 幂等：再次消费 → 最新决议 already-recorded，旧决议仍跳过
    let out = kernel.affair_apply_org_effects(&org_id, &affair_id).unwrap();
    let actions = out["actions"].as_array().unwrap();
    let action_of = |res_hash: &str| {
        actions
            .iter()
            .find(|a| a["resolutionOpHash"] == res_hash)
            .unwrap()
            .clone()
    };
    assert_eq!(action_of(&res2_hash)["action"], "already-recorded");
    assert_eq!(action_of(&res1_hash)["outcome"], "superseded-by-newer");

    // org_effects 读口标注回执状态：res2 recorded；res1 因回执已被取代而
    // unrecorded（同一 scope 的现行回执指向 res2）
    let out = kernel.affair_org_effects(&org_id, &affair_id).unwrap();
    let effects = out["effects"].as_array().unwrap();
    let find = |res_hash: &str| {
        effects
            .iter()
            .find(|e| e["resolutionOpHash"] == res_hash)
            .unwrap()
            .clone()
    };
    assert_eq!(find(&res1_hash)["receipt"]["state"], "unrecorded");
    assert_eq!(find(&res2_hash)["receipt"]["state"], "recorded");
}

/// 由解析产物重建回执原文（存证查找按内容哈希）。
fn receipt_to_value(receipt: &crate::affair::EffectReceipt) -> Value {
    json!({
        "receiptV": 1,
        "orgId": receipt.org_id,
        "affairId": receipt.affair_id,
        "scope": receipt.scope,
        "resolutionOpHash": receipt.resolution_op_hash,
        "grantKey": receipt.grant_key,
        "state": receipt.state,
        "recordedAtMs": receipt.recorded_at_ms,
    })
}
