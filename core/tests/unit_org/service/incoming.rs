//! 入站落库与 recovery 视图：`applyNodeInfoClaim` 全规则（落库三条件、幂等
//! 跳过、防代填）、`applyIncomingSnapshot` 两种线形、`getRecoveryView` admin
//! 惰性补齐、快照不携带根私钥密文（org.md §15）。

use super::*;

use spark_core::org::claim::{NodeInfoClaim, sign_node_info_claim};
use spark_core::org::tx::OrganizationTransactionType;
use spark_core::org::types::{OrganizationNodeInfo, OrganizationSyncVersions};

fn claim_for(mnemonic: &str, peer_id: Option<&str>, now: i64) -> NodeInfoClaim {
    let parsed = parse_mnemonic(mnemonic).unwrap();
    let identity = derive_root_identity(&parsed.seed);
    sign_node_info_claim(
        &identity.signing_key,
        OrganizationNodeInfo {
            peer_id: peer_id.map(str::to_string),
            addresses: vec!["/ip4/5.6.7.8/tcp/15002/ws".to_string()],
        },
        now,
    )
}

#[test]
fn apply_node_info_claim_full_rules() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member_id,
        None,
        &admin,
        NOW + 1,
    )
    .unwrap();

    let claim = claim_for(MNEMONIC2, Some("12D3KooWMember"), NOW + 2);
    // 落库：admin + 成员双条件满足
    let applied = OrganizationService::apply_node_info_claim(
        &mut storage,
        &claim,
        &admin,
        Some("12D3KooWMember"),
        NOW + 2,
    )
    .unwrap();
    assert_eq!(applied, vec![record.org_id.clone()]);
    let updated = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    let m = updated.find_member(&member_id).unwrap();
    assert_eq!(
        m.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some("12D3KooWMember")
    );
    assert_eq!(updated.updated_at, NOW + 2);
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].type_, OrganizationTransactionType::MemberUpdate);
    assert_eq!(txs[0].actor_root_id, member_id);
    assert_eq!(
        txs[0].summary,
        format!("成员节点地址自动回填 {}", &member_id[..8])
    );
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["source"],
        "node-info-claim"
    );

    // 与现有 nodeInfo 完全一致 → 跳过，不 bump 版本
    let applied = OrganizationService::apply_node_info_claim(
        &mut storage,
        &claim,
        &admin,
        Some("12D3KooWMember"),
        NOW + 5000,
    )
    .unwrap();
    assert!(applied.is_empty());
    let same = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(same.updated_at, NOW + 2, "unchanged 不得 bump updatedAt");

    // remotePeerId 不匹配 → 静默丢弃
    let applied = OrganizationService::apply_node_info_claim(
        &mut storage,
        &claim,
        &admin,
        Some("12D3KooWOther"),
        NOW + 6000,
    )
    .unwrap();
    assert!(applied.is_empty());

    // 非 admin 当前用户 → 静默跳过
    let applied = OrganizationService::apply_node_info_claim(
        &mut storage,
        &claim,
        &rid('x'),
        None,
        NOW + 7000,
    )
    .unwrap();
    assert!(applied.is_empty());

    // 声明者不是成员 → 跳过
    let outsider_claim = claim_for(MNEMONIC, Some("12D3KooWAdmin2"), NOW);
    let applied = OrganizationService::apply_node_info_claim(
        &mut storage,
        &outsider_claim,
        &member_id,
        None,
        NOW,
    )
    .unwrap();
    assert!(applied.is_empty());

    // 过期 claim → 不落库
    let stale_claim = claim_for(MNEMONIC2, Some("12D3KooWMember"), NOW - 20 * 60 * 1000);
    let applied =
        OrganizationService::apply_node_info_claim(&mut storage, &stale_claim, &admin, None, NOW)
            .unwrap();
    assert!(applied.is_empty());
}

#[test]
fn recovery_view_admin_lazy_backfill() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW)
        .unwrap();

    // 有 recoverySecret → 直接返回，只有含地址成员的 nodeInfo
    let view = OrganizationService::get_recovery_view(&mut storage, &admin, NOW).unwrap();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].org_id, record.org_id);
    assert_eq!(view[0].recovery_secret.len(), 64);
    assert!(view[0].member_node_infos.is_empty(), "成员均无地址");

    // 手工抹掉 recoverySecret 模拟存量组织：admin 惰性补齐
    let mut bare = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    bare.extra
        .remove(spark_core::org::types::OrganizationRecord::RECOVERY_SECRET_KEY);
    bare.sync = Some(spark_core::org::types::OrganizationSyncState {
        versions: OrganizationSyncVersions {
            summary_version: 1,
            members_version: 2,
            member_details_version: 3,
            transactions_version: 4,
        },
        sections: spark_core::org::snapshot::pick_sync_sections_by_priority(),
        last_synced_at: 777,
    });
    OrganizationService::save_record(&mut storage, &bare).unwrap();

    // 非 admin 成员本轮跳过
    let view = OrganizationService::get_recovery_view(&mut storage, &member_id, NOW + 10).unwrap();
    assert!(view.is_empty());
    // admin 补齐：生成盐、bump updatedAt、保留 transactionsVersion 与 lastSyncedAt
    let view = OrganizationService::get_recovery_view(&mut storage, &admin, NOW + 20).unwrap();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].recovery_secret.len(), 64);
    let patched = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(patched.updated_at, NOW + 20);
    let sync = patched.sync.as_ref().unwrap();
    assert_eq!(sync.versions.summary_version, NOW + 20);
    assert_eq!(
        sync.versions.transactions_version, 4,
        "保留原 transactionsVersion"
    );
    assert_eq!(sync.last_synced_at, 777, "保留原 lastSyncedAt");
    // 成员侧随后也能看到
    let view = OrganizationService::get_recovery_view(&mut storage, &member_id, NOW + 30).unwrap();
    assert_eq!(view.len(), 1);
}

#[test]
fn apply_incoming_snapshot_accepts_both_shapes() {
    let mut storage = MemoryStorage::new();
    let (_admin, record) = setup_org(&mut storage);
    // 原始记录线形（org-share 推送）
    let value = serde_json::to_value(&record).unwrap();
    let merged =
        OrganizationService::apply_incoming_snapshot(&mut storage, &value, NOW + 1).unwrap();
    assert_eq!(merged.org_id, record.org_id);
    assert_eq!(merged.sync.as_ref().unwrap().last_synced_at, NOW + 1);

    // 快照线形（org-pull 响应）
    let snapshot = spark_core::org::snapshot::build_organization_sync_snapshot(&record, &[]);
    let value2 = serde_json::to_value(&snapshot).unwrap();
    let merged2 =
        OrganizationService::apply_incoming_snapshot(&mut storage, &value2, NOW + 2).unwrap();
    assert_eq!(merged2.members.len(), 1);
    assert_eq!(merged2.sync.as_ref().unwrap().last_synced_at, NOW + 2);
}

#[test]
fn snapshot_never_carries_org_root_secret() {
    let mut storage = MemoryStorage::new();
    let (_admin, record) = setup_org(&mut storage);
    // 快照构建：orgRootSecret 被剔除（org.md §15 不同步出本机）
    let snapshot = spark_core::org::snapshot::build_organization_sync_snapshot(&record, &[]);
    let metadata = snapshot.summary.metadata.as_ref().unwrap();
    assert!(
        !metadata.contains_key("orgRootSecret"),
        "根私钥密文不得进 metadata"
    );
    assert!(metadata.contains_key("orgSecret"), "orgSecret 仍随快照流动");
    // orgAddress 作为 summary 显式字段传播；isPublic=false 缺省丢键
    assert_eq!(snapshot.summary.org_address, record.org_address);
    assert_eq!(snapshot.summary.is_public, None);
    // 公开组织的 isPublic=true 显式传播
    let mut public_record = record.clone();
    public_record.is_public = true;
    let snapshot = spark_core::org::snapshot::build_organization_sync_snapshot(&public_record, &[]);
    assert_eq!(snapshot.summary.is_public, Some(true));
    // 合并：orgAddress/isPublic 落到 merged，orgRootSecret 不会经 metadata 注入
    let merged = spark_core::org::snapshot::merge_organization_sync_snapshot(None, &snapshot, NOW);
    assert_eq!(merged.org_address, record.org_address);
    assert!(merged.is_public);
    assert!(merged.org_root_secret().is_none());
}
