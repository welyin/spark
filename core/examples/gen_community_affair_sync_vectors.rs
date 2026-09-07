//! community-affairs C4（事务复制面）golden vectors 生成器（规格：
//! wiki/protocol/community/affair-sync.md §9、affair.md §12 `affairSync` 组）。
//!
//! 运行：`cargo run --example gen_community_affair_sync_vectors`，回填
//! `../spec/vectors/community.json` 的 `affairSync` 组，其余组原样保留。
//!
//! 确定性：固定 affairId/vv/heads/meta（无密钥参与——复制面信封不签名，
//! 安全边界在逐条 verify_op/verify_genesis 链，affair-sync §4）。

use serde_json::{Value, json};
use spark_core::sync::affairsync::{
    AffairsyncRecord, build_affairsync_data_batch, build_affairsync_hello, build_affairsync_need,
    record_key_in_scope,
};
use spark_core::sync::meta::{DocMeta, VersionVector};

fn fixed_vv() -> VersionVector {
    VersionVector::from([("node-a".to_string(), 3), ("node-b".to_string(), 1)])
}

fn fixed_heads() -> Vec<String> {
    vec!["cd".repeat(32), "ef".repeat(32)]
}

fn fixed_record_meta() -> DocMeta {
    DocMeta {
        vv: VersionVector::from([("node-a".to_string(), 2)]),
        ts: 1_720_000_000_000,
        node_id: Some("node-a".to_string()),
        tombstone: None,
    }
}

fn gen_affair_sync(affair_id: &str) -> Value {
    let hello = build_affairsync_hello(affair_id, &fixed_vv(), &fixed_heads(), "pc");
    let need = build_affairsync_need(affair_id, &fixed_vv());
    let record_key = spark_core::affair::affair_op_key(affair_id, &"cd".repeat(32));
    let data = build_affairsync_data_batch(
        affair_id,
        &[spark_core::sync::affairsync::AffairsyncRecord {
            key: record_key.clone(),
            value: json!({
                "opV": 1, "affairId": affair_id, "prevOpHash": affair_id,
                "opType": "content", "payload": { "kind": "post", "text": "向量" },
                "actor": { "kind": "person", "identity": "11".repeat(32),
                           "publicKey": "AAAA" },
                "declaredAt": 1_720_000_000_000i64,
                "sig": "BBBB"
            }),
            meta: fixed_record_meta(),
        }],
        0,
        1,
    );
    // key 白名单用例（affair-sync §3.3 红线）
    let whitelist = [
        (
            "rec-in-scope",
            spark_core::affair::affair_record_key(affair_id),
            true,
        ),
        ("op-in-scope", record_key.clone(), true),
        ("org-key-rejected", "org:meta:some-org".to_string(), false),
        (
            "other-affair-rec-rejected",
            spark_core::affair::affair_record_key(&"ee".repeat(32)),
            false,
        ),
        (
            "head-key-rejected",
            spark_core::affair::affair_head_key(affair_id),
            false,
        ),
        (
            "follow-key-rejected",
            spark_core::affair::affair_follow_key(affair_id),
            false,
        ),
    ]
    .into_iter()
    .map(|(name, key, expect)| json!({ "name": name, "key": key, "inScope": expect }))
    .collect::<Vec<_>>();
    json!({
        "desc": "事务复制面三信封 body 逐字节（affair-sync §3，dm kind 包装不在组内）+ key 白名单拒收用例。信封不签名：安全边界 = 逐条 verify_op/verify_genesis 链（§4）",
        "expect": {
            "affairId": affair_id,
            "hello": { "body": hello.to_string(), "vv": fixed_vv(), "heads": fixed_heads() },
            "need": { "body": need.to_string(), "knownVv": fixed_vv() },
            "data": {
                "body": data.to_string(),
                "recordKey": record_key,
                "recordMeta": fixed_record_meta(),
            },
            "whitelist": whitelist,
        }
    })
}

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/community.json"
    );
    let raw = std::fs::read_to_string(path).expect("read community.json");
    let mut vectors: Value = serde_json::from_str(&raw).expect("parse community.json");

    // 固定 affairId：与 C1 向量同口径的确定性常量（无需真实创世——信封层只钉线形）
    let affair_id = "ab".repeat(32);

    let out = vectors.as_object_mut().expect("top-level object");
    out.insert("affairSync".to_string(), gen_affair_sync(&affair_id));

    vectors["_comment"] = json!(
        "community-affairs golden vectors（wiki/protocol/community/ 协议）。C0 组由 code/spec/gen-community-vectors.mjs（Node 参考实现）自产；C1 组（staticCheck/resolutionReplay/metaBasisVerify/metaArbitrate）由 code/core/examples/gen_community_affair_vectors.rs 自产并逐字节复核 C0 affair 组；C2 组由 code/core/examples/gen_credential_vectors.rs 自产；C4 组（affairSync，事务复制面信封线形）由 code/core/examples/gen_community_affair_sync_vectors.rs 自产。消费：core/tests/community_affair_vectors.rs（C1）、community_credential_vectors.rs（C2）、community_affair_sync_vectors.rs（C4）。"
    );

    std::fs::write(path, serde_json::to_string_pretty(&vectors).unwrap() + "\n")
        .expect("write community.json");
    println!("OK: community.json updated (C4 group: affairSync).");
}
