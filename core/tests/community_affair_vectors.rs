//! community-affairs C1（affair 事务容器）golden vectors 消费测试：
//! 加载 `../spec/vectors/community.json` 中 affair 相关 case 组逐条断言。
//!
//! 向量来源：C0 组（affairGenesis/opChain/ruleMechanisms/snapshot）由
//! `code/spec/gen-community-vectors.mjs`（Node 参考实现）自产；
//! C1 组（staticCheck/resolutionReplay/metaBasisVerify/metaArbitrate）由
//! `code/core/examples/gen_community_affair_vectors.rs` 自产，并在生成前对
//! C0 affair 组做了逐字节复核（两侧独立实现互证）。
//!
//! 规格权威：wiki/protocol/community/affair.md 与 affair-metadata.md。

use serde_json::{Value, json};
use spark_core::affair::*;

const NOW: i64 = 1_720_000_000_000;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

fn hex64(seed: &str) -> String {
    seed.repeat(64 / seed.len())
}

/// affairGenesis 组（affair §2）：canonical 载荷逐字节 + affairId 自认证复算 +
/// 验签；篡改任一字段 affairId/验签必败。
#[test]
fn affair_genesis_cross_validation() {
    let v = vectors();
    let section = &v["affairGenesis"]["expect"];
    let record = &section["record"];

    assert_eq!(
        genesis_sign_payload(record).unwrap(),
        section["payload"].as_str().unwrap(),
        "canonical payload bytes"
    );
    assert_eq!(
        compute_affair_id(record).unwrap(),
        section["affairId"].as_str().unwrap(),
        "affairId self-certifying"
    );
    let (genesis, affair_id, _rules) = verify_genesis(record).expect("genesis verifies");
    assert_eq!(genesis.initiator.identity, record["initiator"]["identity"]);
    assert_eq!(affair_id, section["affairId"].as_str().unwrap());

    // 篡改 title → affairId 变、验签必败
    let mut tampered = record.clone();
    tampered["title"] = json!("被篡改的标题");
    assert_ne!(
        compute_affair_id(&tampered).unwrap(),
        section["affairId"].as_str().unwrap()
    );
    assert_eq!(
        verify_genesis(&tampered).unwrap_err(),
        GenesisReject::InvalidSignature
    );

    // 篡改 rules（affairId 承诺规则文档）→ affairId 变
    let mut tampered = record.clone();
    tampered["rules"]["pubPeriod"] = json!({ "delayMs": 172800000 });
    assert_ne!(
        compute_affair_id(&tampered).unwrap(),
        section["affairId"].as_str().unwrap()
    );
}

/// opChain 组（affair §3/§8）：opHash 链逐字节（首条 prevOpHash = affairId）、
/// 合法 DAG 分支、sortKey 排序、入站校验链（新鲜度/暂存待补/去重/未知 opType）。
#[test]
fn op_chain_cross_validation() {
    let v = vectors();
    let affair_id = v["affairGenesis"]["expect"]["affairId"].as_str().unwrap();
    let section = &v["opChain"]["expect"];
    let ops = section["ops"].as_array().unwrap();

    let mut hashes = Vec::new();
    for op_case in ops {
        let entry = &op_case["entry"];
        assert_eq!(
            op_sign_payload(entry).unwrap(),
            op_case["payload"].as_str().unwrap(),
            "op payload bytes"
        );
        let op_hash = compute_op_hash(entry).unwrap();
        assert_eq!(op_hash, op_case["opHash"].as_str().unwrap(), "opHash bytes");
        // declaredAt 注入为 now：验签 + 结构全过
        let declared_at = entry["declaredAt"].as_i64().unwrap();
        let (op, _target) = verify_op(entry, affair_id, declared_at).expect("op verifies");
        assert_eq!(op.actor.identity, entry["actor"]["identity"]);
        // 新鲜度窗口边界（±10min）：窗口内收，超窗拒
        assert!(verify_op(entry, affair_id, declared_at + OP_FRESHNESS_WINDOW_MS).is_ok());
        assert_eq!(
            verify_op(entry, affair_id, declared_at + OP_FRESHNESS_WINDOW_MS + 1).unwrap_err(),
            OpReject::StaleDeclaredAt
        );
        hashes.push(op_hash);
    }

    // sortKey = opHash 字典序（§8）
    let expected_sort: Vec<String> = section["sortOrder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_string())
        .collect();
    assert_eq!(sort_op_hashes(&hashes), expected_sort);

    // 首条 prevOpHash = affairId（创世承诺即链根）；op2/op3 同指 op1 = 合法 DAG 分支
    assert_eq!(ops[0]["entry"]["prevOpHash"].as_str().unwrap(), affair_id);
    assert_eq!(ops[1]["entry"]["prevOpHash"], ops[2]["entry"]["prevOpHash"]);

    // 乱序入站：op2/op3 先到 → 暂存待补；op1 到达后全部 drain；heads = {op2, op3}
    let initiator = v["meta"]["actors"]["initiator"]["identity"]
        .as_str()
        .unwrap();
    let mut log = OpLog::new(affair_id.to_string(), initiator.to_string());
    let now = NOW + 3000;
    assert!(matches!(
        log.ingest(&ops[2]["entry"], now),
        Inbound::Pending { .. }
    ));
    assert!(matches!(
        log.ingest(&ops[1]["entry"], now),
        Inbound::Pending { .. }
    ));
    assert_eq!(log.pending_len(), 2);
    assert!(matches!(
        log.ingest(&ops[0]["entry"], now),
        Inbound::Accepted { .. }
    ));
    assert_eq!(log.pending_len(), 0);
    assert_eq!(
        log.heads(),
        sort_op_hashes(&[
            ops[1]["opHash"].as_str().unwrap().to_string(),
            ops[2]["opHash"].as_str().unwrap().to_string()
        ])
    );
    // 重复入站 → Duplicate
    assert!(matches!(
        log.ingest(&ops[0]["entry"], now),
        Inbound::Duplicate { .. }
    ));

    // 未知 opType：整条拒收（fail-closed，§4）
    let mut unknown = ops[0]["entry"].clone();
    unknown["opType"] = json!("plugin-custom");
    assert!(matches!(
        log.ingest(&unknown, now),
        Inbound::Rejected(OpReject::UnknownOpType)
    ));

    // 跨事务搬迁：affairId 不匹配 → 拒收
    let mut foreign = ops[0]["entry"].clone();
    foreign["affairId"] = json!(hex64("ab"));
    assert!(matches!(
        log.ingest(&foreign, now),
        Inbound::Rejected(OpReject::AffairMismatch)
    ));
}

/// ruleMechanisms 组（affair §5.3）：三形态机制文档 canonical 逐字节 + 解析。
#[test]
fn rule_mechanisms_cross_validation() {
    let v = vectors();
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
    let cases = [
        (
            "vote",
            json!({ "kind": "vote", "voterSet": "ladder:voters",
                "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 },
                "snapshot": "required" }),
        ),
        (
            "multisig",
            json!({ "kind": "multisig", "m": 2, "n": 3, "signers": admins.clone() }),
        ),
        (
            "delayedVeto",
            json!({ "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } }),
        ),
    ];
    for (key, doc) in cases {
        assert_eq!(
            spark_core::evidence::normalize_object(&doc),
            section[key].as_str().unwrap(),
            "{key} canonical bytes"
        );
        let mechanism = parse_mechanism(&doc).unwrap_or_else(|e| panic!("{key} parses: {e:?}"));
        match (key, &mechanism) {
            (
                "vote",
                Mechanism::Vote {
                    threshold, quorum, ..
                },
            ) => {
                assert_eq!(*threshold, Fraction { num: 1, den: 2 });
                assert_eq!(*quorum, Fraction { num: 1, den: 2 });
            }
            ("multisig", Mechanism::Multisig { m, n, signers }) => {
                assert_eq!((*m, *n), (2, 3));
                assert_eq!(*signers, admins);
            }
            (
                "delayedVeto",
                Mechanism::DelayedVeto {
                    delay_ms,
                    veto_count,
                },
            ) => {
                assert_eq!((*delay_ms, *veto_count), (259200000, 3));
            }
            _ => panic!("{key} kind mismatch"),
        }
    }
}

/// snapshot 组（affair §9）：rosterHash / memberSetHash 固定值（排序口径）。
#[test]
fn snapshot_cross_validation() {
    let v = vectors();
    let section = &v["snapshot"]["expect"];
    let identities: Vec<String> = section["roster"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["identity"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        roster_hash(&identities).unwrap(),
        section["rosterHash"].as_str().unwrap(),
        "rosterHash"
    );
    // 乱序输入同一哈希（确定性，与到达顺序无关）
    let mut shuffled = identities.clone();
    shuffled.reverse();
    assert_eq!(
        roster_hash(&shuffled).unwrap(),
        section["rosterHash"].as_str().unwrap()
    );
    let members = section["memberSet"].as_array().unwrap().clone();
    assert_eq!(
        member_set_hash(&members).unwrap(),
        section["memberSetHash"].as_str().unwrap(),
        "memberSetHash"
    );
}

/// staticCheck 组（affair §5.6）：拒绝用例集逐条对齐 reason。
#[test]
fn static_check_cross_validation() {
    let v = vectors();
    for case in v["staticCheck"]["expect"]["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let outcome = static_check_rules(&case["rules"]);
        let expect_ok = case["expect"]["ok"].as_bool().unwrap();
        assert_eq!(outcome.is_ok(), expect_ok, "{name}");
        if !expect_ok {
            assert_eq!(
                outcome.unwrap_err().reason(),
                case["expect"]["reason"].as_str().unwrap(),
                "{name} reason"
            );
        }
    }
}

/// resolutionReplay 组（affair §6.1）：固定操作集合 → resolution 复算一致性；
/// 负例（countedOps 逆序/rulesHash 篡改/condition 篡改/容忍带内锚定）逐项对齐。
#[test]
fn resolution_replay_cross_validation() {
    let v = vectors();
    let section = &v["resolutionReplay"]["expect"];
    let rules = &section["rules"];

    let valid_ops: Vec<CountedOp> = section["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op_case| {
            let entry = &op_case["entry"];
            CountedOp {
                op_hash: op_case["opHash"].as_str().unwrap().to_string(),
                op_type: parse_op_type(entry["opType"].as_str().unwrap()).unwrap(),
                actor_identity: entry["actor"]["identity"].as_str().unwrap().to_string(),
                payload: entry["payload"].clone(),
            }
        })
        .collect();
    let payload =
        parse_resolution_payload(&section["resolution"]["entry"]["payload"]).expect("payload");

    // 正例：同一操作集合复算一致
    assert_eq!(
        replay_resolution(rules, &payload, &valid_ops, &CloseCtx::empty()),
        ReplayOutcome::Ok
    );

    // countedOps 逆序 → counted-ops-mismatch
    let mut reversed = payload.clone();
    reversed.counted_ops.reverse();
    assert_eq!(
        replay_resolution(rules, &reversed, &valid_ops, &CloseCtx::empty()),
        ReplayOutcome::CountedOpsMismatch
    );

    // rulesHash 篡改 → rules-hash-mismatch
    let mut bad_hash = payload.clone();
    bad_hash.rules_hash = hex64("ff");
    assert_eq!(
        replay_resolution(rules, &bad_hash, &valid_ops, &CloseCtx::empty()),
        ReplayOutcome::RulesHashMismatch
    );

    // condition.count 改为 3（操作集合上未满足）→ condition-not-satisfied
    let mut bad_cond = payload.clone();
    bad_cond.condition["count"] = json!(3);
    assert_eq!(
        replay_resolution(rules, &bad_cond, &valid_ops, &CloseCtx::empty()),
        ReplayOutcome::ConditionNotSatisfied
    );

    // wall-clock 变体：越过容忍带的锚定复算一致；容忍带内视为未满足
    let wc = &section["wallClock"];
    let wc_payload = parse_resolution_payload(&wc["payload"]).expect("wc payload");
    let wc_ctx = |anchored: i64| CloseCtx {
        anchored_ms: Some(anchored),
        ..CloseCtx::empty()
    };
    assert_eq!(
        replay_resolution(
            &wc["rules"],
            &wc_payload,
            &[],
            &wc_ctx(wc["anchoredMsReached"].as_i64().unwrap())
        ),
        ReplayOutcome::Ok
    );
    assert_eq!(
        replay_resolution(
            &wc["rules"],
            &wc_payload,
            &[],
            &wc_ctx(wc["anchoredMsAmbiguous"].as_i64().unwrap())
        ),
        ReplayOutcome::ConditionNotSatisfied
    );

    // threshold 变体（§5.2）：按人去重计入——同一身份多条操作只计一次，
    // 每人取 opHash 字典序最小者
    let th = &section["threshold"];
    let th_ops: Vec<CountedOp> = th["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op_case| {
            let entry = &op_case["entry"];
            CountedOp {
                op_hash: op_case["opHash"].as_str().unwrap().to_string(),
                op_type: parse_op_type(entry["opType"].as_str().unwrap()).unwrap(),
                actor_identity: entry["actor"]["identity"].as_str().unwrap().to_string(),
                payload: entry["payload"].clone(),
            }
        })
        .collect();
    let th_payload =
        parse_resolution_payload(&th["resolution"]["entry"]["payload"]).expect("threshold payload");
    let th_ctx = CloseCtx {
        ladder_voters_size: th["ladderVotersSize"].as_u64(),
        ..CloseCtx::empty()
    };
    // 正例：复算一致，countedOps 与向量登记相同（甲的两条操作只计入 opHash 较小者）
    assert_eq!(
        replay_resolution(&th["rules"], &th_payload, &th_ops, &th_ctx),
        ReplayOutcome::Ok
    );
    let expected_counted: Vec<String> = th["countedOps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_string())
        .collect();
    assert_eq!(th_payload.counted_ops, expected_counted);
    assert_eq!(expected_counted.len(), 2, "甲的两条操作按人去重只计一次");

    // 负例：把甲的两条操作都计入（去重失效）→ counted-ops-mismatch
    let mut greedy = th_payload.clone();
    greedy.counted_ops = sort_op_hashes(
        &th["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op_case| op_case["opHash"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        replay_resolution(&th["rules"], &greedy, &th_ops, &th_ctx),
        ReplayOutcome::CountedOpsMismatch
    );

    // 负例（评审阻塞 1）：自声明公示期与 rules 版本不符 → pub-period-mismatch
    let mut bad_pub_period = th_payload.clone();
    bad_pub_period.pub_period_ms = 2 * DEFAULT_PUB_PERIOD_MS;
    assert_eq!(
        replay_resolution(&th["rules"], &bad_pub_period, &th_ops, &th_ctx),
        ReplayOutcome::PubPeriodMismatch
    );

    // 负例（评审阻塞 1）：自声明公示期低于 24h 在解析期即拒
    for bad_ms in section["badPubPeriodMs"].as_array().unwrap() {
        let mut raw = th["resolution"]["entry"]["payload"].clone();
        raw["pubPeriod"]["delayMs"] = bad_ms.clone();
        assert_eq!(
            parse_resolution_payload(&raw).unwrap_err(),
            "bad-pub-period"
        );
    }

    // 决议两态（§6.2）：公示期内待确认；公示期满生效；阈值异议打回
    let anchored = NOW;
    let day = DEFAULT_PUB_PERIOD_MS;
    assert_eq!(
        resolution_state(anchored, day, 0, 1, anchored + day - 1),
        ResolutionState::Pending
    );
    assert_eq!(
        resolution_state(anchored, day, 0, 1, anchored + day),
        ResolutionState::Effective
    );
    assert_eq!(
        resolution_state(anchored, day, 1, 1, anchored + 1000),
        ResolutionState::Vetoed
    );
}

/// metaBasisVerify 组（affair §11.2 + affair-metadata §4）：修订链复算与
/// 公告三态（verified/unverified/矛盾丢弃）；非主持人修订不进链。
#[test]
fn meta_basis_verify_cross_validation() {
    let v = vectors();
    let section = &v["metaBasisVerify"]["expect"];
    let affair_id = section["affairId"].as_str().unwrap();
    let moderator = section["genesis"]["initiator"]["identity"]
        .as_str()
        .unwrap();
    let anchored_ms = section["anchoredMs"].as_i64().unwrap();
    let now_ms = section["nowMs"].as_i64().unwrap();
    let genesis_meta = AffairMeta {
        title: section["genesisMeta"]["title"]
            .as_str()
            .unwrap()
            .to_string(),
        summary: section["genesisMeta"]["summary"]
            .as_str()
            .unwrap()
            .to_string(),
        tags: section["genesisMeta"]["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect(),
    };

    let entries = [
        ("revise", moderator.to_string()),
        (
            "rogueRevise",
            section["rogueRevise"]["entry"]["actor"]["identity"]
                .as_str()
                .unwrap()
                .to_string(),
        ),
    ]
    .map(|(key, actor)| MetaReviseEntry {
        op_hash: section[key]["opHash"].as_str().unwrap().to_string(),
        actor_identity: actor,
        payload: parse_meta_revise_payload(&section[key]["entry"]["payload"]).unwrap(),
        anchored_ms,
        objection_count: 0,
    });
    let generations =
        derive_meta_generations(affair_id, &genesis_meta, moderator, &entries, now_ms);

    // 与向量登记的代际逐字段一致（rogue 修订不进链 → 只有 2 代）
    let expected = section["generations"].as_array().unwrap();
    assert_eq!(generations.len(), expected.len());
    for (got, want) in generations.iter().zip(expected) {
        assert_eq!(got.meta_seq, want["metaSeq"].as_u64().unwrap());
        assert_eq!(got.basis_op_hash, want["basisOpHash"].as_str().unwrap());
        assert_eq!(got.meta.title, want["meta"]["title"].as_str().unwrap());
    }

    // 公告三态逐条对齐
    for announce in section["announces"].as_array().unwrap() {
        let name = announce["name"].as_str().unwrap();
        let meta = AffairMeta {
            title: announce["meta"]["title"].as_str().unwrap().to_string(),
            summary: announce["meta"]["summary"].as_str().unwrap().to_string(),
            tags: announce["meta"]["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect(),
        };
        let expect = announce["expect"].as_str().unwrap();
        // unverified 用例 = 无日志副本（generations 传 None）
        let gens = if expect == "unverified" {
            None
        } else {
            Some(generations.as_slice())
        };
        let verdict = verify_meta_announce(
            announce["metaSeq"].as_u64().unwrap(),
            announce["basisOpHash"].as_str().unwrap(),
            &meta,
            gens,
        );
        assert_eq!(
            format!("{verdict:?}").to_lowercase(),
            expect,
            "announce {name}"
        );
    }

    // 非主持人 meta-revise 入站即拒（§11：拒入有效集）
    let mut log = OpLog::new(affair_id.to_string(), moderator.to_string());
    assert!(matches!(
        log.ingest(&section["rogueRevise"]["entry"], NOW + 2000),
        Inbound::Rejected(OpReject::NotModerator)
    ));
    assert!(matches!(
        log.ingest(&section["revise"]["entry"], NOW + 1000),
        Inbound::Accepted { .. }
    ));
}

/// metaArbitrate 组（affair-metadata §5）：(metaSeq, updatedAt) 裁决矩阵 +
/// verified 覆盖守卫。
#[test]
fn meta_arbitrate_cross_validation() {
    let v = vectors();
    for case in v["metaArbitrate"]["expect"]["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let seen = |key: &str| MetaSeen {
            meta_seq: case[key]["metaSeq"].as_u64().unwrap(),
            updated_at: case[key]["updatedAt"].as_i64().unwrap(),
            verified: case[key]["verified"].as_bool().unwrap(),
        };
        let outcome = arbitrate_meta(&seen("existing"), &seen("incoming"));
        let expected = match case["expect"].as_str().unwrap() {
            "keep" => MetaArbitration::Keep,
            "replace" => MetaArbitration::Replace,
            other => panic!("unknown arbitration expect {other}"),
        };
        assert_eq!(outcome, expected, "{name}");
    }
}
