//! `code/spec/vectors/evidence-resolution.json` 消费测试（affair.md §6.3 /
//! affair-model §六验收）：条目逐字节（含签名包内嵌）、conclusionHash 计算、
//! 个人决议 sigSet=null、链上条目逐字节与链校验。
//! 向量由 `examples/gen_evidence_resolution_vectors.rs` 生成（改算法须重生成）。

use serde_json::Value;
use spark_core::affair::{
    RESOLUTION_ENTRY_COLLECTION, compute_op_hash, conclusion_hash, resolution_entry_payload,
};
use spark_core::evidence::{
    EvidenceOp, NewEvidenceEntry, append_evidence, build_evidence_payload_hash, normalize_object,
    verify_evidence_chain,
};
use spark_core::storage::MemoryStorage;

fn vectors() -> Value {
    let raw = include_str!("../../spec/vectors/evidence-resolution.json");
    serde_json::from_str(raw).expect("parse evidence-resolution vectors")
}

/// 组 1：组织决议条目逐字节——subject=opHash、conclusionHash、payload
/// canonical 与 payloadHash 全锁；sigSet 原样内嵌；篡改 payload →
/// conclusionHash 必变。
#[test]
fn resolution_entry_byte_exact_with_sigset() {
    let v = &vectors()["entry"];
    let affair_id = v["input"]["affairId"].as_str().unwrap();
    let op = &v["input"]["resolutionOp"];
    let effective_ts = v["input"]["effectiveTs"].as_i64().unwrap();
    let expect = &v["expect"];

    // subject = 决议 id（opHash 复算）
    let op_hash = compute_op_hash(op).unwrap();
    assert_eq!(op_hash, expect["subject"].as_str().unwrap(), "subject 漂移");
    // conclusionHash = 决议 payload canonical 哈希
    let hash = conclusion_hash(&op["payload"]);
    assert_eq!(hash, expect["conclusionHash"].as_str().unwrap(), "conclusionHash 漂移");
    // 条目逐字节（含 sigSet 原样内嵌）
    let payload = resolution_entry_payload(
        affair_id,
        &op_hash,
        &op["payload"],
        op["actor"].get("orgSig").cloned().as_ref(),
        effective_ts,
    );
    assert_eq!(&payload, &expect["payload"], "条目线形漂移");
    assert_eq!(payload["sigSet"], op["actor"]["orgSig"], "sigSet 未原样内嵌");
    assert_eq!(
        normalize_object(&payload),
        expect["canonical"].as_str().unwrap(),
        "canonical 漂移"
    );
    assert_eq!(
        build_evidence_payload_hash(Some(&payload)).unwrap(),
        expect["payloadHash"].as_str().unwrap(),
        "payloadHash 漂移"
    );
    // 篡改 payload → conclusionHash 必变（内容绑定）
    let mut tampered = op["payload"].clone();
    tampered["result"] = Value::String("rejected".to_string());
    assert_ne!(conclusion_hash(&tampered), hash);
}

/// 组 2：个人决议条目——sigSet 落 null；conclusionHash 与组织决议一致
/// （结论哈希只绑定决议 payload，不绑定签名面）。
#[test]
fn resolution_entry_person_sigset_null() {
    let v = &vectors()["entryPerson"];
    let affair_id = v["input"]["affairId"].as_str().unwrap();
    let op = &v["input"]["resolutionOp"];
    let effective_ts = v["input"]["effectiveTs"].as_i64().unwrap();
    let expect = &v["expect"];

    let op_hash = compute_op_hash(op).unwrap();
    assert_eq!(op_hash, expect["subject"].as_str().unwrap());
    let payload = resolution_entry_payload(affair_id, &op_hash, &op["payload"], None, effective_ts);
    assert_eq!(&payload, &expect["payload"], "个人决议条目线形漂移");
    assert_eq!(payload["sigSet"], Value::Null);
    assert_eq!(
        payload["conclusionHash"].as_str().unwrap(),
        conclusion_hash(&op["payload"])
    );
}

/// 组 3：链上条目逐字节——空链追加后条目字段全锁，链校验通过；同内容
/// 重复追加产生新 seq（幂等由生产路径按 id 查重在先，本组只锁字节）。
#[test]
fn chain_entry_byte_exact_and_verifies() {
    let v = &vectors()["chainEntry"];
    let org_id = v["input"]["orgId"].as_str().unwrap();
    let timestamp = v["input"]["timestamp"].as_i64().unwrap();
    let node_id = v["input"]["nodeId"].as_str().unwrap();
    let expect_entry = &v["expect"]["entry"];
    let entry_group = &vectors()["entry"];
    let op_hash = entry_group["expect"]["subject"].as_str().unwrap();
    let payload = &entry_group["expect"]["payload"];

    let mut storage = MemoryStorage::new();
    let entry = append_evidence(
        &mut storage,
        NewEvidenceEntry::from_parts(
            org_id,
            RESOLUTION_ENTRY_COLLECTION,
            op_hash,
            EvidenceOp::Put,
            Some(payload),
            None,
            timestamp,
            node_id,
        ),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        *expect_entry,
        "链上条目逐字节漂移"
    );
    assert!(verify_evidence_chain(&storage).unwrap());
}
