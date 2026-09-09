//! 生成 `code/spec/vectors/evidence-roster.json`（evidence §4.1 名册段
//! golden vectors，§六验收：roster 段逐字节、anchorProof inclusion 正反、
//! 篡改各必败、v1/v2 互验行为）。
//!
//! 全部输入固定（密钥/ts/链条目/锚集/名册），输出逐字节确定——消费测试
//! `core/tests/evidence_roster_vectors.rs` 按本文件验收。
//! 运行：`cargo run --example gen_evidence_roster_vectors -- <repo_root>/code/spec/vectors/evidence-roster.json`

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::affair::snapshot::member_set_hash;
use spark_core::evidence::export::{
    ExportScope, RosterAnchorRef, RosterMember, RosterSection, build_export_package,
    roster_commitment_payload, verify_export_package,
};
use spark_core::evidence::{
    EvidenceOp, MembershipOutcome, NewEvidenceEntry, anchor_key, anchor_root, append_evidence,
    get_evidence_head, inclusion_proof, sign_anchor,
};
use spark_core::storage::{MemoryStorage, StorageBackend};

const ORG: &str = "org_0000000000000001";
const TS_BASE: i64 = 1_720_000_000_000;

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn key_b64(byte: u8) -> String {
    B64.encode([byte; 32])
}

fn identity_of(k: &SigningKey) -> String {
    hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
}

/// 固定治理链（3 张选票，与 evidence-anchor.json 组 4 同形态）。
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

/// 固定名册（导出者 0xEE=admin、覆盖锚 0x11=admin、成员锚 0x22=member）。
fn fixed_members() -> Vec<(String, String)> {
    vec![
        (identity_of(&key(0xEE)), "admin".to_string()),
        (identity_of(&key(0x11)), "admin".to_string()),
        (identity_of(&key(0x22)), "member".to_string()),
    ]
}

/// 名册 → member_set_hash 输入条目。
fn member_values(members: &[(String, String)]) -> Vec<Value> {
    members
        .iter()
        .map(|(id, role)| json!({ "identity": id, "role": role }))
        .collect()
}

/// 构建固定 v2 包（链 + 名册承诺条目 + 锚集 + roster 段 + 导出签名）。
fn build_v2(
    members: &[(String, String)],
) -> (
    spark_core::evidence::EvidenceExportPackage,
    RosterSection,
) {
    let mut s = fixed_chain();
    let hash = member_set_hash(&member_values(members)).expect("memberSetHash");
    let payload = roster_commitment_payload(ORG, &hash);
    append_evidence(
        &mut s,
        NewEvidenceEntry::from_parts(
            ORG,
            spark_core::evidence::ROSTER_ENTRY_COLLECTION,
            ORG,
            EvidenceOp::Put,
            Some(&payload),
            None,
            TS_BASE + 4000,
            "12D3KooWNodeA",
        ),
    )
    .expect("append roster commitment");
    let head = get_evidence_head(&s).expect("head").expect("head some");
    let cover = sign_anchor(&key(0x11), ORG, "12D3KooWNodeA", head.seq, &head.hash, TS_BASE + 5000);
    let other = sign_anchor(&key(0x22), ORG, "12D3KooWNodeB", 5, &hex::encode([0xCD; 32]), TS_BASE + 6000);
    for a in [&cover, &other] {
        s.put(
            &anchor_key(&a.org_id, &a.node_id),
            &serde_json::to_string(a).unwrap(),
        )
        .unwrap();
    }
    let anchors = vec![cover.clone(), other];
    let roster = RosterSection {
        member_set_hash: hash,
        anchor: RosterAnchorRef {
            org_id: ORG.to_string(),
            anchor_root: anchor_root(&anchors).unwrap(),
            ts: cover.ts,
        },
        snapshot: members
            .iter()
            .map(|(id, role)| RosterMember {
                identity: id.clone(),
                role: role.clone(),
                org_user_id: None,
            })
            .collect(),
        anchor_proof: inclusion_proof(&anchors, "12D3KooWNodeA").unwrap(),
    };
    let pkg = build_export_package(
        &s,
        ExportScope {
            domain: Some("plugin:vote".to_string()),
            collection: Some("ballots".to_string()),
            org_id: Some(ORG.to_string()),
        },
        &key(0xEE),
        TS_BASE + 7000,
        Some(roster.clone()),
    )
    .expect("build v2 package");
    (pkg, roster)
}

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../spec/vectors/evidence-roster.json".to_string());

    let members = fixed_members();
    let hash = member_set_hash(&member_values(&members)).expect("memberSetHash");

    // ── 组 1：roster 段逐字节（固定名册 → memberSetHash 固定值 + 快照线形）──
    let (pkg_v2, roster) = build_v2(&members);
    let group_roster = json!({
        "desc": "roster 段逐字节：固定名册（3 条目 identity+role）→ memberSetHash 固定值 + snapshot 线形固定；memberSetHash 复算口径 = affair/snapshot.rs member_set_hash（org-signature §3）",
        "input": { "members": member_values(&members) },
        "expect": {
            "memberSetHash": hash,
            "snapshot": serde_json::to_value(&roster.snapshot).unwrap(),
        },
    });

    // ── 组 2：anchorProof inclusion 正反（固定锚集 → proof 逐字节）──
    let group_proof = json!({
        "desc": "anchorProof inclusion 正反：固定锚集（覆盖锚 A + 成员锚 B）→ roster.anchorRoot 与 anchorProof 逐字节；消费侧断言正例可验、篡改 sibling/错 root 必败",
        "expect": {
            "anchorRoot": roster.anchor.anchor_root,
            "anchorProof": roster.anchor_proof,
        },
    });

    // ── 组 3：v2 包整体（六步核验通过；篡改 snapshot/memberSetHash/
    // anchorRoot 各必败——消费侧断言）──
    let raw_v2 = serde_json::to_string(&pkg_v2).unwrap();
    let report = verify_export_package(&raw_v2);
    assert!(report.ok(), "self-check v2 failed: {:?}", report.failures);
    assert_eq!(report.membership, MembershipOutcome::Pass);
    let group_v2 = json!({
        "desc": "v2 导出包：固定链（含名册承诺条目）+ 固定锚集 + roster 段 + 固定导出者密钥/ts → 六步核验全过、② 层 Pass、签名清单 3 条全有效；篡改 snapshot/memberSetHash/anchorRoot 各必败（消费侧断言）",
        "input": { "exporterKey": key_b64(0xEE), "exporterTs": TS_BASE + 7000 },
        "expect": { "package": serde_json::to_value(&pkg_v2).unwrap() },
    });

    // ── 组 4：v1/v2 互验（v1 包②层 NotCovered 不判失败；v2 缺 roster 必败）──
    let mut s1 = fixed_chain();
    let a1 = sign_anchor(&key(0x11), ORG, "12D3KooWNodeA", 3, &hex::encode([0xAB; 32]), TS_BASE + 5000);
    s1.put(
        &anchor_key(&a1.org_id, &a1.node_id),
        &serde_json::to_string(&a1).unwrap(),
    )
    .unwrap();
    let pkg_v1 = build_export_package(
        &s1,
        ExportScope {
            domain: Some("plugin:vote".to_string()),
            collection: Some("ballots".to_string()),
            org_id: Some(ORG.to_string()),
        },
        &key(0xEE),
        TS_BASE + 7000,
        None,
    )
    .expect("build v1 package");
    let report_v1 = verify_export_package(&serde_json::to_string(&pkg_v1).unwrap());
    assert!(report_v1.ok(), "self-check v1 failed: {:?}", report_v1.failures);
    assert_eq!(report_v1.membership, MembershipOutcome::NotCovered);
    let group_v1v2 = json!({
        "desc": "v1/v2 互验：v1 包 → ① 层通过、② 层 NotCovered（如实标注「本包不含名册快照」，不判失败）；v2 包缺 roster 段必败（消费侧断言）",
        "expect": {
            "v1Package": serde_json::to_value(&pkg_v1).unwrap(),
            "v1Membership": "not-covered",
        },
    });

    let doc = json!({
        "_comment": "evidence §4.1 导出包名册段 golden vectors（A11，2026-09-08）。生成器：core/examples/gen_evidence_roster_vectors.rs（Rust 自产，含六步核验自检）；消费：core/tests/evidence_roster_vectors.rs。",
        "roster": group_roster,
        "anchorProof": group_proof,
        "packageV2": group_v2,
        "v1v2": group_v1v2,
    });
    let text = serde_json::to_string_pretty(&doc).unwrap();
    std::fs::write(&out_path, format!("{text}\n")).expect("write vectors");
    println!("written {out_path}");
}
