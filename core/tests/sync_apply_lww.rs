//! `apply_remote_update` lww 分支与 schema hint 兜底测试：remote put/delete
//! 落地（含 evidence 与索引 diff）、local/equal 不动、concurrent 双向裁决、
//! schema hint 只兜底不持久化。

mod common;

use serde_json::{Value, json};
use spark_core::evidence::{EvidenceOp, get_evidence_entry, get_evidence_head, verify_evidence_chain};
use spark_core::schema::{
    CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema, get_collection_schema,
};
use spark_core::storage::StorageBackend;
use spark_core::sync::{
    ApplyOutcome, ApplyRemoteOptions, DocMeta, apply_remote_update, meta_key, set_meta,
};

use common::sync::*;

// ---------------------------------------------------------------- lww

#[test]
fn lww_remote_put_applies_with_index_diff_and_evidence() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);

    // 首次落地（本地无 meta：cmp == remote）
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "v1", "seq": 1})),
        &remote_meta(&[("n1", 1)], 100, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwRemoteApplied);
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), Some(String::new()));

    // 更新版本 + 索引字段变化：旧索引删除、新索引写入
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "v2", "seq": 2})),
        &remote_meta(&[("n1", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwRemoteApplied);
    assert_eq!(
        s.get(&index_key("1", "m1")).unwrap(),
        None,
        "旧索引必须删除"
    );
    assert_eq!(s.get(&index_key("2", "m1")).unwrap(), Some(String::new()));
    assert_eq!(
        serde_json::from_str::<Value>(&s.get(&doc_key("m1")).unwrap().unwrap()).unwrap(),
        json!({"text": "v2", "seq": 2})
    );
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        "{\"vv\":{\"n1\":2},\"ts\":200}"
    );

    // 两次落地各写一条 evidence（op=put）
    assert_eq!(get_evidence_head(&s).unwrap().unwrap().seq, 2);
    for seq in 1..=2 {
        assert_eq!(
            get_evidence_entry(&s, seq).unwrap().unwrap().op,
            EvidenceOp::Put
        );
    }
    assert!(verify_evidence_chain(&s).unwrap());
}

#[test]
fn lww_remote_delete_writes_tombstone_and_evidence() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "v1", "seq": 1})),
        &remote_meta(&[("n1", 1)], 100, "n1"),
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
        &remote_meta(&[("n1", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwRemoteApplied);
    assert_eq!(s.get(&doc_key("m1")).unwrap(), None, "文档必须删除");
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), None, "索引必须删除");
    // tombstone meta
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        "{\"vv\":{\"n1\":2},\"ts\":200,\"tombstone\":true}"
    );
    // evidence op=delete，payloadHash=null
    let entry = get_evidence_entry(&s, 2).unwrap().unwrap();
    assert_eq!(entry.op, EvidenceOp::Delete);
    assert_eq!(entry.payload_hash, None);
    assert!(entry.meta_hash.is_some());
    assert!(verify_evidence_chain(&s).unwrap());
}

#[test]
fn lww_local_and_equal_keep_local() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "v1", "seq": 1})),
        &remote_meta(&[("n1", 5)], 100, "n1"),
        default_options(),
    )
    .unwrap();
    let doc_before = s.get(&doc_key("m1")).unwrap().unwrap();
    let meta_before = s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap();

    // cmp == local
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "stale", "seq": 1})),
        &remote_meta(&[("n1", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwLocalKept);

    // cmp == equal
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "same-vv", "seq": 1})),
        &remote_meta(&[("n1", 5)], 300, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwEqualNoop);

    assert_eq!(s.get(&doc_key("m1")).unwrap().unwrap(), doc_before);
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        meta_before
    );
}

#[test]
fn lww_concurrent_resolved_by_timestamp() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    // 本地已有 doc + meta（vv {a:2,b:1} ts 100）
    s.put(&doc_key("m1"), "{\"text\":\"local\",\"seq\":1}")
        .unwrap();
    s.put(&index_key("1", "m1"), "").unwrap();
    set_meta(
        &mut s,
        DOMAIN,
        COLLECTION,
        "m1",
        &DocMeta {
            vv: vv(&[("a", 2), ("b", 1)]),
            ts: 100,
            ..DocMeta::default()
        },
    )
    .unwrap();

    // concurrent 且远端 ts 更大 → 远端落地（put）
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "remote", "seq": 9})),
        &remote_meta(&[("a", 1), ("b", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwConcurrentRemoteApplied);
    assert_eq!(
        serde_json::from_str::<Value>(&s.get(&doc_key("m1")).unwrap().unwrap()).unwrap(),
        json!({"text": "remote", "seq": 9})
    );
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), None);
    assert_eq!(s.get(&index_key("9", "m1")).unwrap(), Some(String::new()));
    // concurrent-remote 分支也写 evidence
    let entry = get_evidence_entry(&s, 1).unwrap().unwrap();
    assert_eq!(entry.op, EvidenceOp::Put);

    // concurrent 且本地 ts 更大 → 不动
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m2",
        Some(&json!({"text": "remote2", "seq": 1})),
        &remote_meta(&[("b", 2)], 50, "n1"),
        default_options(),
    )
    .unwrap();
    // m2 本地无 meta → cmp == remote，不是 concurrent；先造本地 meta 再测
    assert_eq!(outcome, ApplyOutcome::LwwRemoteApplied);

    set_meta(
        &mut s,
        DOMAIN,
        COLLECTION,
        "m3",
        &DocMeta {
            vv: vv(&[("a", 2), ("b", 1)]),
            ts: 300,
            ..DocMeta::default()
        },
    )
    .unwrap();
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m3",
        Some(&json!({"text": "remote3", "seq": 1})),
        &remote_meta(&[("a", 1), ("b", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwConcurrentLocalKept);
    assert_eq!(
        s.get(&doc_key("m3")).unwrap(),
        None,
        "本地胜出：不得写入远端 doc"
    );
}

#[test]
fn lww_concurrent_delete_remote_wins() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    s.put(&doc_key("m1"), "{\"text\":\"local\",\"seq\":1}")
        .unwrap();
    s.put(&index_key("1", "m1"), "").unwrap();
    set_meta(
        &mut s,
        DOMAIN,
        COLLECTION,
        "m1",
        &DocMeta {
            vv: vv(&[("a", 2), ("b", 1)]),
            ts: 100,
            ..DocMeta::default()
        },
    )
    .unwrap();

    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        None,
        &remote_meta(&[("a", 1), ("b", 2)], 200, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwConcurrentRemoteApplied);
    assert_eq!(s.get(&doc_key("m1")).unwrap(), None);
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), None);
    assert_eq!(
        s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(),
        "{\"vv\":{\"a\":1,\"b\":2},\"ts\":200,\"tombstone\":true}"
    );
}

#[test]
fn lww_without_evidence_writes_no_evidence() {
    let (mut s, c) = setup();
    declare_lww(&mut s, false);
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "v1", "seq": 1})),
        &remote_meta(&[("n1", 1)], 100, "n1"),
        default_options(),
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::LwwRemoteApplied);
    assert!(s.get(&doc_key("m1")).unwrap().is_some());
    assert_eq!(
        get_evidence_head(&s).unwrap(),
        None,
        "enableEvidence=false 不写存证"
    );
}

// ---------------------------------------------------------------- schema hint

#[test]
fn schema_hint_applies_transiently_but_never_persists() {
    let (mut s, c) = setup();
    let hint = CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence: false,
    };
    // 本地未声明：hint 兜底生效（lww 行为：cmp==local 时不动）
    set_meta(
        &mut s,
        DOMAIN,
        COLLECTION,
        "m1",
        &DocMeta {
            vv: vv(&[("n1", 5)]),
            ts: 100,
            ..DocMeta::default()
        },
    )
    .unwrap();
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "stale"})),
        &remote_meta(&[("n1", 2)], 200, "n1"),
        ApplyRemoteOptions {
            schema: Some(hint.clone()),
            ..default_options()
        },
    )
    .unwrap();
    assert_eq!(
        outcome,
        ApplyOutcome::LwwLocalKept,
        "hint 必须作为 lww 兜底生效"
    );
    assert_eq!(
        get_collection_schema(&s, DOMAIN, COLLECTION).unwrap(),
        None,
        "hint 永不写入注册表"
    );

    // 非法 hint：sanitize 后丢弃，退回默认 append-only
    let invalid: CollectionSchemaDeclaration =
        serde_json::from_value(json!({"syncStrategy": "merge"})).unwrap();
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m2",
        Some(&json!({"text": "x"})),
        &remote_meta(&[("n1", 1)], 100, "n1"),
        ApplyRemoteOptions {
            schema: Some(invalid),
            ..default_options()
        },
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyAccepted);
    assert_eq!(get_collection_schema(&s, DOMAIN, COLLECTION).unwrap(), None);
}

#[test]
fn local_declaration_wins_over_schema_hint() {
    let (mut s, c) = setup();
    // 本地声明 append-only（默认带存证）
    declare_collection_schema(
        &mut s,
        DOMAIN,
        COLLECTION,
        &CollectionSchemaDeclaration::append_only(),
        NOW,
    )
    .unwrap();
    apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "local"})),
        &remote_meta(&[("n1", 1)], 100, "n1"),
        default_options(),
    )
    .unwrap();

    // 即使 hint 声称 lww，本地 append-only 仍优先：冲突载荷被拒绝
    let hint = CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence: false,
    };
    let outcome = apply_remote_update(
        &mut s,
        &c,
        DOMAIN,
        COLLECTION,
        "m1",
        Some(&json!({"text": "overwrite-attempt"})),
        &remote_meta(&[("n1", 2)], 200, "n1"),
        ApplyRemoteOptions {
            schema: Some(hint),
            ..default_options()
        },
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::AppendOnlyConflictKeptLocal);
}
