//! community-affairs C1 golden vectors 生成器（规格：wiki/protocol/community/affair.md §12、
//! affair-metadata.md §7）。
//!
//! 运行：`cargo run --example gen_community_affair_vectors`，直接回填
//! `../spec/vectors/community.json` 中 C1 所属的 case 组（staticCheck /
//! resolutionReplay / metaBasisVerify / metaArbitrate），其余组原样保留。
//!
//! 复核纪律（先向量后实现的可执行重述）：生成前先用本实现复算 C0 期 JS 参考
//! 实现自产的 affairGenesis / opChain / ruleMechanisms / snapshot 四组并逐字节
//! 断言相等，不等则以非零退出——向量口径由两侧独立实现互证。
//!
//! 确定性：全部密钥 = 固定 seed（与 gen-community-vectors.mjs 同口径：
//! initiator 0x11 / personA 0x21 / personB 0x22），时间以 NOW = 1720000000000
//! 为基准固定偏移。

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::affair::*;

const NOW: i64 = 1_720_000_000_000;

struct FixedKey {
    signing_key: SigningKey,
    public_key: String,
    identity: String,
}

fn key_from_seed(seed_byte: u8) -> FixedKey {
    let signing_key = SigningKey::from_bytes(&[seed_byte; 32]);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    let public_key =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public_key_bytes);
    // 身份 id = sha256hex(原始 32 字节公钥)（community README 总约）
    let identity = hex::encode(Sha256::digest(public_key_bytes));
    FixedKey {
        signing_key,
        public_key,
        identity,
    }
}

fn actor_json(key: &FixedKey) -> Value {
    json!({ "kind": "person", "identity": key.identity, "publicKey": key.public_key })
}

fn sign_payload(key: &FixedKey, payload: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        key.signing_key.sign(payload.as_bytes()).to_bytes(),
    )
}

fn make_op(
    affair_id: &str,
    prev_op_hash: &str,
    op_type: &str,
    payload: Value,
    key: &FixedKey,
    declared_at: i64,
) -> (Value, String) {
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": op_type, "payload": payload, "actor": actor_json(key),
        "declaredAt": declared_at,
    });
    let sign_payload_str = op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(sign_payload(key, &sign_payload_str));
    let op_hash = compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

fn make_genesis(
    rules: Value,
    title: &str,
    summary: &str,
    tags: Value,
    initiator: &FixedKey,
) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": title, "summary": summary, "tags": tags,
        "initiator": actor_json(initiator), "rules": rules,
        "initialVoters": [initiator.identity], "refs": [], "createdAt": NOW,
    });
    let payload = genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign_payload(initiator, &payload));
    let affair_id = compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

/// 复算 C0 组并逐字节断言（返回错误名列表，空 = 通过）。
fn cross_check_c0_groups(v: &Value) -> Vec<String> {
    let mut fails = Vec::new();
    // affairGenesis：固定输入 → 载荷/affairId/记录逐字节
    {
        let section = &v["affairGenesis"]["expect"];
        let record = &section["record"];
        if genesis_sign_payload(record).ok().as_deref() != section["payload"].as_str() {
            fails.push("affairGenesis.payload".to_string());
        }
        if compute_affair_id(record).ok().as_deref() != section["affairId"].as_str() {
            fails.push("affairGenesis.affairId".to_string());
        }
        if verify_genesis(record).is_err() {
            fails.push("affairGenesis.verify".to_string());
        }
    }
    // opChain：opHash/签名载荷逐字节 + 排序键
    {
        let section = &v["opChain"]["expect"];
        let mut hashes = Vec::new();
        for op_case in section["ops"].as_array().expect("opChain.ops") {
            let entry = &op_case["entry"];
            if op_sign_payload(entry).ok().as_deref() != op_case["payload"].as_str() {
                fails.push("opChain.payload".to_string());
            }
            let hash = compute_op_hash(entry).expect("op hash");
            if Some(hash.as_str()) != op_case["opHash"].as_str() {
                fails.push("opChain.opHash".to_string());
            }
            hashes.push(hash);
        }
        let expected_sort: Vec<String> = section["sortOrder"]
            .as_array()
            .expect("sortOrder")
            .iter()
            .map(|h| h.as_str().unwrap().to_string())
            .collect();
        if sort_op_hashes(&hashes) != expected_sort {
            fails.push("opChain.sortOrder".to_string());
        }
    }
    // ruleMechanisms：三形态 canonical 逐字节 + 解析通过。
    // 注意：库存 canonical 是 normalizeObject 输出（嵌套对象已字符串化），不能
    // 直接作为机制 JSON 解析——按固定输入重建机制文档再比对。
    {
        let section = &v["ruleMechanisms"]["expect"];
        let admins: Vec<String> = ["admin1", "admin2", "admin3"]
            .iter()
            .map(|k| {
                v["meta"]["actors"][k]["identity"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let mechanisms = [
            (
                "vote",
                "vote",
                json!({ "kind": "vote", "voterSet": "ladder:voters",
                    "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                    "snapshot": "required" }),
            ),
            (
                "multisig",
                "multisig",
                json!({ "kind": "multisig", "m": 2, "n": 3, "signers": admins }),
            ),
            (
                "delayedVeto",
                "delayed-veto",
                json!({ "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } }),
            ),
        ];
        for (key, kind, doc) in mechanisms {
            let canonical = section[key].as_str().expect("mechanism canonical");
            if spark_core::evidence::normalize_object(&doc) != canonical {
                fails.push(format!("ruleMechanisms.{key}.canonical"));
            }
            match parse_mechanism(&doc) {
                Ok(m) if m.kind_str() == kind => {}
                _ => fails.push(format!("ruleMechanisms.{key}.parse")),
            }
        }
    }
    // snapshot：rosterHash / memberSetHash 固定值
    {
        let section = &v["snapshot"]["expect"];
        let identities: Vec<String> = section["roster"]
            .as_array()
            .expect("roster")
            .iter()
            .map(|e| e["identity"].as_str().unwrap().to_string())
            .collect();
        if roster_hash(&identities).ok().as_deref() != section["rosterHash"].as_str() {
            fails.push("snapshot.rosterHash".to_string());
        }
        let members = section["memberSet"].as_array().expect("memberSet").clone();
        if member_set_hash(&members).ok().as_deref() != section["memberSetHash"].as_str() {
            fails.push("snapshot.memberSetHash".to_string());
        }
    }
    fails
}

fn baseline_rules() -> Value {
    json!({
        "engine": "b1",
        "closeConditions": [{ "type": "op-count", "opType": "content", "count": 100 }],
        "pubPeriod": { "delayMs": 86400000 },
        "participation": { "contribute": { "ladder": "contributor" }, "vote": { "ladder": "voter" }, "combine": "all" },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
        "exec": null
    })
}

fn gen_static_check(admin1: &FixedKey) -> Value {
    let mut cases: Vec<Value> = Vec::new();
    let mut push = |name: &str, rules: Value| {
        let expect = match static_check_rules(&rules) {
            Ok(_) => json!({ "ok": true }),
            Err(reject) => json!({ "ok": false, "reason": reject.reason() }),
        };
        cases.push(json!({ "name": name, "rules": rules, "expect": expect }));
    };
    push("baseline-ok", baseline_rules());
    let mut wall_clock_ok = baseline_rules();
    wall_clock_ok["closeConditions"] =
        json!([{ "type": "wall-clock", "notBefore": NOW + 86_400_000 }]);
    push("wall-clock-with-default-pub-period-ok", wall_clock_ok);
    let mut arrival = baseline_rules();
    arrival["closeConditions"] = json!([{ "type": "first-received", "count": 5 }]);
    push("arrival-order-dependency", arrival);
    let mut no_pub = baseline_rules();
    no_pub["closeConditions"] = json!([{ "type": "wall-clock", "notBefore": NOW + 86_400_000 }]);
    no_pub["pubPeriod"] = json!({ "delayMs": 0 });
    push("wall-clock-without-pub-period", no_pub);
    let mut m_gt_n = baseline_rules();
    m_gt_n["ruleChange"] = json!({ "kind": "multisig", "m": 3, "n": 2,
        "signers": [admin1.identity, "ab".repeat(32), "cd".repeat(32)] });
    push("multisig-m-gt-n", m_gt_n);
    let mut single = baseline_rules();
    single["ruleChange"] =
        json!({ "kind": "multisig", "m": 1, "n": 1, "signers": [admin1.identity] });
    push("single-point-effect", single);
    let mut dictator = baseline_rules();
    dictator["ruleChange"] = json!({ "kind": "dictator" });
    push("unknown-mechanism", dictator);
    let mut count0 = baseline_rules();
    count0["closeConditions"] = json!([{ "type": "op-count", "opType": "content", "count": 0 }]);
    push("op-count-zero", count0);
    let mut bad_ratio = baseline_rules();
    bad_ratio["closeConditions"] =
        json!([{ "type": "threshold", "base": "ladder:voters", "num": 3, "den": 2 }]);
    push("threshold-num-gt-den", bad_ratio);
    let mut bad_base = baseline_rules();
    bad_base["closeConditions"] =
        json!([{ "type": "threshold", "base": "local-clock", "num": 1, "den": 2 }]);
    push("threshold-bad-base", bad_base);
    let mut bad_op = baseline_rules();
    bad_op["closeConditions"] =
        json!([{ "type": "op-count", "opType": "plugin-custom", "count": 1 }]);
    push("unknown-op-type-in-condition", bad_op);
    // 评审阻塞 2：pubPeriod present 但形状/类型非法 → malformed（不回退缺省）
    let mut pp_not_obj = baseline_rules();
    pp_not_obj["pubPeriod"] = json!("86400000");
    push("pub-period-not-object", pp_not_obj);
    let mut pp_delay_type = baseline_rules();
    pp_delay_type["pubPeriod"] = json!({ "delayMs": "86400000" });
    push("pub-period-delay-ms-wrong-type", pp_delay_type);
    let mut pp_veto_type = baseline_rules();
    pp_veto_type["pubPeriod"] = json!({ "delayMs": 86400000, "vetoThreshold": "1" });
    push("pub-period-veto-threshold-wrong-type", pp_veto_type);
    let mut pp_veto_count_type = baseline_rules();
    pp_veto_count_type["pubPeriod"] =
        json!({ "delayMs": 86400000, "vetoThreshold": { "count": "1" } });
    push("pub-period-veto-count-wrong-type", pp_veto_count_type);
    // 字段整体缺席 → 缺省值生效（正例）
    let mut pp_absent = baseline_rules();
    pp_absent.as_object_mut().unwrap().shift_remove("pubPeriod");
    push("pub-period-absent-default-ok", pp_absent);
    // 评审建议 1：ladderParams 越界/形状非法在校验期即拒
    let mut lp_ok = baseline_rules();
    lp_ok["participation"]["ladderParams"] =
        json!({ "contributorAccepts": 2, "voterDays": 10, "voterAccepts": 2 });
    push("ladder-params-ok", lp_ok);
    let mut lp_zero = baseline_rules();
    lp_zero["participation"]["ladderParams"] = json!({ "voterDays": 0 });
    push("ladder-params-voter-days-zero", lp_zero);
    let mut lp_not_obj = baseline_rules();
    lp_not_obj["participation"]["ladderParams"] = json!("bad");
    push("ladder-params-not-object", lp_not_obj);
    json!({
        "desc": "§5.6 拒绝用例集 + 正例基线（C1 core/examples 生成器自产）：到达顺序依赖、无公示期 wall-clock、m>n、单点效力、畸形 pubPeriod 形状（fail-closed，缺省只应用于字段整体缺席）、越界 ladderParams 等 fail-closed",
        "expect": { "cases": cases }
    })
}

fn gen_resolution_replay(initiator: &FixedKey, person_a: &FixedKey, person_b: &FixedKey) -> Value {
    let condition = json!({ "type": "op-count", "opType": "content", "count": 2 });
    let rules = json!({
        "engine": "b1",
        "closeConditions": [condition],
        "pubPeriod": { "delayMs": 86400000 },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
        "exec": null
    });
    let (_genesis, affair_id) = make_genesis(
        rules.clone(),
        "签名征集",
        "固定操作集合 → resolution 复算",
        json!([]),
        initiator,
    );
    let (op_a, hash_a) = make_op(
        &affair_id,
        &affair_id,
        "content",
        json!({ "kind": "sign", "text": "附议甲" }),
        person_a,
        NOW + 1000,
    );
    let (op_b, hash_b) = make_op(
        &affair_id,
        &hash_a,
        "content",
        json!({ "kind": "sign", "text": "附议乙" }),
        person_b,
        NOW + 2000,
    );
    let counted_ops = sort_op_hashes(&[hash_a.clone(), hash_b.clone()]);
    let resolution_payload = json!({
        "result": "passed",
        "condition": condition,
        "countedOps": counted_ops,
        "tally": { "signs": 2 },
        "quorumSnapshot": null,
        "rulesHash": rules_hash(&rules),
        "pubPeriod": { "delayMs": 86400000 }
    });
    let (resolution_op, resolution_hash) = make_op(
        &affair_id,
        &hash_b,
        "resolution",
        resolution_payload,
        initiator,
        NOW + 3000,
    );
    // 自证：本实现复算必须通过
    let valid_ops = vec![
        CountedOp {
            op_hash: hash_a.clone(),
            op_type: OpType::Content,
            actor_identity: person_a.identity.clone(),
            payload: json!({ "kind": "sign", "text": "附议甲" }),
        },
        CountedOp {
            op_hash: hash_b.clone(),
            op_type: OpType::Content,
            actor_identity: person_b.identity.clone(),
            payload: json!({ "kind": "sign", "text": "附议乙" }),
        },
    ];
    let parsed = parse_resolution_payload(&resolution_op["payload"]).expect("resolution payload");
    assert_eq!(
        replay_resolution(&rules, &parsed, &valid_ops, &CloseCtx::empty()),
        ReplayOutcome::Ok,
        "self-check: resolution replay"
    );
    // wall-clock 变体：同一结构，条件换 wall-clock，countedOps 为空
    let wc_condition = json!({ "type": "wall-clock", "notBefore": NOW + 86_400_000 });
    let wc_rules = json!({
        "engine": "b1",
        "closeConditions": [wc_condition],
        "pubPeriod": { "delayMs": 86400000 },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
        "exec": null
    });
    let wc_anchored = NOW + 86_400_000 + WALL_CLOCK_TOLERANCE_MS; // 恰越过容忍带 → Reached
    let wc_payload = json!({
        "result": "passed",
        "condition": wc_condition,
        "countedOps": [],
        "tally": null,
        "quorumSnapshot": null,
        "rulesHash": rules_hash(&wc_rules),
        "pubPeriod": { "delayMs": 86400000 }
    });
    let wc_parsed = parse_resolution_payload(&wc_payload).expect("wc payload");
    let wc_ctx = CloseCtx {
        anchored_ms: Some(wc_anchored),
        ..CloseCtx::empty()
    };
    assert_eq!(
        replay_resolution(&wc_rules, &wc_parsed, &[], &wc_ctx),
        ReplayOutcome::Ok,
        "self-check: wall-clock replay"
    );
    // threshold 变体（评审建议 5）：按人去重计入——同一身份多条操作只计一次，
    // 每人取 opHash 字典序最小者；负例（把同一人的两条都计入）由消费测试构造
    let th_condition = json!({ "type": "threshold", "base": "ladder:voters", "num": 1, "den": 2 });
    let th_rules = json!({
        "engine": "b1",
        "closeConditions": [th_condition],
        "pubPeriod": { "delayMs": 86400000 },
        "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
        "exec": null
    });
    let (_th_genesis, th_affair_id) = make_genesis(
        th_rules.clone(),
        "按人头关闭",
        "threshold 关闭条件复算",
        json!([]),
        initiator,
    );
    let (th_op_a1, th_hash_a1) = make_op(
        &th_affair_id,
        &th_affair_id,
        "content",
        json!({ "kind": "post", "text": "甲的第一条" }),
        person_a,
        NOW + 1000,
    );
    let (th_op_a2, th_hash_a2) = make_op(
        &th_affair_id,
        &th_hash_a1,
        "content",
        json!({ "kind": "post", "text": "甲的第二条" }),
        person_a,
        NOW + 2000,
    );
    let (th_op_b1, th_hash_b1) = make_op(
        &th_affair_id,
        &th_hash_a1,
        "content",
        json!({ "kind": "post", "text": "乙的第一条" }),
        person_b,
        NOW + 3000,
    );
    // 每人取 opHash 字典序最小者；甲的两条只计 min 的那条
    let a_counted = std::cmp::min(th_hash_a1.clone(), th_hash_a2.clone());
    let th_counted = sort_op_hashes(&[a_counted, th_hash_b1.clone()]);
    let th_payload = json!({
        "result": "passed",
        "condition": th_condition,
        "countedOps": th_counted,
        "tally": { "participants": 2 },
        "quorumSnapshot": null,
        "rulesHash": rules_hash(&th_rules),
        "pubPeriod": { "delayMs": 86400000 }
    });
    let (th_resolution_op, th_resolution_hash) = make_op(
        &th_affair_id,
        &th_hash_a2,
        "resolution",
        th_payload,
        initiator,
        NOW + 4000,
    );
    let th_valid_ops = vec![
        CountedOp {
            op_hash: th_hash_a1.clone(),
            op_type: OpType::Content,
            actor_identity: person_a.identity.clone(),
            payload: json!({ "kind": "post", "text": "甲的第一条" }),
        },
        CountedOp {
            op_hash: th_hash_a2.clone(),
            op_type: OpType::Content,
            actor_identity: person_a.identity.clone(),
            payload: json!({ "kind": "post", "text": "甲的第二条" }),
        },
        CountedOp {
            op_hash: th_hash_b1.clone(),
            op_type: OpType::Content,
            actor_identity: person_b.identity.clone(),
            payload: json!({ "kind": "post", "text": "乙的第一条" }),
        },
    ];
    let th_ctx = CloseCtx {
        ladder_voters_size: Some(2),
        ..CloseCtx::empty()
    };
    let th_parsed =
        parse_resolution_payload(&th_resolution_op["payload"]).expect("threshold payload");
    assert_eq!(
        replay_resolution(&th_rules, &th_parsed, &th_valid_ops, &th_ctx),
        ReplayOutcome::Ok,
        "self-check: threshold replay"
    );
    json!({
        "desc": "固定操作集合 → resolution 复算一致性（§6.1：countedOps 排序/rulesHash/condition/pubPeriod 逐项复算）。消费侧负例：countedOps 逆序 → counted-ops-mismatch；rulesHash 篡改 → rules-hash-mismatch；condition.count 改为 3 → condition-not-satisfied；wall-clock 锚定落入 ±10min 容忍带 → condition-not-satisfied；threshold 按人去重计入（同一身份多条操作只计一次）→ 多计入同一人操作 = counted-ops-mismatch；自声明 pubPeriod 与 rules 版本不符 → pub-period-mismatch；自声明 pubPeriod 低于 24h（badPubPeriodMs）在解析期即拒",
        "expect": {
            "affairId": affair_id,
            "rules": rules,
            "ops": [
                { "entry": op_a, "opHash": hash_a },
                { "entry": op_b, "opHash": hash_b },
            ],
            "resolution": { "entry": resolution_op, "opHash": resolution_hash },
            "wallClock": {
                "rules": wc_rules,
                "payload": wc_payload,
                "anchoredMsReached": wc_anchored,
                "anchoredMsAmbiguous": NOW + 86_400_000
            },
            "threshold": {
                "affairId": th_affair_id,
                "rules": th_rules,
                "ops": [
                    { "entry": th_op_a1, "opHash": th_hash_a1 },
                    { "entry": th_op_a2, "opHash": th_hash_a2 },
                    { "entry": th_op_b1, "opHash": th_hash_b1 },
                ],
                "resolution": { "entry": th_resolution_op, "opHash": th_resolution_hash },
                "ladderVotersSize": 2,
                "countedOps": th_counted,
            },
            "badPubPeriodMs": [0, 86_399_999],
        }
    })
}

fn gen_meta_basis_verify(initiator: &FixedKey, person_a: &FixedKey) -> Value {
    let rules = baseline_rules();
    let (genesis, affair_id) = make_genesis(
        rules,
        "旧标题",
        "旧简介",
        json!(["region:110105"]),
        initiator,
    );
    let mechanism =
        json!({ "kind": "delayed-veto", "delayMs": 1000, "vetoThreshold": { "count": 1 } });
    let (revise_op, revise_hash) = make_op(
        &affair_id,
        &affair_id,
        "meta-revise",
        json!({ "title": "新标题", "mechanism": mechanism }),
        initiator,
        NOW + 1000,
    );
    // 非主持人修订（签名有效但不在修订链内，§11.2 仅主持人可提议）
    let (rogue_op, rogue_hash) = make_op(
        &affair_id,
        &affair_id,
        "meta-revise",
        json!({ "title": "伪造标题", "mechanism": mechanism }),
        person_a,
        NOW + 2000,
    );
    let anchored_ms = NOW + 5000;
    let now_ms = anchored_ms + 1000; // delayed-veto 窗口满，无异议 → 生效
    let genesis_meta = AffairMeta {
        title: "旧标题".to_string(),
        summary: "旧简介".to_string(),
        tags: vec!["region:110105".to_string()],
    };
    let entries = [
        MetaReviseEntry {
            op_hash: revise_hash.clone(),
            actor_identity: initiator.identity.clone(),
            payload: parse_meta_revise_payload(&revise_op["payload"]).expect("revise payload"),
            anchored_ms,
            objection_count: 0,
        },
        MetaReviseEntry {
            op_hash: rogue_hash.clone(),
            actor_identity: person_a.identity.clone(),
            payload: parse_meta_revise_payload(&rogue_op["payload"]).expect("rogue payload"),
            anchored_ms,
            objection_count: 0,
        },
    ];
    let generations = derive_meta_generations(
        &affair_id,
        &genesis_meta,
        &initiator.identity,
        &entries,
        now_ms,
    );
    assert_eq!(generations.len(), 2, "self-check: one effective revision");
    let gen1_meta = generations[1].meta.clone();
    let announces = [
        json!({ "name": "verified", "metaSeq": 1, "basisOpHash": revise_hash,
            "meta": { "title": gen1_meta.title, "summary": gen1_meta.summary, "tags": gen1_meta.tags },
            "expect": "verified" }),
        json!({ "name": "wrong-basis", "metaSeq": 1, "basisOpHash": affair_id,
            "meta": { "title": gen1_meta.title, "summary": gen1_meta.summary, "tags": gen1_meta.tags },
            "expect": "conflict" }),
        json!({ "name": "unknown-seq", "metaSeq": 7, "basisOpHash": revise_hash,
            "meta": { "title": gen1_meta.title, "summary": gen1_meta.summary, "tags": gen1_meta.tags },
            "expect": "conflict" }),
        json!({ "name": "stale-content", "metaSeq": 1, "basisOpHash": revise_hash,
            "meta": { "title": "旧标题", "summary": "旧简介", "tags": ["region:110105"] },
            "expect": "conflict" }),
        json!({ "name": "rogue-revise", "metaSeq": 1, "basisOpHash": rogue_hash,
            "meta": { "title": "伪造标题", "summary": "旧简介", "tags": ["region:110105"] },
            "expect": "conflict" }),
        json!({ "name": "no-log-copy", "metaSeq": 1, "basisOpHash": revise_hash,
            "meta": { "title": gen1_meta.title, "summary": gen1_meta.summary, "tags": gen1_meta.tags },
            "expect": "unverified" }),
    ];
    json!({
        "desc": "公告 vs 日志修订链复算（affair-metadata §4 三态：verified/unverified/矛盾丢弃）。genesis 第 0 代 basisOpHash = affairId；主持人 meta-revise 经 delayed-veto 生效后为第 1 代；非主持人修订不进修订链（basisOpHash 指向它即矛盾）",
        "expect": {
            "affairId": affair_id,
            "genesis": genesis,
            "genesisMeta": { "title": genesis_meta.title, "summary": genesis_meta.summary, "tags": genesis_meta.tags },
            "revise": { "entry": revise_op, "opHash": revise_hash },
            "rogueRevise": { "entry": rogue_op, "opHash": rogue_hash },
            "anchoredMs": anchored_ms,
            "nowMs": now_ms,
            "generations": generations.iter().map(|g| json!({
                "metaSeq": g.meta_seq, "basisOpHash": g.basis_op_hash,
                "meta": { "title": g.meta.title, "summary": g.meta.summary, "tags": g.meta.tags }
            })).collect::<Vec<_>>(),
            "announces": announces,
        }
    })
}

fn gen_meta_arbitrate() -> Value {
    let cases = [
        ("seq-wins-over-time", (1, 100, false), (2, 50, false)),
        ("time-tiebreak", (2, 100, false), (2, 200, false)),
        ("incoming-older", (2, 200, false), (2, 100, false)),
        ("verified-guard", (1, 100, true), (2, 999, false)),
        (
            "verified-replaced-by-verified",
            (1, 100, true),
            (2, 50, true),
        ),
        (
            "verified-incoming-lower-seq",
            (2, 100, false),
            (1, 50, true),
        ),
        ("equal-keeps-existing", (2, 100, false), (2, 100, false)),
    ]
    .into_iter()
    .map(|(name, existing, incoming)| {
        let existing_seen = MetaSeen {
            meta_seq: existing.0,
            updated_at: existing.1,
            verified: existing.2,
        };
        let incoming_seen = MetaSeen {
            meta_seq: incoming.0,
            updated_at: incoming.1,
            verified: incoming.2,
        };
        let outcome = match arbitrate_meta(&existing_seen, &incoming_seen) {
            MetaArbitration::Keep => "keep",
            MetaArbitration::Replace => "replace",
        };
        json!({
            "name": name,
            "existing": { "metaSeq": existing.0, "updatedAt": existing.1, "verified": existing.2 },
            "incoming": { "metaSeq": incoming.0, "updatedAt": incoming.1, "verified": incoming.2 },
            "expect": outcome,
        })
    })
    .collect::<Vec<_>>();
    json!({
        "desc": "(metaSeq, updatedAt) 裁决矩阵 + verified 覆盖守卫（affair-metadata §5：metaSeq 优先于 updatedAt；verified 不被 unverified 覆盖）。裁决纯逻辑属 C1；暂存区/淘汰属 C10",
        "expect": { "cases": cases }
    })
}

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community.json");
    let mut vectors: Value = serde_json::from_str(&raw).expect("parse community.json");

    let initiator = key_from_seed(0x11);
    let person_a = key_from_seed(0x21);
    let person_b = key_from_seed(0x22);
    let admin1 = key_from_seed(0x41);

    // 复核：C0 期 JS 参考实现自产的 affair 组逐字节一致
    let fails = cross_check_c0_groups(&vectors);
    if !fails.is_empty() {
        eprintln!("CROSS-CHECK FAILED: {fails:?}");
        std::process::exit(1);
    }

    let out = vectors.as_object_mut().expect("top-level object");
    out.insert("staticCheck".to_string(), gen_static_check(&admin1));
    out.insert(
        "resolutionReplay".to_string(),
        gen_resolution_replay(&initiator, &person_a, &person_b),
    );
    out.insert(
        "metaBasisVerify".to_string(),
        gen_meta_basis_verify(&initiator, &person_a),
    );
    out.insert("metaArbitrate".to_string(), gen_meta_arbitrate());
    // 占位登记回写：C1/C2 六组已产出，其余占位组保持不动（分属 C3/C5/C6）
    out.entry("meta").and_modify(|meta| {
        meta["placeholders"]["desc"] = json!(
            "依赖实现的 case 组（登记于规格文档末节）：ladderDerive（affair §12，C6）；cycleCheck / memberKindEnforce（org-genesis §7，C3）；legacyDegraded（org-signature §6，C3）；readGate.verifyChain 的 policyRef 求值（read-gate §6，C5）。staticCheck / resolutionReplay（affair §12）与 metaBasisVerify / metaArbitrate（affair-metadata §7）已由 C1 core/examples/gen_community_affair_vectors.rs 自产回填；credential.trustTimeline 与 readGate.verifyChain（第 1–4 步）已由 C2 core/examples/gen_credential_vectors.rs 自产回填。"
        );
    });
    vectors["_comment"] = json!(
        "community-affairs golden vectors（wiki/protocol/community/ 协议）。C0 组由 code/spec/gen-community-vectors.mjs（Node 参考实现）自产；C1 组（staticCheck/resolutionReplay/metaBasisVerify/metaArbitrate）由 code/core/examples/gen_community_affair_vectors.rs 自产并逐字节复核 C0 affair 组；C2 组由 code/core/examples/gen_credential_vectors.rs 自产。消费：core/tests/community_affair_vectors.rs（C1）、community_credential_vectors.rs（C2）。"
    );

    std::fs::write(path, serde_json::to_string_pretty(&vectors).unwrap() + "\n")
        .expect("write community.json");
    println!(
        "OK: community.json updated (C1 groups: staticCheck / resolutionReplay / metaBasisVerify / metaArbitrate), C0 affair groups cross-checked byte-identical."
    );
}
