//! 存证锚定 kernel 级全链验收（阶段四F，模拟治理集合）：组织 + 治理集合
//! （append-only + governance）写入 → 链头变化自动锚定（org:evi:anchor:
//! 落库）→ 导出包构建 → 五步核验通过；锚幂等 + 分叉检测判定表。

mod common;

use spark_core::collection::CollectionConfig;
use spark_core::evidence::{
    AnchorFork, EVIDENCE_ANCHOR_PREFIX, EVIDENCE_FORK_PREFIX, ExportScope, detect_anchor_fork,
    sign_anchor, verify_anchor,
};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::schema::{CollectionSchemaDeclaration, SyncStrategy};

use common::*;

/// 模拟治理集合全链：写入 → 自动锚定 → 导出 → 核验（投票插件是消费者
/// 不是依赖，设计 §4）。
#[test]
fn governance_collection_anchor_export_verify_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _) = init_identity(&mut kernel);

    // 组织 + 治理集合声明（投票集合形态：append-only + governance）
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "治理测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    kernel
        .declare_collection(
            "plugin:vote",
            "ballots",
            CollectionSchemaDeclaration {
                sync_strategy: Some(SyncStrategy::AppendOnly),
                governance: true,
                enable_evidence: true,
            },
        )
        .unwrap();

    // 治理事件写入（3 张选票 → 存证链 3 条目；链头变化应触发自动锚定）
    for i in 1..=3 {
        kernel
            .doc_put(
                "plugin:vote",
                "ballots",
                &format!("ballot-{i}"),
                serde_json::json!({"choice": "approve", "voter": i}),
                CollectionConfig::default(),
            )
            .unwrap();
    }
    let status = kernel.evidence_verify().unwrap();
    assert!(status.valid && status.height == 3, "链高 3 且校验通过");

    // 链头变化自动锚定：本机成员组织的锚记录已落库（orgsync 键域）
    let anchors = kernel.evidence_anchors(&org_id).unwrap();
    assert_eq!(anchors.len(), 1, "单节点锚唯一");
    let anchor = &anchors[0];
    assert_eq!(anchor.org_id, org_id);
    assert!(!anchor.node_id.is_empty());
    assert_eq!(anchor.head_seq, 3);
    assert_eq!(anchor.head_hash.len(), 64);
    assert!(verify_anchor(anchor), "锚签名有效");

    // 幂等：链头未变显式再锚 → false
    assert!(!kernel.evidence_anchor(&org_id).unwrap(), "链头未变不重锚");

    // 导出包：按治理集合 scope 声明 → 五步核验全过
    let pkg = kernel
        .evidence_export(ExportScope {
            domain: Some("plugin:vote".to_string()),
            collection: Some("ballots".to_string()),
            org_id: Some(org_id.clone()),
        })
        .unwrap();
    assert_eq!(pkg.head.seq, 3);
    assert_eq!(pkg.entries.len(), 3);
    assert_eq!(pkg.anchors.len(), 1, "导出时刻已知本机锚");
    assert!(pkg.anchor_root.is_some());
    let raw = serde_json::to_string_pretty(&pkg).unwrap();
    let report = spark_core::evidence::verify_export_package(&raw);
    assert!(report.ok(), "五步核验全过: {:?}", report.failures);
    assert_eq!(report.height, 3);
    assert_eq!(report.anchor_count, 1);

    // 再写入 → 链头变化 → 锚前进（LWW 自覆盖）
    kernel
        .doc_put(
            "plugin:vote",
            "ballots",
            "ballot-4",
            serde_json::json!({"choice": "reject", "voter": 4}),
            CollectionConfig::default(),
        )
        .unwrap();
    let anchors = kernel.evidence_anchors(&org_id).unwrap();
    assert_eq!(anchors.len(), 1, "LWW 自覆盖仍单条");
    assert_eq!(anchors[0].head_seq, 4, "链头变化锚前进");
    assert!(verify_anchor(&anchors[0]));
}

/// 分叉检测判定表 + 留档形状（§8；入站 LWW 覆盖前的留档接线在
/// inbound_dm/orgsync/data.rs，此处锁判定语义与留档负载线形）。
#[test]
fn anchor_fork_detect_and_archive_shape() {
    let key_a = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let org_id = "org_0000000000000001";
    let local = sign_anchor(&key_a, org_id, "nodeX", 10, &"ab".repeat(32), 1000);
    // 回退
    let regress = sign_anchor(&key_a, org_id, "nodeX", 5, &"cd".repeat(32), 2000);
    assert_eq!(
        detect_anchor_fork(&local, &regress),
        Some(AnchorFork::SeqRegression)
    );
    // 同 seq 异 hash
    let conflict = sign_anchor(&key_a, org_id, "nodeX", 10, &"ef".repeat(32), 3000);
    assert_eq!(
        detect_anchor_fork(&local, &conflict),
        Some(AnchorFork::SameSeqHashMismatch)
    );
    // 正常前进/相同/异节点 → 非分叉
    let forward = sign_anchor(&key_a, org_id, "nodeX", 11, &"ab".repeat(32), 4000);
    assert_eq!(detect_anchor_fork(&local, &forward), None);
    assert_eq!(detect_anchor_fork(&local, &local.clone()), None);
    let other_node = sign_anchor(&key_a, org_id, "nodeY", 1, &"00".repeat(32), 4000);
    assert_eq!(detect_anchor_fork(&local, &other_node), None);

    // 留档形状：双份签名锚全文 + 类别（可举证性 = 并排签名锚）
    let archive =
        spark_core::evidence::fork_archive_value(&local, &regress, AnchorFork::SeqRegression, 4000);
    assert_eq!(archive["kind"], "seq-regression");
    let local_in: spark_core::evidence::AnchorRecord =
        serde_json::from_value(archive["local"].clone()).unwrap();
    let incoming_in: spark_core::evidence::AnchorRecord =
        serde_json::from_value(archive["incoming"].clone()).unwrap();
    assert!(verify_anchor(&local_in) && verify_anchor(&incoming_in));
}

/// 键族前缀常量锚点（防前缀漂移无人察觉）。
#[test]
fn prefix_shape() {
    assert_eq!(EVIDENCE_ANCHOR_PREFIX, "org:evi:anchor:");
    assert_eq!(EVIDENCE_FORK_PREFIX, "org:evi:fork:");
}
