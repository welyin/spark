//! kernel 文档与存证门面集成测试：doc 写入路径（meta/evidence/索引）、
//! append-only 拒绝、重启持久化与存证链查询。

mod common;

use serde_json::{Value, json};

use spark_core::collection::{CollectionConfig, QueryOptions};

use common::*;

// ---------------------------------------------------------------------------
// doc 写入路径：meta/evidence/索引、append-only 拒绝、重启持久化
// ---------------------------------------------------------------------------

#[test]
fn doc_write_meta_evidence_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    // 登录即在线：p2p 运行期 nodeId 取 libp2p peerId；本用例断言本地 "local-node" 口径，先停
    kernel.stop_p2p().unwrap();

    // 声明 lww + 存证集合
    kernel
        .declare_collection("chat", "messages", lww_evidence_declaration())
        .unwrap();

    // 写文档
    kernel
        .doc_put(
            "chat",
            "messages",
            "id1",
            json!({"from": "alice", "text": "hello"}),
            from_indexed_config(),
        )
        .unwrap();
    let doc = kernel.doc_get("chat", "messages", "id1").unwrap().unwrap();
    assert_eq!(doc, json!({"from": "alice", "text": "hello"}));

    // lww 覆盖 + 索引查询
    kernel
        .doc_put(
            "chat",
            "messages",
            "id1",
            json!({"from": "bob", "text": "hi"}),
            from_indexed_config(),
        )
        .unwrap();
    let result = kernel
        .doc_query(
            "chat",
            "messages",
            from_indexed_config(),
            QueryOptions {
                index_name: Some("from".to_string()),
                index_value: Some(json!("bob")),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].id, "id1");

    // 导出全库：逐字节检查 doc/meta key 与存证链
    let export_path = dir.path().join("dump.json");
    let written = kernel.export_dump(&export_path).unwrap();
    assert!(written.entries > 0 && written.bytes > 0);
    let dump: Value =
        serde_json::from_str(&std::fs::read_to_string(&export_path).unwrap()).unwrap();
    let entries: std::collections::HashMap<_, _> = dump["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["key"].as_str().unwrap().to_string(),
                e["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        entries["doc:chat:messages:id1"],
        r#"{"from":"bob","text":"hi"}"#
    );
    let meta_raw = &entries["meta:chat:messages:id1"];
    let meta: Value = serde_json::from_str(meta_raw).unwrap();
    let node_id = meta["nodeId"].as_str().unwrap();
    assert_eq!(meta["vv"][node_id], 2, "本节点两次写入计数");
    assert_ne!(
        node_id, "local-node",
        "已初始化身份：nodeId 应派生自持久化 p2p 身份（pdsync 节点唯一性）"
    );
    assert!(meta_raw.starts_with(r#"{"vv":"#), "meta 键序 vv 在前");
    assert!(
        entries.keys().any(|k| k.starts_with("doc:evidence:proof:")),
        "存证条目已落库"
    );
    assert!(entries.contains_key("doc:evidence:head"));

    // append-only 默认策略拒绝覆盖
    kernel
        .doc_put(
            "chat",
            "audit",
            "a1",
            json!({"v": 1}),
            CollectionConfig::default(),
        )
        .unwrap();
    let err = kernel
        .doc_put(
            "chat",
            "audit",
            "a1",
            json!({"v": 2}),
            CollectionConfig::default(),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Collection \"audit\" is append-only: document \"a1\" already exists and cannot be overwritten"
    );

    // 删除：墓碑 + 返回值语义
    assert!(
        kernel
            .doc_delete("chat", "messages", "id1", from_indexed_config())
            .unwrap()
    );
    assert!(kernel.doc_get("chat", "messages", "id1").unwrap().is_none());
    assert!(
        !kernel
            .doc_delete("chat", "messages", "id1", from_indexed_config())
            .unwrap()
    );
    let entries_after: Value = {
        let p = dir.path().join("dump2.json");
        kernel.export_dump(&p).unwrap();
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let tombstone = entries_after["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "meta:chat:messages:id1")
        .expect("tombstone meta");
    let tombstone: Value = serde_json::from_str(tombstone["value"].as_str().unwrap()).unwrap();
    assert_eq!(tombstone["tombstone"], true);
    assert!(tombstone.get("nodeId").is_none(), "墓碑 meta 不带 nodeId");

    kernel.shutdown().unwrap();

    // 重启：数据仍在（audit 集合的 a1 未被删除）
    let mut kernel = fresh_kernel(dir.path());
    kernel.unlock(PASSWORD, None).unwrap();
    let doc = kernel.doc_get("chat", "audit", "a1").unwrap().unwrap();
    assert_eq!(doc, json!({"v": 1}));
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 存证查询门面：head/verify/entry、put/delete 链完整性
// ---------------------------------------------------------------------------

#[test]
fn evidence_facade_queries() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 空链
    assert_eq!(kernel.evidence_head_hash().unwrap(), None);
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 0);
    assert!(kernel.evidence_entry(1).unwrap().is_none());

    // 写入两篇（enable_evidence 集合）
    kernel
        .declare_collection("plugin:app", "notes", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put(
            "plugin:app",
            "notes",
            "n1",
            json!({"v": 1}),
            CollectionConfig::default(),
        )
        .unwrap();
    kernel
        .doc_put(
            "plugin:app",
            "notes",
            "n2",
            json!({"v": 2}),
            CollectionConfig::default(),
        )
        .unwrap();

    let head = kernel.evidence_head_hash().unwrap();
    assert!(head.as_deref().is_some_and(|h| h.len() == 64));
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 2);

    let first = kernel.evidence_entry(1).unwrap().unwrap();
    assert_eq!(first.domain, "plugin:app");
    assert_eq!(first.collection, "notes");
    assert_eq!(first.id, "n1");
    assert_eq!(first.op.as_str(), "put");
    let second = kernel.evidence_entry(2).unwrap().unwrap();
    assert_eq!(second.prev_hash.as_deref(), Some(first.hash.as_str()));
    assert_eq!(
        head.as_deref(),
        Some(second.hash.as_str()),
        "链头 = 末条 hash"
    );

    // 删除 → 第三条 op=delete，链仍完整
    kernel
        .doc_delete("plugin:app", "notes", "n1", CollectionConfig::default())
        .unwrap();
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 3);
    assert_eq!(
        kernel.evidence_entry(3).unwrap().unwrap().op.as_str(),
        "delete"
    );

    kernel.shutdown().unwrap();
}
