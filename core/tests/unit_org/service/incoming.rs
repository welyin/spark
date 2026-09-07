//! 入站落库与 recovery 视图：`getRecoveryView` admin 惰性补齐、快照不携带
//! 根私钥密文（org.md §15）。（legacy 平面的 `applyNodeInfoClaim` /
//! `applyIncomingSnapshot` 用例已随该平面退役删除。）

use super::*;

use spark_core::org::types::OrganizationSyncVersions;

#[test]
fn recovery_view_admin_lazy_backfill() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let member_id = root_id_of(MNEMONIC2);
    OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW)
        .unwrap();

    // 有 recoverySecret → 直接返回，只有含地址成员的 nodeInfo
    let view = OrganizationService::get_recovery_view(
        &mut storage,
        &crate::test_io_lock(),
        &admin,
        NOW,
        "node-a",
    )
    .unwrap();
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
    let view = OrganizationService::get_recovery_view(
        &mut storage,
        &crate::test_io_lock(),
        &member_id,
        NOW + 10,
        "node-a",
    )
    .unwrap();
    assert!(view.is_empty());
    // admin 补齐：生成盐、bump updatedAt、保留 transactionsVersion 与 lastSyncedAt
    let view = OrganizationService::get_recovery_view(
        &mut storage,
        &crate::test_io_lock(),
        &admin,
        NOW + 20,
        "node-a",
    )
    .unwrap();
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
    let view = OrganizationService::get_recovery_view(
        &mut storage,
        &crate::test_io_lock(),
        &member_id,
        NOW + 30,
        "node-a",
    )
    .unwrap();
    assert_eq!(view.len(), 1);
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
