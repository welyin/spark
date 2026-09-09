//! `code/spec/vectors/evidence-roster.json` 消费测试（evidence §4.1 / §六
//! 验收）：roster 段逐字节复算、anchorProof inclusion 正反、篡改
//! snapshot/memberSetHash/anchorRoot 各必败、v1/v2 互验行为。
//! 向量由 `examples/gen_evidence_roster_vectors.rs` 生成（改算法须重生成）。

use serde_json::Value;
use spark_core::evidence::export::verify_export_package;
use spark_core::evidence::{EvidenceExportPackage, MembershipOutcome};

fn vectors() -> Value {
    let raw = include_str!("../../spec/vectors/evidence-roster.json");
    serde_json::from_str(raw).expect("parse evidence-roster vectors")
}

/// roster 段逐字节：固定名册 → memberSetHash 固定值 + snapshot 线形固定。
#[test]
fn roster_section_byte_exact() {
    let v = vectors();
    let members = v["roster"]["input"]["members"].as_array().unwrap();
    let expect_hash = v["roster"]["expect"]["memberSetHash"].as_str().unwrap();
    // memberSetHash 复算（affair/snapshot.rs 同算法——org-signature §3 口径）
    let recomputed = spark_core::affair::snapshot::member_set_hash(members).unwrap();
    assert_eq!(recomputed, expect_hash, "memberSetHash 逐字节漂移");
    // snapshot 线形与名册输入一致（identity/role 两键，保留名册原序——
    // 排序只在 memberSetHash 承诺内部做，段本身不改写名册顺序）
    let snapshot = v["roster"]["expect"]["snapshot"].as_array().unwrap();
    assert_eq!(members, snapshot, "snapshot 线形漂移");
}

/// anchorProof inclusion 正反：向量 proof 对包内 v2 package 可验（正例在
/// 组 3 整包核验覆盖）；此处锁 proof/root 逐字节 + 错 root 反例必败。
#[test]
fn anchor_proof_positive_and_negative() {
    let v = vectors();
    let root = v["anchorProof"]["expect"]["anchorRoot"].as_str().unwrap();
    let proof: Vec<String> = serde_json::from_value(v["anchorProof"]["expect"]["anchorProof"].clone())
        .unwrap();
    let pkg_raw = serde_json::to_string(&v["packageV2"]["expect"]["package"]).unwrap();
    let pkg: EvidenceExportPackage = serde_json::from_str(&pkg_raw).unwrap();
    let roster = pkg.roster.as_ref().unwrap();
    assert_eq!(roster.anchor.anchor_root, root, "anchorRoot 逐字节漂移");
    assert_eq!(&roster.anchor_proof, &proof, "anchorProof 逐字节漂移");
    // 正例：覆盖锚（nodeA）可验
    assert!(spark_core::evidence::verify_inclusion(
        &pkg.anchors,
        "12D3KooWNodeA",
        &proof,
        root
    ));
    // 反例：错 root / 换叶索引 / 篡改 sibling 必败
    assert!(!spark_core::evidence::verify_inclusion(
        &pkg.anchors,
        "12D3KooWNodeA",
        &proof,
        &"00".repeat(32)
    ));
    assert!(!spark_core::evidence::verify_inclusion(
        &pkg.anchors,
        "12D3KooWNodeB",
        &proof,
        root
    ));
    if !proof.is_empty() {
        let mut bad = proof.clone();
        bad[0] = "00".repeat(32);
        assert!(!spark_core::evidence::verify_inclusion(
            &pkg.anchors,
            "12D3KooWNodeA",
            &bad,
            root
        ));
    }
}

/// v2 包整体：六步核验全过（② 层 Pass、签名清单全有效）；篡改
/// snapshot / memberSetHash / anchorRoot 各必败（② 层）。
#[test]
fn package_v2_verify_and_tamper_failures() {
    let v = vectors();
    let pkg_raw = serde_json::to_string(&v["packageV2"]["expect"]["package"]).unwrap();
    let report = verify_export_package(&pkg_raw);
    assert!(report.ok(), "v2 包六步核验: {:?}", report.failures);
    assert_eq!(report.membership, MembershipOutcome::Pass);
    assert!(report.signer_checks.iter().all(|c| c.ok));
    assert_eq!(report.roster_summary.as_ref().unwrap().member_count, 3);

    let pkg: EvidenceExportPackage = serde_json::from_str(&pkg_raw).unwrap();
    // 篡改 snapshot（换一名成员 identity）
    let mut p1 = pkg.clone();
    p1.roster.as_mut().unwrap().snapshot[2].identity = "ff".repeat(32);
    let r = verify_export_package(&serde_json::to_string(&p1).unwrap());
    assert!(!r.ok(), "篡改 snapshot 必败");
    assert!(r.failures.iter().any(|f| f.contains('②')));
    // 篡改 memberSetHash
    let mut p2 = pkg.clone();
    p2.roster.as_mut().unwrap().member_set_hash = "00".repeat(32);
    let r = verify_export_package(&serde_json::to_string(&p2).unwrap());
    assert!(!r.ok(), "篡改 memberSetHash 必败");
    assert!(r.failures.iter().any(|f| f.contains('②')));
    // 篡改 anchorRoot
    let mut p3 = pkg.clone();
    p3.roster.as_mut().unwrap().anchor.anchor_root = "00".repeat(32);
    let r = verify_export_package(&serde_json::to_string(&p3).unwrap());
    assert!(!r.ok(), "篡改 anchorRoot 必败");
    assert!(r.failures.iter().any(|f| f.contains('②')));
}

/// v1/v2 互验：v1 包①层通过、② 层 NotCovered（如实标注不判失败）；
/// v2 缺 roster 段必败。
#[test]
fn v1_v2_cross_verification_behavior() {
    let v = vectors();
    let v1_raw = serde_json::to_string(&v["v1v2"]["expect"]["v1Package"]).unwrap();
    let report = verify_export_package(&v1_raw);
    assert!(report.ok(), "v1 包①层通过: {:?}", report.failures);
    assert_eq!(report.membership, MembershipOutcome::NotCovered);
    assert!(
        report
            .membership_notes
            .iter()
            .any(|n| n.contains("不含名册快照")),
        "v1 ② 层如实标注: {:?}",
        report.membership_notes
    );

    // v2 缺 roster 段（手工抬版本）→ 必败
    let v1_pkg: EvidenceExportPackage = serde_json::from_str(&v1_raw).unwrap();
    let mut forged = v1_pkg.clone();
    forged.format_version = 2;
    let r = verify_export_package(&serde_json::to_string(&forged).unwrap());
    assert!(!r.ok(), "v2 缺 roster 段必败");
    assert!(r.failures.iter().any(|f| f.contains('②')));
}
