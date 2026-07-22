//! `apply_remote_update` 全分支单测：MemoryStorage + fake CollectionAdapter。
//!
//! 覆盖：水位线拦截、append-only 接受/幂等合并/冲突拒绝/删除拒绝、
//! lww remote put/delete 落地（含 evidence 与索引 diff）、local/equal 不动、
//! concurrent 双向裁决、schema hint 只兜底不持久化。

use std::collections::BTreeMap;

use serde_json::{Value, json};
use spark_core::evidence::{
    EvidenceOp, build_evidence_payload_hash, get_evidence_entry, get_evidence_head,
    js_number_to_string, verify_evidence_chain,
};
use spark_core::schema::{
    CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema, get_collection_schema,
};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::{
    ApplyOutcome, ApplyRemoteOptions, CollectionAdapter, DocMeta, PurgeWatermark, RemoteMeta,
    SyncResult, VersionVector, apply_remote_update, generate_updated_meta, get_meta, meta_key,
    set_meta,
};

const DOMAIN: &str = "chat";
const COLLECTION: &str = "messages";
const NOW: i64 = 1_700_000_000_000;

/// fake 集合：对齐 TS DocumentCollection 的 docKey/indexKey/buildIndexMap。
struct FakeCollection {
    domain: String,
    collection: String,
    indexed_fields: Vec<String>,
}

impl FakeCollection {
    fn new(indexed_fields: &[&str]) -> Self {
        Self {
            domain: DOMAIN.to_string(),
            collection: COLLECTION.to_string(),
            indexed_fields: indexed_fields.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl CollectionAdapter for FakeCollection {
    fn get(&self, storage: &dyn StorageBackend, id: &str) -> SyncResult<Option<Value>> {
        let Some(raw) = storage.get(&self.doc_key(id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    fn doc_key(&self, id: &str) -> String {
        format!("doc:{}:{}:{id}", self.domain, self.collection)
    }

    fn index_key(&self, index_name: &str, index_value: &str, id: &str) -> String {
        format!(
            "idx:{}:{}:{index_name}:{}:{id}",
            self.domain,
            self.collection,
            spark_core::schema::encode_uri_component(index_value)
        )
    }

    fn build_index_map(&self, doc: Option<&Value>) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let Some(doc) = doc else { return map };
        for field in &self.indexed_fields {
            let Some(value) = doc.get(field) else { continue };
            // 对齐 TS String(fieldValue)（null/undefined 跳过）
            let s = match value {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => n.as_f64().map(js_number_to_string),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            };
            if let Some(s) = s {
                map.insert(field.clone(), s);
            }
        }
        map
    }
}

/// 固定水位线：`remote_ts < watermark` 即拦截。
struct FixedWatermark(i64);

impl PurgeWatermark for FixedWatermark {
    fn is_purged_by_watermark(
        &self,
        _storage: &mut dyn StorageBackend,
        _domain: &str,
        _collection: &str,
        remote_ts: i64,
    ) -> SyncResult<bool> {
        Ok(remote_ts < self.0)
    }
}

fn vv(pairs: &[(&str, i64)]) -> VersionVector {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn remote_meta(pairs: &[(&str, i64)], ts: i64, node_id: &str) -> RemoteMeta {
    RemoteMeta {
        vv: vv(pairs),
        ts,
        node_id: Some(node_id.to_string()),
    }
}

fn default_options() -> ApplyRemoteOptions<'static> {
    ApplyRemoteOptions {
        now_ms: NOW,
        ..Default::default()
    }
}

fn setup() -> (MemoryStorage, FakeCollection) {
    (MemoryStorage::new(), FakeCollection::new(&["seq"]))
}

fn doc_key(id: &str) -> String {
    format!("doc:{DOMAIN}:{COLLECTION}:{id}")
}

fn index_key(value: &str, id: &str) -> String {
    format!("idx:{DOMAIN}:{COLLECTION}:seq:{value}:{id}")
}

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
    assert_eq!(outcome, ApplyOutcome::AppendOnlyDeduplicated { meta_updated: true });
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
    assert_eq!(outcome, ApplyOutcome::AppendOnlyDeduplicated { meta_updated: false });
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
    assert_eq!(s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(), meta_before);
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

// ---------------------------------------------------------------- lww

fn declare_lww(s: &mut MemoryStorage, enable_evidence: bool) {
    let decl = CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence,
    };
    declare_collection_schema(s, DOMAIN, COLLECTION, &decl, NOW).unwrap();
}

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
    assert_eq!(s.get(&index_key("1", "m1")).unwrap(), None, "旧索引必须删除");
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
        assert_eq!(get_evidence_entry(&s, seq).unwrap().unwrap().op, EvidenceOp::Put);
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
    assert_eq!(s.get(&meta_key(DOMAIN, COLLECTION, "m1")).unwrap().unwrap(), meta_before);
}

#[test]
fn lww_concurrent_resolved_by_timestamp() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    // 本地已有 doc + meta（vv {a:2,b:1} ts 100）
    s.put(&doc_key("m1"), "{\"text\":\"local\",\"seq\":1}").unwrap();
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
    assert_eq!(s.get(&doc_key("m3")).unwrap(), None, "本地胜出：不得写入远端 doc");
}

#[test]
fn lww_concurrent_delete_remote_wins() {
    let (mut s, c) = setup();
    declare_lww(&mut s, true);
    s.put(&doc_key("m1"), "{\"text\":\"local\",\"seq\":1}").unwrap();
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
    assert_eq!(get_evidence_head(&s).unwrap(), None, "enableEvidence=false 不写存证");
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
    assert_eq!(outcome, ApplyOutcome::LwwLocalKept, "hint 必须作为 lww 兜底生效");
    assert_eq!(
        get_collection_schema(&s, DOMAIN, COLLECTION).unwrap(),
        None,
        "hint 永不写入注册表"
    );

    // 非法 hint：sanitize 后丢弃，退回默认 append-only
    let invalid: CollectionSchemaDeclaration = serde_json::from_value(json!({"syncStrategy": "merge"})).unwrap();
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
    s.put(&meta_key("chat", "messages", "bad"), "not json").unwrap();
    assert_eq!(get_meta(&s, "chat", "messages", "bad").unwrap(), None);
}
