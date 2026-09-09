//! A1 blob 层 golden vectors 生成器：`cargo run -p spark-core --example gen_blob_vectors`
//! 输出 `code/spec/vectors/blob.json`（消费测试 `tests/blob_vectors.rs`）。
//!
//! 字节级权威：`wiki/protocol/p2p/personal-data-sync.md` §14。
//! 确定性内容规则（向量不内嵌大内容）：`byte[i] = (i % 251) as u8`
//! （与 `plugindata::blob` 测试同口径）。所有取值由 spark_core 公开 API 推导，
//! 重复运行字节级一致。

use serde_json::{Value, json};
use spark_core::storage::{MemoryStorage, StorageBackend as _};
use spark_core::sync::blob::{
    self, BlobManifest, CHUNK_THRESHOLD_BYTES, FetchResponse, FetchTarget, PresenceRecord,
    bitmap_encode, chunk_replica_counts, full_replica_count,
};

/// 确定性内容生成规则（与消费测试/模块单测同一条）。
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// manifest 类用例的 expect：cid + chunkCids + 规范 JSON 逐字节。
fn manifest_expect(m: &BlobManifest) -> Value {
    json!({
        "cid": m.cid,
        "chunkCids": m.chunk_cids,
        "manifestJson": m.to_json(),
    })
}

fn presence_record(cid: &str, uid: &str, held: &[bool]) -> PresenceRecord {
    PresenceRecord {
        v: 1,
        cid: cid.to_string(),
        device_uid: uid.to_string(),
        chunks: bitmap_encode(held),
    }
}

fn main() {
    let mut cases: Vec<Value> = Vec::new();

    // 1. 小 blob（1000B）：单块，chunkCid == cid，manifest 逐字节
    let small = blob::build_manifest(&pattern(1000));
    cases.push(json!({
        "id": "manifest_small_blob",
        "input": { "contentRule": "byte[i] = (i % 251) as u8", "size": 1000 },
        "expect": manifest_expect(&small),
    }));

    // 2. 空 blob：cid = sha256("")，0 块
    let empty = blob::build_manifest(&[]);
    cases.push(json!({
        "id": "chunking_empty_blob",
        "input": { "contentRule": "byte[i] = (i % 251) as u8", "size": 0 },
        "expect": manifest_expect(&empty),
    }));

    // 3–5. 切分边界：阈值 -1 / 恰阈值 / 阈值 +1
    for (id, size) in [
        ("chunking_threshold_minus_1", CHUNK_THRESHOLD_BYTES - 1),
        ("chunking_threshold_exact", CHUNK_THRESHOLD_BYTES),
        ("chunking_threshold_plus_1", CHUNK_THRESHOLD_BYTES + 1),
    ] {
        let m = blob::build_manifest(&pattern(size));
        cases.push(json!({
            "id": id,
            "input": { "contentRule": "byte[i] = (i % 251) as u8", "size": size },
            "expect": manifest_expect(&m),
        }));
    }

    // 6. presence 记录线形逐字节 + 位图编解码规则
    let cid9 = "ab".repeat(32);
    let rec = presence_record(&cid9, &"cd".repeat(16), &[true, false, false, true, false, false, false, false, true]);
    cases.push(json!({
        "id": "presence_record_bytes",
        "expect": {
            "recordJson": rec.to_json(),
            "bitmapCases": [
                { "held": [true, false, false], "encoded": bitmap_encode(&[true, false, false]) },
                { "held": [false, false, false], "encoded": bitmap_encode(&[false, false, false]) },
                { "held": [true, false, false, true, false, false, false, false, true], "encoded": rec.chunks },
            ],
        },
    }));

    // 7. 确定性副本计数：5 块 blob，A/B 完整、C 部分（{0,2,4}）；两种插入顺序同结果
    let cid5 = "ef".repeat(32);
    let ra = presence_record(&cid5, "uid-a", &[true; 5]);
    let rb = presence_record(&cid5, "uid-b", &[true; 5]);
    let rc = presence_record(&cid5, "uid-c", &[true, false, true, false, true]);
    let order1 = vec![ra.clone(), rb.clone(), rc.clone()];
    let order2 = vec![rc.clone(), ra.clone(), rb.clone()];
    let full1 = full_replica_count(&order1, 5);
    let per1 = chunk_replica_counts(&order1, 5);
    assert_eq!(full1, full_replica_count(&order2, 5), "计数与插入顺序无关");
    assert_eq!(per1, chunk_replica_counts(&order2, 5), "逐块计数与插入顺序无关");
    cases.push(json!({
        "id": "presence_deterministic_count",
        "input": {
            "chunkCount": 5,
            "records": [
                { "json": ra.to_json(), "deviceUid": "uid-a", "held": [true, true, true, true, true] },
                { "json": rb.to_json(), "deviceUid": "uid-b", "held": [true, true, true, true, true] },
                { "json": rc.to_json(), "deviceUid": "uid-c", "held": [true, false, true, false, true] },
            ],
        },
        "expect": { "fullReplicaCount": full1, "chunkReplicaCounts": per1 },
    }));

    // 8. blob-fetch / blob-chunk 信封体线形逐字节
    let chunk_cid = "11".repeat(32);
    let cid = "22".repeat(32);
    let fetch_chunk = blob::build_fetch_body(&FetchTarget::Chunk {
        chunk_cid: chunk_cid.clone(),
        offset: 0,
    });
    let fetch_chunk_offset = blob::build_fetch_body(&FetchTarget::Chunk {
        chunk_cid: chunk_cid.clone(),
        offset: 245760,
    });
    let fetch_manifest = blob::build_fetch_body(&FetchTarget::Manifest { cid: cid.clone() });
    // 响应体经 serve_fetch 真实产出：32B 小块整块应答、缺块 missing、manifest 应答
    let mut storage = MemoryStorage::new();
    let small_data = pattern(32);
    let saved = blob::save_blob(&mut storage, "node-a", "uid-a", &small_data, 1_720_000_000_000)
        .expect("save blob");
    let resp_whole = blob::serve_fetch(
        &storage,
        &FetchTarget::Chunk {
            chunk_cid: saved.chunk_cids[0].clone(),
            offset: 0,
        },
    )
    .expect("serve chunk");
    let resp_missing_chunk = blob::serve_fetch(
        &storage,
        &FetchTarget::Chunk {
            chunk_cid: chunk_cid.clone(),
            offset: 0,
        },
    )
    .expect("serve missing");
    let resp_manifest = blob::serve_fetch(&storage, &FetchTarget::Manifest { cid: saved.cid.clone() })
        .expect("serve manifest");
    let resp_missing_manifest =
        blob::serve_fetch(&storage, &FetchTarget::Manifest { cid: cid.clone() }).expect("serve missing manifest");
    // 自检：响应可解析且语义正确
    assert!(matches!(
        blob::parse_chunk_body(&resp_whole),
        Some(FetchResponse::Chunk { .. })
    ));
    assert_eq!(
        blob::parse_chunk_body(&resp_missing_chunk),
        Some(FetchResponse::Missing)
    );
    cases.push(json!({
        "id": "fetch_envelope_bodies",
        "input": { "contentRule": "byte[i] = (i % 251) as u8", "wholeChunkSize": 32 },
        "expect": {
            "fetchChunk": serde_json::to_string(&fetch_chunk).unwrap(),
            "fetchChunkOffset": serde_json::to_string(&fetch_chunk_offset).unwrap(),
            "fetchManifest": serde_json::to_string(&fetch_manifest).unwrap(),
            "respChunkWhole": serde_json::to_string(&resp_whole).unwrap(),
            "respChunkMissing": serde_json::to_string(&resp_missing_chunk).unwrap(),
            "respManifest": serde_json::to_string(&resp_manifest).unwrap(),
            "respManifestMissing": serde_json::to_string(&resp_missing_manifest).unwrap(),
        },
    }));

    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/blob.json");
    std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&cases).unwrap()))
        .expect("write vectors");
    println!("written: {out} ({} cases)", cases.len());
}
