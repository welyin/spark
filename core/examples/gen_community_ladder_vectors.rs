//! community-affairs C6 golden vectors 生成器：回填 `../spec/vectors/community.json`
//! 的 `ladderDerive` 组（规格：wiki/protocol/community/affair.md §5.5/§12/§13），
//! 其余组原样保留。
//!
//! 运行：`cargo run --example gen_community_ladder_vectors`。
//!
//! 输入为「已验签有效操作集合」的抽象视图（opHash/actor/opType/payload/锚定时刻/
//! 异议计数——验签与入站判定属 §3，不在推导面内），opHash 以
//! sha256("{case}:{seq}") 固定生成；身份取自 meta.actors（与 C0–C4 组同一组
//! 固定密钥）。输出名册由 `derive_ladder` 自产（生成器即规格的可执行重述），
//! 消费测试 `core/tests/community_ladder_vectors.rs` 逐字段回放断言。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::affair::*;

const NOW: i64 = 1_720_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

struct CaseBuilder {
    name: String,
    initial_voters: Vec<String>,
    genesis_anchored_ms: Option<i64>,
    participation: Value,
    ops: Vec<LadderOpView>,
    now_ms: i64,
}

impl CaseBuilder {
    fn new(name: &str, now_ms: i64) -> Self {
        Self {
            name: name.to_string(),
            initial_voters: Vec::new(),
            genesis_anchored_ms: None,
            participation: json!({}),
            ops: Vec::new(),
            now_ms,
        }
    }

    /// 追加一条操作视图；opHash = sha256("{case}:{序号}")（确定性、互不相同）。
    fn op(
        &mut self,
        actor_kind: ActorKind,
        identity: &str,
        op_type: OpType,
        payload: Value,
        anchored_ms: Option<i64>,
        objections: u64,
    ) -> &mut Self {
        let seq = self.ops.len();
        let op_hash = hex::encode(Sha256::digest(format!("{}:{seq}", self.name).as_bytes()));
        self.ops.push(LadderOpView {
            op_hash,
            actor_kind,
            actor_identity: identity.to_string(),
            op_type,
            payload,
            anchored_ms,
            objection_count: objections,
        });
        self
    }

    /// delayed-veto 形态的 meta-revise 提议载荷（阶梯采纳的唯一计入口径）。
    fn adopt_op(&mut self, identity: &str, anchored_ms: Option<i64>, objections: u64) -> &mut Self {
        self.op(
            ActorKind::Person,
            identity,
            OpType::MetaRevise,
            json!({
                "title": format!("修订 {}", self.ops.len()),
                "mechanism": { "kind": "delayed-veto", "delayMs": DAY, "vetoThreshold": { "count": 1 } }
            }),
            anchored_ms,
            objections,
        )
    }

    fn content_op(&mut self, identity: &str, anchored_ms: Option<i64>) -> &mut Self {
        self.op(
            ActorKind::Person,
            identity,
            OpType::Content,
            json!({ "kind": "post", "text": "内容" }),
            anchored_ms,
            0,
        )
    }

    fn build(&self) -> Value {
        let params = LadderParams::from_participation(&self.participation).expect("params valid");
        let roster = derive_ladder(&LadderInput {
            initial_voters: &self.initial_voters,
            genesis_anchored_ms: self.genesis_anchored_ms,
            ops: &self.ops,
            params,
            now_ms: self.now_ms,
        });
        let ops_json: Vec<Value> = self
            .ops
            .iter()
            .map(|op| {
                json!({
                    "opHash": op.op_hash,
                    "actorKind": match op.actor_kind {
                        ActorKind::Person => "person",
                        ActorKind::Org => "org",
                    },
                    "actorIdentity": op.actor_identity,
                    "opType": op.op_type.as_str(),
                    "payload": op.payload,
                    "anchoredMs": op.anchored_ms,
                    "objections": op.objection_count,
                })
            })
            .collect();
        let roster_json: Vec<Value> = roster
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "identity": entry.identity,
                    "tier": entry.tier.as_str(),
                    "accepts": entry.accepts,
                    "tierSinceMs": entry.tier_since_ms,
                    "lastActivityMs": entry.last_activity_ms,
                    "accountAgeMs": account_age_ms(&entry.identity, &self.ops, self.now_ms),
                })
            })
            .collect();
        json!({
            "name": self.name,
            "initialVoters": self.initial_voters,
            "genesisAnchoredMs": self.genesis_anchored_ms,
            "participation": self.participation,
            "ops": ops_json,
            "nowMs": self.now_ms,
            "roster": roster_json,
            "voters": roster.voter_identities(),
        })
    }
}

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community.json");
    let mut vectors: Value = serde_json::from_str(&raw).expect("parse community.json");
    let actor = |name: &str| {
        vectors["meta"]["actors"][name]["identity"]
            .as_str()
            .expect("meta.actors")
            .to_string()
    };
    let initiator = actor("initiator");
    let person_a = actor("personA");
    let person_b = actor("personB");
    let org_root = actor("orgRoot");

    let mut cases = Vec::new();

    // 缺省参数全链路：3 次采纳 + 在级 39 天 + 近窗活跃 → 投票者；1 次采纳 →
    // 贡献者；仅内容 → 观察者；未锚定操作不参与推导；org 表态不进名册。
    let mut c = CaseBuilder::new("default-lifecycle", NOW);
    c.genesis_anchored_ms = Some(NOW - 80 * DAY);
    c.adopt_op(&person_a, Some(NOW - 40 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 38 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 36 * DAY), 0)
        .adopt_op(&person_b, Some(NOW - 10 * DAY), 0)
        .content_op(&person_b, Some(NOW - 2 * DAY))
        .content_op(&initiator, Some(NOW - DAY))
        .adopt_op(&initiator, None, 0) // 未锚定：不计采纳也不构成活动
        .op(
            ActorKind::Org,
            &org_root,
            OpType::MetaRevise,
            json!({ "title": "组织表态", "mechanism": { "kind": "delayed-veto", "delayMs": DAY, "vetoThreshold": { "count": 1 } } }),
            Some(NOW - 5 * DAY),
            0,
        );
    let case = c.build();
    assert_eq!(case["roster"].as_array().unwrap().len(), 3, "org 不进名册");
    cases.push(case);

    // 在级衰减：投票者近 90 天无链上活动 → 降回贡献者（采纳保留累计）。
    let mut c = CaseBuilder::new("voter-decay", NOW);
    c.adopt_op(&person_a, Some(NOW - 120 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 118 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 116 * DAY), 0);
    cases.push(c.build());

    // 衰减后恢复活动 → 回级投票者（在级起点保留首次采纳生效时刻）。
    let mut c = CaseBuilder::new("revive-after-decay", NOW);
    c.adopt_op(&person_a, Some(NOW - 120 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 118 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 116 * DAY), 0)
        .content_op(&person_a, Some(NOW - DAY));
    cases.push(c.build());

    // 初始投票者冷启动：无链上活动 → 无衰减时刻可判，保留投票者；
    // 在级起点 = 创世锚定时刻，账龄无（无任何署名操作）。
    let mut c = CaseBuilder::new("initial-voter-cold-start", NOW);
    c.initial_voters = vec![initiator.clone()];
    c.genesis_anchored_ms = Some(NOW - 50 * DAY);
    c.content_op(&person_a, Some(NOW - DAY));
    cases.push(c.build());

    // 初始投票者有链上活动且超衰减窗 → 观察者（零采纳，无贡献者级可降）。
    let mut c = CaseBuilder::new("initial-voter-decayed", NOW);
    c.initial_voters = vec![initiator.clone()];
    c.genesis_anchored_ms = Some(NOW - 200 * DAY);
    c.content_op(&initiator, Some(NOW - 100 * DAY));
    cases.push(c.build());

    // 异议达否决阈值 → 不采纳（不计入阶梯）。
    let mut c = CaseBuilder::new("veto-blocks-adoption", NOW);
    c.adopt_op(&person_a, Some(NOW - 40 * DAY), 1);
    cases.push(c.build());

    // 参数覆盖（§5.5 ladderParams，voterDays 单位为天）：缺省下 2 次采纳仅
    // 贡献者，覆盖后（2 次采纳 + 在级 10 天）即投票者。
    let mut c = CaseBuilder::new("params-override", NOW);
    c.participation = json!({
        "ladderParams": { "contributorAccepts": 2, "voterDays": 10, "voterAccepts": 2 }
    });
    c.adopt_op(&person_a, Some(NOW - 20 * DAY), 0)
        .adopt_op(&person_a, Some(NOW - 15 * DAY), 0);
    cases.push(c.build());

    let group = json!({
        "desc": "固定操作日志 → 阶梯名册确定性推导（affair §5.5/§13）：observer/contributor/voter 三级、delayed-veto 生效采纳口径、在级天数（voterDays 单位天）、近窗活跃与 90 天衰减、初始投票者冷启动与衰减、org 表态不进名册、未锚定操作不参与时间推导。名册按 identity 字典序；accountAgeMs = nowMs − 最早锚定活跃时刻",
        "expect": {
            "cases": cases,
            "badParams": [
                { "name": "voter-days-zero", "participation": { "ladderParams": { "voterDays": 0 } }, "error": "bad-ladder-params" },
                { "name": "contributor-accepts-zero", "participation": { "ladderParams": { "contributorAccepts": 0 } }, "error": "bad-ladder-params" },
                { "name": "not-an-object", "participation": { "ladderParams": "bad" }, "error": "bad-ladder-params" }
            ]
        }
    });

    let out = vectors.as_object_mut().expect("top-level object");
    out.insert("ladderDerive".to_string(), group);
    // 占位登记回写：ladderDerive 已产出，其余占位组保持不动
    out.entry("meta").and_modify(|meta| {
        meta["placeholders"]["desc"] = json!(
            "依赖实现的 case 组（登记于规格文档末节）：readGate.verifyChain 的 policyRef 求值（read-gate §6，C5）。staticCheck / resolutionReplay（affair §12）与 metaBasisVerify / metaArbitrate（affair-metadata §7）已由 C1 core/examples/gen_community_affair_vectors.rs 自产回填；credential.trustTimeline 与 readGate.verifyChain（第 1–4 步）已由 C2 core/examples/gen_credential_vectors.rs 自产回填；cycleCheck / memberKindEnforce（org-genesis §7）与 legacyDegraded（org-signature §6）已由 C3 core/examples/gen_community_orgmember_vectors.rs 自产回填；ladderDerive（affair §12）已由 C6 core/examples/gen_community_ladder_vectors.rs 自产回填。"
        );
    });
    vectors["_comment"] = json!(
        "community-affairs golden vectors（wiki/protocol/community/ 协议）。C0 组由 code/spec/gen-community-vectors.mjs（Node 参考实现）自产；C1 组（staticCheck/resolutionReplay/metaBasisVerify/metaArbitrate）由 code/core/examples/gen_community_affair_vectors.rs 自产并逐字节复核 C0 affair 组；C2 组由 code/core/examples/gen_credential_vectors.rs 自产；C4 组（affairSync，事务复制面信封线形）由 code/core/examples/gen_community_affair_sync_vectors.rs 自产；C6 组（ladderDerive，账龄/阶梯推导）由 code/core/examples/gen_community_ladder_vectors.rs 自产。消费：core/tests/community_affair_vectors.rs（C1）、community_credential_vectors.rs（C2）、community_affair_sync_vectors.rs（C4）、community_ladder_vectors.rs（C6）。"
    );

    std::fs::write(path, serde_json::to_string_pretty(&vectors).unwrap() + "\n")
        .expect("write community.json");
    println!("OK: community.json updated (C6 group: ladderDerive), other groups untouched.");
}
