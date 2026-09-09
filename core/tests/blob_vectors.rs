//! A1 blob 层 golden vectors 消费测试：`spec/vectors/blob.json` 的断言必须与
//! `core/src/sync/blob` 真实实现字节级一致。
//!
//! 规格：`wiki/protocol/p2p/personal-data-sync.md` §14；生成器
//! `examples/gen_blob_vectors.rs`。

use serde_json::Value;
use spark_core::storage::MemoryStorage;
use spark_core::sync::blob::{
    self, BlobManifest, CHUNK_THRESHOLD_BYTES, FetchTarget, PresenceRecord, bitmap_decode,
    bitmap_encode, chunk_replica_counts, full_replica_count,
};

fn load_vectors() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/blob.json");
    let raw = std::fs::read_to_string(path).expect("read blob vectors");
    serde_json::from_str(&raw).expect("parse blob vectors")
}

fn case<'a>(vectors: &'a [Value], id: &str) -> &'a Value {
    vectors
        .iter()
        .find(|t| t["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("vector case {id} missing"))
}

/// 确定性内容规则（与生成器同一条，向量不内嵌大内容）。
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn assert_manifest_case(t: &Value) {
    let size = t["input"]["size"].as_u64().unwrap() as usize;
    let expect = &t["expect"];
    let data = pattern(size);
    let m = blob::build_manifest(&data);
    assert_eq!(m.cid, expect["cid"].as_str().unwrap(), "cid exact");
    let expected_cids: Vec<String> = expect["chunkCids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(m.chunk_cids, expected_cids, "chunkCids exact");
    assert_eq!(
        m.to_json(),
        expect["manifestJson"].as_str().unwrap(),
        "manifest 规范 JSON 逐字节"
    );
    // 规范 JSON 可被结构校验接受且往返一致
    assert_eq!(BlobManifest::from_json(&m.to_json()).unwrap(), m);
}

#[test]
fn manifest_small_blob() {
    let vectors = load_vectors();
    let t = case(&vectors, "manifest_small_blob");
    assert_manifest_case(t);
    // 单块恒等式：chunkCid == cid
    let expect = &t["expect"];
    assert_eq!(
        expect["chunkCids"][0].as_str().unwrap(),
        expect["cid"].as_str().unwrap()
    );
}

#[test]
fn chunking_empty_blob() {
    let vectors = load_vectors();
    let t = case(&vectors, "chunking_empty_blob");
    assert_manifest_case(t);
    assert_eq!(
        t["expect"]["cid"].as_str().unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "cid = sha256(空)"
    );
    assert_eq!(t["expect"]["chunkCids"].as_array().unwrap().len(), 0);
}

#[test]
fn chunking_threshold_boundaries() {
    let vectors = load_vectors();
    let minus_1 = case(&vectors, "chunking_threshold_minus_1");
    assert_manifest_case(minus_1);
    assert_eq!(minus_1["expect"]["chunkCids"].as_array().unwrap().len(), 1, "阈值-1 单块");

    let exact = case(&vectors, "chunking_threshold_exact");
    assert_manifest_case(exact);
    assert_eq!(exact["expect"]["chunkCids"].as_array().unwrap().len(), 1, "恰阈值单块");

    let plus_1 = case(&vectors, "chunking_threshold_plus_1");
    assert_manifest_case(plus_1);
    let cids = plus_1["expect"]["chunkCids"].as_array().unwrap();
    assert_eq!(cids.len(), 5, "阈值+1 → 4×256KiB + 1B");
    // 尾块 = 内容的最后 1 字节
    let data = pattern(CHUNK_THRESHOLD_BYTES + 1);
    assert_eq!(
        cids[4].as_str().unwrap(),
        blob::sha256_hex(&data[CHUNK_THRESHOLD_BYTES..]),
        "尾块 chunkCid 独立复算"
    );
}

#[test]
fn presence_record_bytes() {
    let vectors = load_vectors();
    let t = case(&vectors, "presence_record_bytes");
    let expect = &t["expect"];
    let record_json = expect["recordJson"].as_str().unwrap();
    // 线形往返逐字节
    let record = PresenceRecord::from_json(record_json).expect("parse presence record");
    assert_eq!(record.to_json(), record_json, "presence 规范 JSON 逐字节");
    // 位图编解码用例
    for case in expect["bitmapCases"].as_array().unwrap() {
        let held: Vec<bool> = case["held"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_bool().unwrap())
            .collect();
        let encoded = case["encoded"].as_str().unwrap();
        assert_eq!(bitmap_encode(&held), encoded, "位图编码 exact");
        let decoded = bitmap_decode(encoded).expect("decode");
        // 最短长度编码：短缺位视为未持有
        for (i, h) in held.iter().enumerate() {
            assert_eq!(
                decoded.get(i).copied().unwrap_or(false),
                *h,
                "位图解码往返 bit {i}"
            );
        }
    }
    // 9 块持有 {0,3,8} 的规范编码
    assert_eq!(
        bitmap_encode(&[true, false, false, true, false, false, false, false, true]),
        "CQE="
    );
}

#[test]
fn presence_deterministic_count() {
    let vectors = load_vectors();
    let t = case(&vectors, "presence_deterministic_count");
    let chunk_count = t["input"]["chunkCount"].as_u64().unwrap() as usize;
    let records: Vec<PresenceRecord> = t["input"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| PresenceRecord::from_json(r["json"].as_str().unwrap()).unwrap())
        .collect();
    let expected_full = t["expect"]["fullReplicaCount"].as_u64().unwrap() as usize;
    let expected_per: Vec<u32> = t["expect"]["chunkReplicaCounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    // 两种插入顺序复算结果一致（确定性）
    let reversed: Vec<PresenceRecord> = records.iter().rev().cloned().collect();
    for r in [&records, &reversed] {
        assert_eq!(full_replica_count(r, chunk_count), expected_full, "完整副本数");
        assert_eq!(chunk_replica_counts(r, chunk_count), expected_per, "逐块副本数");
    }
}

#[test]
fn fetch_envelope_bodies() {
    let vectors = load_vectors();
    let t = case(&vectors, "fetch_envelope_bodies");
    let expect = &t["expect"];
    let chunk_cid = "11".repeat(32);
    let cid = "22".repeat(32);

    // 请求体逐字节
    let fetch_chunk = blob::build_fetch_body(&FetchTarget::Chunk {
        chunk_cid: chunk_cid.clone(),
        offset: 0,
    });
    assert_eq!(
        serde_json::to_string(&fetch_chunk).unwrap(),
        expect["fetchChunk"].as_str().unwrap()
    );
    let fetch_chunk_offset = blob::build_fetch_body(&FetchTarget::Chunk {
        chunk_cid: chunk_cid.clone(),
        offset: 245760,
    });
    assert_eq!(
        serde_json::to_string(&fetch_chunk_offset).unwrap(),
        expect["fetchChunkOffset"].as_str().unwrap()
    );
    let fetch_manifest = blob::build_fetch_body(&FetchTarget::Manifest { cid: cid.clone() });
    assert_eq!(
        serde_json::to_string(&fetch_manifest).unwrap(),
        expect["fetchManifest"].as_str().unwrap()
    );
    // 解析往返
    assert_eq!(
        blob::parse_fetch_body(&fetch_chunk),
        Some(FetchTarget::Chunk {
            chunk_cid: chunk_cid.clone(),
            offset: 0
        })
    );
    assert_eq!(
        blob::parse_fetch_body(&fetch_manifest),
        Some(FetchTarget::Manifest { cid: cid.clone() })
    );

    // 响应体：同输入经 serve_fetch 复现逐字节
    let mut storage = MemoryStorage::new();
    let saved = blob::save_blob(&mut storage, "node-a", "uid-a", &pattern(32), 1_720_000_000_000)
        .expect("save blob");
    let cases: [(FetchTarget, &str); 4] = [
        (
            FetchTarget::Chunk {
                chunk_cid: saved.chunk_cids[0].clone(),
                offset: 0,
            },
            "respChunkWhole",
        ),
        (
            FetchTarget::Chunk {
                chunk_cid: chunk_cid.clone(),
                offset: 0,
            },
            "respChunkMissing",
        ),
        (
            FetchTarget::Manifest {
                cid: saved.cid.clone(),
            },
            "respManifest",
        ),
        (FetchTarget::Manifest { cid: cid.clone() }, "respManifestMissing"),
    ];
    for (target, key) in cases {
        let body = blob::serve_fetch(&storage, &target).expect("serve");
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            expect[key].as_str().unwrap(),
            "{key} 逐字节"
        );
        // 响应可被请求方解析
        assert!(blob::parse_chunk_body(&body).is_some(), "{key} 可解析");
    }
}
