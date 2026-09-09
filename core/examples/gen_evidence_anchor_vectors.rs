//! 生成 `code/spec/vectors/evidence-anchor.json`（阶段四F 存证锚定/导出包
//! golden vectors，sync-evidence §10 四组清单）。
//!
//! 全部输入固定（密钥/ts/链条目/锚集），输出逐字节确定——消费测试
//! `core/tests/evidence_anchor_vectors.rs` 按本文件验收。
//! 运行：`cargo run --example gen_evidence_anchor_vectors -- <repo_root>/code/spec/vectors/evidence-anchor.json`

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::json;
use spark_core::evidence::anchor::{
    AnchorRecord, anchor_leaf, anchor_root, anchor_sign_payload, inclusion_proof, sign_anchor,
};
use spark_core::evidence::export::{
    ExportScope, build_export_package, export_sign_payload, verify_export_package,
};
use spark_core::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence, normalize_object};
use spark_core::storage::{MemoryStorage, StorageBackend};

const ORG: &str = "org_0000000000000001";
const TS_BASE: i64 = 1_720_000_000_000;

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn key_b64(byte: u8) -> String {
    B64.encode([byte; 32])
}

/// 固定锚记录（nodeId/seq/hash/ts 逐组显式给定）。
fn fixed_anchor(byte: u8, node: &str, seq: u64, hash_byte: u8, ts: i64) -> AnchorRecord {
    sign_anchor(
        &key(byte),
        ORG,
        node,
        seq,
        &hex::encode([hash_byte; 32]),
        ts,
    )
}

/// 固定链（3 条目，plugin:vote/ballots，模拟治理集合）。
fn fixed_chain() -> MemoryStorage {
    let mut s = MemoryStorage::new();
    for i in 1..=3u64 {
        append_evidence(
            &mut s,
            NewEvidenceEntry::from_parts(
                "plugin:vote",
                "ballots",
                format!("ballot-{i}"),
                EvidenceOp::Put,
                Some(&json!({"choice": "approve", "voter": format!("member-{i}")})),
                None,
                TS_BASE + i64::try_from(i).unwrap() * 1000,
                "12D3KooWNodeA",
            ),
        )
        .expect("append evidence");
    }
    s
}

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../spec/vectors/evidence-anchor.json".to_string());

    // ── 组 1：锚记录（canonical 载荷逐字节 + sig 固定值）──
    let anchor_rec = fixed_anchor(0x11, "12D3KooWNodeA", 42, 0xAB, TS_BASE);
    let group_anchor = json!({
        "desc": "锚记录：固定 orgId/nodeId/root 密钥/headSeq/headHash/ts → canonical 签名载荷串逐字节 + sig 固定值；篡改任一字段验签必败（消费侧断言）",
        "input": {
            "rootKey": key_b64(0x11),
            "orgId": ORG,
            "nodeId": "12D3KooWNodeA",
            "headSeq": 42u64,
            "headHash": hex::encode([0xAB; 32]),
            "ts": TS_BASE,
        },
        "expect": {
            "payload": anchor_sign_payload(&anchor_rec),
            "record": serde_json::to_value(&anchor_rec).unwrap(),
            "sig": anchor_rec.sig,
        },
    });

    // ── 组 2：默克尔根（单叶/偶数叶/奇数叶三形态 → 固定 root）──
    let a = fixed_anchor(0x11, "12D3KooWNodeA", 42, 0xAB, TS_BASE);
    let b = fixed_anchor(0x22, "12D3KooWNodeB", 17, 0xCD, TS_BASE + 1000);
    let c = fixed_anchor(0x33, "12D3KooWNodeC", 88, 0xEF, TS_BASE + 2000);
    let anchor_value = |r: &AnchorRecord| serde_json::to_value(r).unwrap();
    let group_merkle = json!({
        "desc": "默克尔根：同一锚记录集合 → 固定 anchorRoot；覆盖单叶/偶数叶/奇数叶（末叶复制）三形态",
        "input": {
            "anchors": {
                "A": anchor_value(&a),
                "B": anchor_value(&b),
                "C": anchor_value(&c),
            },
        },
        "expect": {
            "leafA": hex::encode(anchor_leaf(&a)),
            "rootSingle": anchor_root(std::slice::from_ref(&a)),
            "rootEven": anchor_root(&[a.clone(), b.clone()]),
            "rootOdd": anchor_root(&[a.clone(), b.clone(), c.clone()]),
        },
    });

    // ── 组 3：inclusion proof（固定三叶树 → 每叶 proof 逐字节）──
    let tree = vec![a.clone(), b.clone(), c.clone()];
    let root_odd = anchor_root(&tree).unwrap();
    let group_proof = json!({
        "desc": "inclusion proof：固定树 → 每叶 proof（自叶向根兄弟 hex 序列）逐字节；验证通过 + 篡改 sibling/换叶索引失败（消费侧断言）",
        "input": { "anchors": ["A", "B", "C"] },
        "expect": {
            "root": root_odd,
            "proofA": inclusion_proof(&tree, "12D3KooWNodeA"),
            "proofB": inclusion_proof(&tree, "12D3KooWNodeB"),
            "proofC": inclusion_proof(&tree, "12D3KooWNodeC"),
        },
    });

    // ── 组 4：导出包（固定链 + 固定锚集 + 固定导出者密钥/ts → canonical
    // 包字节 + exporter.sig 固定值）──
    let mut s = fixed_chain();
    for r in [&a, &b] {
        s.put(
            &spark_core::evidence::anchor_key(&r.org_id, &r.node_id),
            &serde_json::to_string(r).unwrap(),
        )
        .unwrap();
    }
    let pkg = build_export_package(
        &s,
        ExportScope {
            domain: Some("plugin:vote".to_string()),
            collection: Some("ballots".to_string()),
            org_id: Some(ORG.to_string()),
        },
        &key(0xEE),
        TS_BASE + 5000,
        None,
    )
    .expect("build package");
    let canonical_full = normalize_object(&serde_json::to_value(&pkg).unwrap());
    // 生成器自检：五步核验全过
    let report = verify_export_package(&serde_json::to_string(&pkg).unwrap());
    assert!(report.ok(), "self-check failed: {:?}", report.failures);
    let group_export = json!({
        "desc": "导出包：固定链（3 条目）+ 固定锚集（A/B）+ 固定导出者密钥/ts → canonical 包字节 + exporter.sig 固定值；逐字段篡改与断链必败（消费侧断言）",
        "input": {
            "exporterKey": key_b64(0xEE),
            "exporterTs": TS_BASE + 5000,
            "scope": { "domain": "plugin:vote", "collection": "ballots", "orgId": ORG },
        },
        "expect": {
            "package": serde_json::to_value(&pkg).unwrap(),
            "canonicalPackage": canonical_full,
            "signPayload": export_sign_payload(&pkg),
            "exporterSig": pkg.exporter.sig,
        },
    });

    let doc = json!({
        "_comment": "sync-evidence §10 存证锚定/导出包 golden vectors（阶段四F）。生成器：core/examples/gen_evidence_anchor_vectors.rs（Rust 自产，含五步核验自检）；消费：core/tests/evidence_anchor_vectors.rs。",
        "anchor": group_anchor,
        "merkle": group_merkle,
        "proof": group_proof,
        "export": group_export,
    });
    let text = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(&out_path, format!("{text}\n")).expect("write vectors");
    println!("written {out_path}");
}
