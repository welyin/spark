//! community-affairs C6（账龄/阶梯推导）golden vectors 消费测试：
//! 加载 `../spec/vectors/community.json` 的 `ladderDerive` 组逐条回放断言。
//!
//! 向量由 `code/core/examples/gen_community_ladder_vectors.rs` 自产回填
//! （生成器即规格的可执行重述）。规格权威：wiki/protocol/community/affair.md
//! §5.5/§12/§13。

use serde_json::Value;
use spark_core::affair::*;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community vectors");
    serde_json::from_str(&raw).expect("parse community vectors")
}

/// 向量操作视图 → LadderOpView（opType/actorKind 字符串回枚举）。
fn view_from(case: &Value, op: &Value) -> LadderOpView {
    LadderOpView {
        op_hash: op["opHash"].as_str().unwrap().to_string(),
        actor_kind: match op["actorKind"].as_str().unwrap() {
            "person" => ActorKind::Person,
            "org" => ActorKind::Org,
            other => panic!("{}: unknown actorKind {other}", case["name"]),
        },
        actor_identity: op["actorIdentity"].as_str().unwrap().to_string(),
        op_type: parse_op_type(op["opType"].as_str().unwrap())
            .unwrap_or_else(|| panic!("{}: unknown opType", case["name"])),
        payload: op["payload"].clone(),
        anchored_ms: op["anchoredMs"].as_i64(),
        objection_count: op["objections"].as_u64().unwrap(),
    }
}

fn tier_of(s: &str) -> LadderTier {
    match s {
        "observer" => LadderTier::Observer,
        "contributor" => LadderTier::Contributor,
        "voter" => LadderTier::Voter,
        other => panic!("unknown tier {other}"),
    }
}

/// ladderDerive 组（affair §5.5/§13）：逐 case 从向量操作集合重推名册，
/// 逐字段比对（tier/accepts/tierSinceMs/lastActivityMs/accountAgeMs）+ 投票者
/// 集合 + 乱序输入等价（确定性，与到达顺序无关，§8）。
#[test]
fn ladder_derive_replay() {
    let v = vectors();
    let cases = v["ladderDerive"]["expect"]["cases"].as_array().unwrap();
    assert!(cases.len() >= 7, "ladderDerive cases present");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let params = LadderParams::from_participation(&case["participation"])
            .unwrap_or_else(|e| panic!("{name}: params rejected: {e}"));
        let views: Vec<LadderOpView> = case["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| view_from(case, op))
            .collect();
        let initial_voters: Vec<String> = case["initialVoters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let now_ms = case["nowMs"].as_i64().unwrap();
        let input = LadderInput {
            initial_voters: &initial_voters,
            genesis_anchored_ms: case["genesisAnchoredMs"].as_i64(),
            ops: &views,
            params,
            now_ms,
        };
        let roster = derive_ladder(&input);
        let expected = case["roster"].as_array().unwrap();
        assert_eq!(roster.entries.len(), expected.len(), "{name}: roster size");
        for (entry, want) in roster.entries.iter().zip(expected) {
            assert_eq!(entry.identity, want["identity"], "{name}: identity order");
            assert_eq!(
                entry.tier,
                tier_of(want["tier"].as_str().unwrap()),
                "{name}: {} tier",
                entry.identity
            );
            assert_eq!(
                entry.accepts,
                want["accepts"].as_u64().unwrap(),
                "{name}: accepts"
            );
            assert_eq!(
                entry.tier_since_ms,
                want["tierSinceMs"].as_i64(),
                "{name}: tierSinceMs"
            );
            assert_eq!(
                entry.last_activity_ms,
                want["lastActivityMs"].as_i64(),
                "{name}: lastActivityMs"
            );
            assert_eq!(
                account_age_ms(&entry.identity, &views, now_ms),
                want["accountAgeMs"].as_i64(),
                "{name}: accountAgeMs"
            );
        }
        let voters: Vec<String> = case["voters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(roster.voter_identities(), voters, "{name}: voters");

        // 确定性：同一操作集合任意排列 → 名册逐字节一致。
        let mut shuffled = views.clone();
        shuffled.reverse();
        let shuffled_roster = derive_ladder(&LadderInput {
            ops: &shuffled,
            ..input
        });
        assert_eq!(roster, shuffled_roster, "{name}: order-independent");
    }
}

/// ladderParams 形状/数值非法 → fail-closed 拒绝（§5.5 静态检查口径）。
#[test]
fn ladder_bad_params_rejected() {
    let v = vectors();
    for case in v["ladderDerive"]["expect"]["badParams"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            LadderParams::from_participation(&case["participation"]).unwrap_err(),
            case["error"].as_str().unwrap(),
            "{name}"
        );
    }
}
