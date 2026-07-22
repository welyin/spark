//! golden vectors 验收测试：加载 `../spec/vectors/sync-evidence.json` 逐条断言。
//!
//! 覆盖 sync-evidence.md §5：
//! 1. normalizeObject 17 例（嵌套对象/数组/整数型 key 序/数字格式/中文与转义/undefined/null）
//! 2. payloadHash / metaHash / dataHash / entryHash 固定输入 → 固定 hash
//! 3. 三条目链式构建 → seq/prevHash/hash 链 + 存储键 + verifyChain
//! 4. compareVersionVectors / resolveConflictByLWW / mergeVersionVectors 全分支
//! 5. resolveSchemaDeclaration 全分支（含两类抛错）与默认策略/存储键

use std::collections::BTreeMap;

use serde_json::Value;
use spark_core::evidence::{
    EvidenceEntry, EvidenceOp, NewEvidenceEntry, append_evidence, build_evidence_data_hash,
    build_evidence_entry_hash, build_evidence_meta_hash, build_evidence_payload_hash,
    evidence_key, get_evidence_head, normalize_object, normalize_value, verify_evidence_chain,
};
use spark_core::schema::{
    CollectionSchemaDeclaration, DEFAULT_COLLECTION_POLICY, SyncStrategy, collection_schema_key,
    resolve_schema_declaration,
};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::{
    CompareResult, VersionVector, compare_version_vectors, merge_version_vectors,
    resolve_conflict_by_lww,
};

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/sync-evidence.json");
    let raw = std::fs::read_to_string(path).expect("read sync-evidence vectors");
    serde_json::from_str(&raw).expect("parse sync-evidence vectors")
}

#[test]
fn normalize_object_cases() {
    let v = vectors();
    for case in v["normalizeObject"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let expected = case["expected"].as_str().unwrap();
        let actual = match name {
            // JS undefined：Rust 侧以 None 表达
            "undefined-literal" => normalize_value(None),
            // -0（向量无 input 字段，按 inputExpr 构造）
            "negative-zero" => normalize_object(&Value::from(-0.0_f64)),
            _ => normalize_object(&case["input"]),
        };
        assert_eq!(actual, expected, "normalizeObject case {name}");
    }
}

#[test]
fn payload_hash_cases() {
    let v = vectors();
    for case in v["payloadHash"].as_array().unwrap() {
        let note = case["note"].as_str().unwrap_or("");
        // TS 对 null/undefined 短路返回 null；Rust 侧统一以 None 表达
        let input: Option<&Value> = if case.get("inputKind").and_then(Value::as_str) == Some("undefined")
            || case["input"].is_null()
        {
            None
        } else {
            Some(&case["input"])
        };
        let actual = build_evidence_payload_hash(input);
        let expected = case["payloadHash"].as_str().map(str::to_string);
        assert_eq!(actual, expected, "payloadHash case {note}");
    }
}

#[test]
fn meta_hash_cases() {
    let v = vectors();
    for case in v["metaHash"].as_array().unwrap() {
        let note = case["note"].as_str().unwrap_or("");
        let input = if case["input"].is_null() {
            None
        } else {
            Some(&case["input"])
        };
        let actual = build_evidence_meta_hash(input);
        let expected = case["metaHash"].as_str().map(str::to_string);
        assert_eq!(actual, expected, "metaHash case {note}");
    }
}

#[test]
fn data_hash_cases() {
    let v = vectors();
    for case in v["dataHash"].as_array().unwrap() {
        let op: EvidenceOp = serde_json::from_value(case["op"].clone()).unwrap();
        let actual = build_evidence_data_hash(
            case["domain"].as_str().unwrap(),
            case["collection"].as_str().unwrap(),
            case["id"].as_str().unwrap(),
            op,
            case["payloadHash"].as_str(),
            case["metaHash"].as_str(),
        );
        assert_eq!(actual, case["dataHash"].as_str().unwrap());
    }
}

#[test]
fn entry_hash_cases() {
    let v = vectors();
    for case in v["entryHash"].as_array().unwrap() {
        let entry: EvidenceEntry = serde_json::from_value(case["entry"].clone()).unwrap();
        assert_eq!(
            build_evidence_entry_hash(&entry),
            case["hash"].as_str().unwrap()
        );
    }
}

#[test]
fn evidence_chain_cases() {
    let v = vectors();
    let chain = &v["evidenceChain"];
    let mut storage = MemoryStorage::new();

    for (i, e) in chain["entries"].as_array().unwrap().iter().enumerate() {
        let op: EvidenceOp = serde_json::from_value(e["op"].clone()).unwrap();
        let entry = NewEvidenceEntry {
            domain: e["domain"].as_str().unwrap().to_string(),
            collection: e["collection"].as_str().unwrap().to_string(),
            id: e["id"].as_str().unwrap().to_string(),
            op,
            data_hash: e["dataHash"].as_str().unwrap().to_string(),
            payload_hash: e["payloadHash"].as_str().map(str::to_string),
            meta_hash: e["metaHash"].as_str().map(str::to_string),
            timestamp: e["timestamp"].as_i64().unwrap(),
            node_id: e["nodeId"].as_str().unwrap().to_string(),
        };
        let appended = append_evidence(&mut storage, entry).unwrap();
        assert_eq!(appended.seq, e["seq"].as_u64().unwrap(), "entry {i} seq");
        assert_eq!(
            appended.prev_hash.as_deref(),
            e["prevHash"].as_str(),
            "entry {i} prevHash"
        );
        assert_eq!(appended.hash, e["hash"].as_str().unwrap(), "entry {i} hash");

        // 存储键：doc:evidence:proof:{seq 左补零至 12 位}
        let seq = appended.seq;
        assert_eq!(evidence_key(seq), chain["storageKeys"]["proofKeys"][i].as_str().unwrap());
        assert!(storage.get(&evidence_key(seq)).unwrap().is_some());
    }

    // 头指针
    let head = get_evidence_head(&storage).unwrap().unwrap();
    assert_eq!(head.seq, chain["head"]["seq"].as_u64().unwrap());
    assert_eq!(head.hash, chain["head"]["hash"].as_str().unwrap());
    assert!(storage.get("doc:evidence:head").unwrap().is_some());

    // 校验链
    assert_eq!(
        chain["verifyChainResult"].as_bool().unwrap(),
        verify_evidence_chain(&storage).unwrap()
    );
}

#[test]
fn compare_version_vector_cases() {
    let v = vectors();
    for case in v["compareVersionVectors"].as_array().unwrap() {
        let branch = case["branch"].as_str().unwrap();
        let local: Option<VersionVector> = serde_json::from_value(case["local"].clone()).unwrap();
        let remote: Option<VersionVector> = serde_json::from_value(case["remote"].clone()).unwrap();
        let actual = compare_version_vectors(local.as_ref(), remote.as_ref());
        assert_eq!(
            actual.as_str(),
            case["result"].as_str().unwrap(),
            "compareVersionVectors case {branch}"
        );
    }
}

#[test]
fn resolve_conflict_by_lww_cases() {
    let v = vectors();
    for case in v["resolveConflictByLWW"].as_array().unwrap() {
        let branch = case["branch"].as_str().unwrap();
        let local_ts = case["localTs"].as_i64();
        let remote_ts = case["remoteTs"].as_i64();
        let actual = resolve_conflict_by_lww(local_ts, remote_ts);
        assert_eq!(
            actual.as_str(),
            case["result"].as_str().unwrap(),
            "resolveConflictByLWW case {branch}"
        );
    }
}

#[test]
fn merge_version_vector_cases() {
    let v = vectors();
    for case in v["mergeVersionVectors"].as_array().unwrap() {
        let local: Option<VersionVector> = serde_json::from_value(case["local"].clone()).unwrap();
        let remote: Option<VersionVector> = serde_json::from_value(case["remote"].clone()).unwrap();
        let merged = merge_version_vectors(local.as_ref(), remote.as_ref());
        let expected: BTreeMap<String, i64> = serde_json::from_value(case["merged"].clone()).unwrap();
        assert_eq!(merged, expected);
    }
}

#[test]
fn resolve_schema_declaration_cases() {
    let v = vectors();
    for case in v["resolveSchemaDeclaration"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let declaration: Option<CollectionSchemaDeclaration> =
            serde_json::from_value(case["declaration"].clone()).unwrap();
        let result = resolve_schema_declaration(declaration.as_ref());
        if let Some(expected_error) = case["error"].as_str() {
            let err = result.unwrap_err();
            assert_eq!(err.to_string(), expected_error, "error message case {name}");
        } else {
            let policy = result.unwrap();
            let expected = &case["result"];
            assert_eq!(
                policy.sync_strategy.as_str(),
                expected["syncStrategy"].as_str().unwrap(),
                "syncStrategy case {name}"
            );
            assert_eq!(
                policy.governance,
                expected["governance"].as_bool().unwrap(),
                "governance case {name}"
            );
            assert_eq!(
                policy.enable_evidence,
                expected["enableEvidence"].as_bool().unwrap(),
                "enableEvidence case {name}"
            );
        }
    }
}

#[test]
fn schema_defaults_cases() {
    let v = vectors();
    let defaults = &v["schemaDefaults"]["DEFAULT_COLLECTION_POLICY"];
    assert_eq!(DEFAULT_COLLECTION_POLICY.sync_strategy, SyncStrategy::AppendOnly);
    assert_eq!(
        DEFAULT_COLLECTION_POLICY.sync_strategy.as_str(),
        defaults["syncStrategy"].as_str().unwrap()
    );
    assert_eq!(
        DEFAULT_COLLECTION_POLICY.governance,
        defaults["governance"].as_bool().unwrap()
    );
    assert_eq!(
        DEFAULT_COLLECTION_POLICY.enable_evidence,
        defaults["enableEvidence"].as_bool().unwrap()
    );

    let key_example = &v["schemaDefaults"]["collectionSchemaKeyExample"];
    assert_eq!(
        collection_schema_key(
            key_example["domain"].as_str().unwrap(),
            key_example["collection"].as_str().unwrap()
        ),
        key_example["key"].as_str().unwrap()
    );
}

#[test]
fn compare_result_strings_roundtrip() {
    assert_eq!(CompareResult::Local.as_str(), "local");
    assert_eq!(CompareResult::Remote.as_str(), "remote");
    assert_eq!(CompareResult::Concurrent.as_str(), "concurrent");
    assert_eq!(CompareResult::Equal.as_str(), "equal");
}
