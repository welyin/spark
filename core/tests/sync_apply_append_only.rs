//! `apply_remote_update` 水位线拦截与 append-only 分支测试：MemoryStorage +
//! fake CollectionAdapter；附 meta 原语（meta_key/generate_updated_meta）用例。

mod common;

use serde_json::{Value, json};
use spark_core::evidence::{
    EvidenceOp, build_evidence_payload_hash, get_evidence_entry, get_evidence_head,
    verify_evidence_chain,
};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::{
    ApplyOutcome, ApplyRemoteOptions, apply_remote_update, generate_updated_meta, get_meta,
    meta_key, set_meta,
};

use common::sync::*;

// ---------------------------------------------------------------- 水位线拦截

#[test]
fn watermark_intercepts_purged_update() {
    let (mut s, c) = setup();
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "hi", "seq": 1})),
        &remote_meta(&[("n1", 1)], 50, "n1"),
        ApplyRemoteOptions {
            watermark: Some(&FixedWatermark(100)),
            ..default_options()
        },
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::PurgedByWatermark);
    assert!(s.is_empty(), "被拦截的更新不得写入任何 key");
}

#[test]
fn watermark_passes_newer_update() {
    let (mut s, c) = setup();
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "hi", "seq": 1})),
        &remote_meta(&[("n1", 1)], 200, "n1"),
        ApplyRemoteOptions {
            watermark: Some(&FixedWatermark(100)),
            ..default_options()
        },
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyAccepted);
}

// ---------------------------------------------------------------- append-only

#[test]
fn append_only_accepts_new_doc_with_meta_index_evidence() {
    let (mut s, c) = setup();
    let payload = json!({"text": "hi", "seq": 1});
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&payload),
        &remote_meta(&[("n1", 1)], 1000, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyAccepted);

    // doc
    assert_eq!(
        serde_json::from_str::<Value>(&s.get(&doc_key("m1")).unwrap().unwrap()).unwrap(),
        payload
    );
    // meta：{vv, ts}，不带 nodeId/tombstone
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        "{\"vv\":{\"n1\":1},\"ts\":1000}"
    );
    // 索引
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), Some(String::new()));
    // evidence：默认策略 enableEvidence=true
    let head = get_evidence_head(&s).unwrap().unwrap();
    assert_eq!(head.seq, 1);
    let entry = get_evidence_entry(&s, 1).unwrap().unwrap();
    assert_eq!(entry.op, EvidenceOp::Put);
    assert_eq!(entry.node_id, "n1");
    assert_eq!(
        entry.payload_hash,
        build_evidence_payload_hash(Some(&payload))
    );
    assert!(verify_evidence_chain(&s).unwrap());
}

#[test]
fn append_only_dedup_merges_vv_and_ts() {
    let (mut s, c) = setup();
    let payload = json!({"text": "hi", "seq": 1});
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&payload),
        &remote_meta(&[("n1", 1)], 1000, "n1"),
        default_options(),
    )
    .unwrap();

    // 同载荷、不同 vv/ts：幂等去重并合并
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&payload),
        &remote_meta(&[("n2", 1)], 2000, "n2"),
        default_options(),
    )
    .unwrap();
    assert_eq!(
        outcome,
        ApplyOutcome::AppendOnlyDeduplicated { meta_updated: true }
    );
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        "{\"vv\":{\"n1\":1,\"n2\":1},\"ts\":2000}"
    );
    // doc 未动、无新存证
    assert_eq!(
        serde_json::from_str::<Value>(&s.get(&doc_key("m1")).unwrap().unwrap()).unwrap(),
        payload
    );
    assert_eq!(get_evidence_head(&s).unwrap().unwrap().seq, 1);

    // 完全相同的 meta 再来一次：无变化不写 meta
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&payload),
        &remote_meta(&[("n2", 1)], 2000, "n2"),
        default_options(),
    )
    .unwrap();
    assert_eq!(
        outcome,
        ApplyOutcome::AppendOnlyDeduplicated {
            meta_updated: false
        }
    );
}

#[test]
fn append_only_conflict_keeps_local() {
    let (mut s, c) = setup();
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "local", "seq": 1})),
        &remote_meta(&[("n1", 1)], 1000, "n1"),
        default_options(),
    )
    .unwrap();
    let meta_before = s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap();

    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "conflicting", "seq": 1})),
        &remote_meta(&[("n2", 1)], 2000, "n2"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyConflictKeptLocal);
    assert_eq!(
        serde_json::from_str::<Value>(&s.get(&doc_key("m1")).unwrap().unwrap()).unwrap(),
        json!({"text": "local", "seq": 1})
    );
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        meta_before
    );
}

#[test]
fn append_only_rejects_remote_delete() {
    let (mut s, c) = setup();
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "hi", "seq": 1})),
        &remote_meta(&[("n1", 1)], 1000, "n1"),
        default_options(),
    )
    .unwrap();

    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        None,
        &remote_meta(&[("n1", 2)], 2000, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyDeleteRejected);
    assert!(s.get(&doc_key("m1")).unwrap().is_some(), "文档不得被删除");
}

// ---------------------------------------------------------------- meta 原语

#[test]
fn meta_key_and_generate_updated_meta() {
    assert_eq!(meta_key("chat", "messages", "m1"), "meta:chat:messages:m1");
    let mut s = MemoryStorage::new();
    let meta = generate_updated_meta(&s, "nodeA", "chat", "messages", "m1", 1000).unwrap();
    assert_eq!(meta.vv, vv(&[("nodeA", 1)]));
    assert_eq!(meta.ts, 1000);
    assert_eq!(meta.node_id.as_deref(), Some("nodeA"));

    set_meta(&mut s, "chat", "messages", "m1", &meta).unwrap();
    let meta2 = generate_updated_meta(&s, "nodeA", "chat", "messages", "m1", 2000).unwrap();
    assert_eq!(meta2.vv, vv(&[("nodeA", 2)]));
    assert_eq!(meta2.ts, 2000);

    let got = get_meta(&s, "chat", "messages", "m1").unwrap().unwrap();
    assert_eq!(got, meta);
    // 损坏 meta → None
    s.put(&meta_key("chat", "messages", "bad"), "not json")
        .unwrap();
    assert_eq!(get_meta(&s, "chat", "messages", "bad").unwrap(), None);
}
