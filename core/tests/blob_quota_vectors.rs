//! A2 配额与驱逐 golden vectors 消费测试：`spec/vectors/blob_quota.json`
//! 的断言必须与 `core/src/sync/blob`（quota/evict）真实实现一致。
//!
//! 规格：`wiki/protocol/p2p/personal-data-sync.md` §15；生成器
//! `examples/gen_blob_quota_vectors.rs`。

use serde_json::Value;
use spark_core::sync::blob::{
    DEFAULT_QUOTA_MOBILE_BYTES, DEFAULT_QUOTA_PC_BYTES, EvictionState, k_target, plan_eviction,
};

fn load_vectors() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/blob_quota.json");
    let raw = std::fs::read_to_string(path).expect("read blob quota vectors");
    serde_json::from_str(&raw).expect("parse blob quota vectors")
}

fn case<'a>(vectors: &'a [Value], id: &str) -> &'a Value {
    vectors
        .iter()
        .find(|t| t["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("vector case {id} missing"))
}

fn parse_states(v: &Value) -> Vec<EvictionState> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| EvictionState {
            cid: s["cid"].as_str().unwrap().to_string(),
            bytes: s["bytes"].as_u64().unwrap(),
            access_ts: s["accessTs"].as_i64().unwrap(),
            holders: s["holders"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u.as_str().unwrap().to_string())
                .collect(),
        })
        .collect()
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn k_target_semantics() {
    let vectors = load_vectors();
    let expect = &case(&vectors, "k_target_semantics")["expect"];
    for (key, devices) in [("devices1", 1), ("devices2", 2), ("devices3", 3), ("devices5", 5)] {
        assert_eq!(
            k_target(devices),
            expect[key].as_u64().unwrap() as usize,
            "设备 {devices} 台的 K"
        );
    }
}

#[test]
fn quota_defaults() {
    let vectors = load_vectors();
    let expect = &case(&vectors, "quota_defaults")["expect"];
    assert_eq!(DEFAULT_QUOTA_PC_BYTES, expect["pcBytes"].as_u64().unwrap());
    assert_eq!(
        DEFAULT_QUOTA_MOBILE_BYTES,
        expect["mobileBytes"].as_u64().unwrap()
    );
}

#[test]
fn eviction_k_equal_never() {
    let vectors = load_vectors();
    let t = case(&vectors, "eviction_k_equal_never");
    let k = t["input"]["k"].as_u64().unwrap() as usize;
    let needed = t["input"]["neededBytes"].as_u64().unwrap();
    let states = parse_states(&t["input"]["states"]);
    let perspectives = str_list(&t["input"]["perspectives"]);
    let expected: Vec<Vec<String>> = t["expect"]["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(str_list)
        .collect();
    // 副本 =K：任何设备视角、任何龄期都无候选（硬规则 1）
    for (i, my_uid) in perspectives.iter().enumerate() {
        assert_eq!(plan_eviction(&states, my_uid, k, needed), expected[i], "rank {i}");
    }
}

#[test]
fn eviction_over_k_by_age() {
    let vectors = load_vectors();
    let t = case(&vectors, "eviction_over_k_by_age");
    let k = t["input"]["k"].as_u64().unwrap() as usize;
    let my_uid = t["input"]["myUid"].as_str().unwrap();
    let states = parse_states(&t["input"]["states"]);
    assert_eq!(
        plan_eviction(&states, my_uid, k, 600),
        str_list(&t["expect"]["planNeeded600"]),
        "按龄凑够即停"
    );
    assert_eq!(
        plan_eviction(&states, my_uid, k, 10_000),
        str_list(&t["expect"]["planNeeded10000"]),
        "富余身份下全部可逐"
    );
}

#[test]
fn eviction_i_am_keeper() {
    let vectors = load_vectors();
    let t = case(&vectors, "eviction_i_am_keeper");
    let k = t["input"]["k"].as_u64().unwrap() as usize;
    let my_uid = t["input"]["myUid"].as_str().unwrap();
    let states = parse_states(&t["input"]["states"]);
    assert_eq!(
        plan_eviction(&states, my_uid, k, 10_000),
        str_list(&t["expect"]["plan"]),
        "保留位持有者即使超配额也无候选（硬规则 2）"
    );
}

#[test]
fn eviction_concurrent_converges() {
    let vectors = load_vectors();
    let t = case(&vectors, "eviction_concurrent_converges");
    let k = t["input"]["k"].as_u64().unwrap() as usize;
    let states = parse_states(&t["input"]["states"]);
    let holders = states[0].holders.clone();
    // 三视角独立决策：rank 2 不逐、rank 3/4 逐
    let plan2 = plan_eviction(&states, &holders[2], k, 4096);
    let plan3 = plan_eviction(&states, &holders[3], k, 4096);
    let plan4 = plan_eviction(&states, &holders[4], k, 4096);
    assert_eq!(plan2, str_list(&t["expect"]["planRank2"]));
    assert_eq!(plan3, str_list(&t["expect"]["planRank3"]));
    assert_eq!(plan4, str_list(&t["expect"]["planRank4"]));
    // 并发执行后幸存持有者 = 前 K 位（精确收敛到 K，无时序协调）
    let evicted_by: Vec<&str> = [
        (holders[3].as_str(), &plan3),
        (holders[4].as_str(), &plan4),
    ]
    .into_iter()
    .filter(|(_, p)| !p.is_empty())
    .map(|(u, _)| u)
    .collect();
    let survivors: Vec<String> = holders
        .iter()
        .filter(|u| !evicted_by.contains(&u.as_str()))
        .cloned()
        .collect();
    assert_eq!(survivors, str_list(&t["expect"]["survivorsAfterEviction"]));
    assert_eq!(survivors.len(), k, "收敛到 K 副本");
}
