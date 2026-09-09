//! A2 配额与驱逐 golden vectors 生成器：
//! `cargo run -p spark-core --example gen_blob_quota_vectors`
//! 输出 `code/spec/vectors/blob_quota.json`（消费测试 `tests/blob_quota_vectors.rs`）。
//!
//! 字节级权威：`wiki/protocol/p2p/personal-data-sync.md` §15（含位次规则）。
//! 所有取值由 spark_core 公开 API 推导，重复运行字节级一致。

use serde_json::{Value, json};
use spark_core::sync::blob::{
    DEFAULT_QUOTA_MOBILE_BYTES, DEFAULT_QUOTA_PC_BYTES, EvictionState, k_target, plan_eviction,
};

fn state(cid: &str, bytes: u64, access: i64, holders: &[&str]) -> EvictionState {
    EvictionState {
        cid: cid.to_string(),
        bytes,
        access_ts: access,
        holders: holders.iter().map(|s| s.to_string()).collect(),
    }
}

fn holders(n: usize) -> Vec<&'static str> {
    ["uid-a", "uid-b", "uid-c", "uid-d", "uid-e"][..n].to_vec()
}

fn state_json(s: &EvictionState) -> Value {
    json!({
        "cid": s.cid,
        "bytes": s.bytes,
        "accessTs": s.access_ts,
        "holders": s.holders,
    })
}

fn main() {
    let mut cases: Vec<Value> = Vec::new();

    // 1. K 语义：设备 1/2/3/5 台 → 1/2/3/3
    cases.push(json!({
        "id": "k_target_semantics",
        "expect": {
            "devices1": k_target(1),
            "devices2": k_target(2),
            "devices3": k_target(3),
            "devices5": k_target(5),
        },
    }));

    // 2. 默认配额
    cases.push(json!({
        "id": "quota_defaults",
        "expect": {
            "pcBytes": DEFAULT_QUOTA_PC_BYTES,
            "mobileBytes": DEFAULT_QUOTA_MOBILE_BYTES,
        },
    }));

    // 3. 副本 =K 永不驱逐（硬规则 1）：任何龄期都无候选
    let h3 = holders(3);
    let states_eq = vec![
        state("aa".repeat(32).as_str(), 1000, 1_000, &h3),
        state("bb".repeat(32).as_str(), 2000, 500, &h3),
    ];
    for (i, my_uid) in h3.iter().enumerate() {
        let plan = plan_eviction(&states_eq, my_uid, 3, 10_000);
        assert!(plan.is_empty(), "rank {i} 在 |H|=K 下不应有候选");
    }
    cases.push(json!({
        "id": "eviction_k_equal_never",
        "input": {
            "k": 3,
            "states": states_eq.iter().map(state_json).collect::<Vec<_>>(),
            "perspectives": h3,
            "neededBytes": 10_000,
        },
        "expect": { "plans": [Vec::<String>::new(), Vec::<String>::new(), Vec::<String>::new()] },
    }));

    // 4. 副本 >K 按龄驱逐：|H|=4, K=3, 本机 uid-d（rank 3 富余）
    let h4 = holders(4);
    let states_age = vec![
        state("cc".repeat(32).as_str(), 100, 3_000, &h4), // 最新
        state("aa".repeat(32).as_str(), 500, 1_000, &h4), // 最老
        state("bb".repeat(32).as_str(), 200, 2_000, &h4), // 中间
    ];
    let plan_partial = plan_eviction(&states_age, "uid-d", 3, 600);
    let plan_full = plan_eviction(&states_age, "uid-d", 3, 10_000);
    assert_eq!(plan_partial, vec!["aa".repeat(32), "bb".repeat(32)], "自检：按龄凑够即停");
    assert_eq!(plan_full.len(), 3, "自检：富余身份下全部可逐");
    cases.push(json!({
        "id": "eviction_over_k_by_age",
        "input": {
            "k": 3,
            "myUid": "uid-d",
            "states": states_age.iter().map(state_json).collect::<Vec<_>>(),
        },
        "expect": {
            "planNeeded600": plan_partial,
            "planNeeded10000": plan_full,
        },
    }));

    // 5. 本机是保留位（rank 0）：即使超配额也无候选（硬规则 2）
    let plan_keeper = plan_eviction(&states_age, "uid-a", 3, 10_000);
    assert!(plan_keeper.is_empty());
    cases.push(json!({
        "id": "eviction_i_am_keeper",
        "input": { "k": 3, "myUid": "uid-a", "states": states_age.iter().map(state_json).collect::<Vec<_>>() },
        "expect": { "plan": plan_keeper },
    }));

    // 6. 全设备并发驱逐收敛：|H|=5, K=3，三视角独立决策
    let h5 = holders(5);
    let blob_cid = "dd".repeat(32);
    let states5 = vec![state(&blob_cid, 4096, 1_234, &h5)];
    let plan_c = plan_eviction(&states5, "uid-c", 3, 4096); // rank 2 < 3
    let plan_d = plan_eviction(&states5, "uid-d", 3, 4096); // rank 3 ≥ 3
    let plan_e = plan_eviction(&states5, "uid-e", 3, 4096); // rank 4 ≥ 3
    assert!(plan_c.is_empty(), "自检：rank 2 保留位不逐");
    assert_eq!(plan_d, vec![blob_cid.clone()]);
    assert_eq!(plan_e, vec![blob_cid.clone()]);
    // d/e 驱逐后幸存持有者 = 前 K 位 = [uid-a, uid-b, uid-c]
    cases.push(json!({
        "id": "eviction_concurrent_converges",
        "input": { "k": 3, "states": states5.iter().map(state_json).collect::<Vec<_>>() },
        "expect": {
            "planRank2": plan_c,
            "planRank3": plan_d,
            "planRank4": plan_e,
            "survivorsAfterEviction": ["uid-a", "uid-b", "uid-c"],
        },
    }));

    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/blob_quota.json");
    std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&cases).unwrap()))
        .expect("write vectors");
    println!("written: {out} ({} cases)", cases.len());
}
