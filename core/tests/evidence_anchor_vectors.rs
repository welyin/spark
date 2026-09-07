//! evidence-anchor golden vectors 验收测试（阶段四F）：加载
//! `../spec/vectors/evidence-anchor.json` 逐组断言（sync-evidence §10 四组
//! 清单：锚记录 / 默克尔根三形态 / inclusion proof / 导出包 + 篡改与断链
//! 必败）。
//!
//! 向量由 `core/examples/gen_evidence_anchor_vectors.rs` 生成（固定密钥/
//! ts/链/锚集，输出逐字节确定）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use spark_core::evidence::anchor::{
    AnchorRecord, anchor_key, anchor_root, anchor_sign_payload, inclusion_proof, sign_anchor,
    verify_anchor, verify_inclusion,
};
use spark_core::evidence::export::{
    EvidenceExportPackage, ExportScope, build_export_package, export_sign_payload,
    verify_export_package,
};
use spark_core::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence, normalize_object};
use spark_core::storage::{MemoryStorage, StorageBackend};

const ORG: &str = "org_0000000000000001";
const TS_BASE: i64 = 1_720_000_000_000;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spec/vectors/evidence-anchor.json"
    );
    let raw = std::fs::read_to_string(path).expect("read evidence-anchor vectors");
    serde_json::from_str(&raw).expect("parse evidence-anchor vectors")
}

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn key_from_b64(v: &Value) -> SigningKey {
    let raw = B64.decode(v.as_str().unwrap()).unwrap();
    SigningKey::from_bytes(&<[u8; 32]>::try_from(raw.as_slice()).unwrap())
}

/// 与生成器同输入重建固定锚记录。
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

fn tree() -> Vec<AnchorRecord> {
    vec![
        fixed_anchor(0x11, "12D3KooWNodeA", 42, 0xAB, TS_BASE),
        fixed_anchor(0x22, "12D3KooWNodeB", 17, 0xCD, TS_BASE + 1000),
        fixed_anchor(0x33, "12D3KooWNodeC", 88, 0xEF, TS_BASE + 2000),
    ]
}

/// 与生成器同输入重建固定链。
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
                Some(&serde_json::json!({"choice": "approve", "voter": format!("member-{i}")})),
                None,
                TS_BASE + i64::try_from(i).unwrap() * 1000,
                "12D3KooWNodeA",
            ),
        )
        .expect("append evidence");
    }
    s
}

/// §10-1 锚记录：canonical 载荷串逐字节 + sig 固定值 + 篡改验签必败。
#[test]
fn anchor_record_byte_exact_and_tamper_fails() {
    let v = vectors();
    let g = &v["anchor"];
    let input = &g["input"];
    let signing = key_from_b64(&input["rootKey"]);
    let rec = sign_anchor(
        &signing,
        input["orgId"].as_str().unwrap(),
        input["nodeId"].as_str().unwrap(),
        input["headSeq"].as_u64().unwrap(),
        input["headHash"].as_str().unwrap(),
        input["ts"].as_i64().unwrap(),
    );
    assert_eq!(
        anchor_sign_payload(&rec),
        g["expect"]["payload"].as_str().unwrap(),
        "canonical 签名载荷逐字节"
    );
    assert_eq!(rec.sig, g["expect"]["sig"].as_str().unwrap(), "sig 固定值");
    assert_eq!(
        serde_json::to_value(&rec).unwrap(),
        g["expect"]["record"],
        "锚记录全文字段逐字节"
    );
    assert!(verify_anchor(&rec), "锚验签过");
    // 篡改任一字段验签必败
    for tampered in [
        AnchorRecord {
            anchor_v: 2,
            ..rec.clone()
        },
        AnchorRecord {
            head_seq: rec.head_seq + 1,
            ..rec.clone()
        },
        AnchorRecord {
            head_hash: "00".repeat(32),
            ..rec.clone()
        },
        AnchorRecord {
            node_id: "otherNode".to_string(),
            ..rec.clone()
        },
        AnchorRecord {
            org_id: "org_ffffffffffffffff".to_string(),
            ..rec.clone()
        },
        AnchorRecord {
            ts: rec.ts + 1,
            ..rec.clone()
        },
    ] {
        assert!(!verify_anchor(&tampered), "篡改字段验签必败: {tampered:?}");
    }
}

/// §10-2 默克尔根：单叶/偶数叶/奇数叶三形态固定 root。
#[test]
fn merkle_root_three_shapes() {
    let v = vectors();
    let g = &v["merkle"];
    let [a, b, c] = tree().try_into().unwrap();
    assert_eq!(
        spark_core::evidence::anchor_leaf(&a),
        {
            let raw = hex::decode(g["expect"]["leafA"].as_str().unwrap()).unwrap();
            <[u8; 32]>::try_from(raw.as_slice()).unwrap()
        },
        "叶哈希逐字节"
    );
    assert_eq!(
        anchor_root(std::slice::from_ref(&a)).as_deref(),
        g["expect"]["rootSingle"].as_str(),
        "单叶 root = 该叶"
    );
    assert_eq!(
        anchor_root(&[a.clone(), b.clone()]).as_deref(),
        g["expect"]["rootEven"].as_str(),
        "偶数叶 root"
    );
    assert_eq!(
        anchor_root(&[a, b, c]).as_deref(),
        g["expect"]["rootOdd"].as_str(),
        "奇数叶（末叶复制）root"
    );
}

/// §10-3 inclusion proof：每叶 proof 逐字节 + 验证通过 + 篡改/换叶必败。
#[test]
fn inclusion_proof_byte_exact_and_tamper_fails() {
    let v = vectors();
    let g = &v["proof"];
    let tree = tree();
    let root = g["expect"]["root"].as_str().unwrap();
    for (node, key) in [
        ("12D3KooWNodeA", "proofA"),
        ("12D3KooWNodeB", "proofB"),
        ("12D3KooWNodeC", "proofC"),
    ] {
        let expect: Vec<String> = serde_json::from_value(g["expect"][key].clone()).unwrap();
        let proof = inclusion_proof(&tree, node).unwrap();
        assert_eq!(proof, expect, "{node} proof 逐字节");
        assert!(
            verify_inclusion(&tree, node, &proof, root),
            "{node} proof 验证通过"
        );
    }
    // 篡改 sibling 必败
    let proof_c = inclusion_proof(&tree, "12D3KooWNodeC").unwrap();
    let mut bad = proof_c.clone();
    bad[0] = "00".repeat(32);
    assert!(!verify_inclusion(&tree, "12D3KooWNodeC", &bad, root));
    // 换叶索引：nodeA 的证明验 nodeC 必败
    let proof_a = inclusion_proof(&tree, "12D3KooWNodeA").unwrap();
    assert!(!verify_inclusion(&tree, "12D3KooWNodeC", &proof_a, root));
    // 证明长度截断必败
    assert!(!verify_inclusion(
        &tree,
        "12D3KooWNodeC",
        &proof_c[..1],
        root
    ));
}

/// 与生成器同输入重建导出包（锚 A/B 落库后构建）。
fn rebuild_package() -> EvidenceExportPackage {
    let mut s = fixed_chain();
    for r in &tree()[..2] {
        s.put(
            &anchor_key(&r.org_id, &r.node_id),
            &serde_json::to_string(r).unwrap(),
        )
        .unwrap();
    }
    build_export_package(
        &s,
        ExportScope {
            domain: Some("plugin:vote".to_string()),
            collection: Some("ballots".to_string()),
            org_id: Some(ORG.to_string()),
        },
        &key(0xEE),
        TS_BASE + 5000,
    )
    .expect("build package")
}

/// §10-4 导出包：canonical 包字节 + exporter.sig 固定值 + 五步核验通过。
#[test]
fn export_package_byte_exact_and_verifies() {
    let v = vectors();
    let g = &v["export"];
    let pkg = rebuild_package();
    assert_eq!(
        serde_json::to_value(&pkg).unwrap(),
        g["expect"]["package"],
        "包全文字段逐字节"
    );
    assert_eq!(
        normalize_object(&serde_json::to_value(&pkg).unwrap()),
        g["expect"]["canonicalPackage"].as_str().unwrap(),
        "canonical 包字节逐字节"
    );
    assert_eq!(
        export_sign_payload(&pkg),
        g["expect"]["signPayload"].as_str().unwrap(),
        "签名载荷逐字节"
    );
    assert_eq!(
        pkg.exporter.sig,
        g["expect"]["exporterSig"].as_str().unwrap(),
        "exporter.sig 固定值"
    );
    let report = verify_export_package(&serde_json::to_string(&pkg).unwrap());
    assert!(report.ok(), "五步核验全过: {:?}", report.failures);
    assert_eq!(report.height, 3);
    assert_eq!(report.anchor_count, 2);
}

/// §10-4 负面：逐字段篡改（条目/hash/锚/proof/签名）各项必败；断链必败。
#[test]
fn export_package_tamper_and_broken_chain_fail() {
    let pkg = rebuild_package();

    // 篡改条目内容
    let mut p = pkg.clone();
    p.entries[0].id = "forged".to_string();
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('②')),
        "{:?}",
        r.failures
    );

    // 篡改条目 hash
    let mut p = pkg.clone();
    p.entries[1].hash = "00".repeat(32);
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('②')),
        "{:?}",
        r.failures
    );

    // 篡改 head
    let mut p = pkg.clone();
    p.head.seq = 99;
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(!r.ok(), "{:?}", r.failures);

    // 篡改锚（headSeq +1，签名失效）
    let mut p = pkg.clone();
    p.anchors[0].head_seq += 1;
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('③')),
        "{:?}",
        r.failures
    );

    // 篡改 proof（换 sibling）
    let mut p = pkg.clone();
    let first_node = pkg.anchors[0].node_id.clone();
    p.anchor_proofs
        .insert(first_node, serde_json::json!(["00".repeat(32)]));
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('③')),
        "{:?}",
        r.failures
    );

    // 篡改 anchorRoot
    let mut p = pkg.clone();
    p.anchor_root = Some("ff".repeat(32));
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('③')),
        "{:?}",
        r.failures
    );

    // 篡改导出者签名
    let mut p = pkg.clone();
    p.exporter.sig = B64.encode([0x42; 64]);
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('①')),
        "{:?}",
        r.failures
    );

    // 断链（删中间条目）
    let mut p = pkg.clone();
    p.entries.remove(1);
    let r = verify_export_package(&serde_json::to_string(&p).unwrap());
    assert!(
        !r.ok() && r.failures.iter().any(|f| f.contains('②')),
        "{:?}",
        r.failures
    );
}
