//! 组织设置：`setOrgGateways` 规则（数量/成员校验、去重归一、幂等）与
//! `setOrgPublic`（展示名更新、幂等、存量组织根密钥对懒补齐，org.md §16/§15）、
//! `updateOrgInfo`（名称/描述更新、幂等、admin 校验）。

use super::*;

use spark_core::org::OrgError;

#[test]
fn update_org_info_rules() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);

    // 非 admin 拒绝 / 组织不存在
    assert!(matches!(
        OrganizationService::update_org_info(
            &mut storage,
            &record.org_id,
            Some("新名字"),
            None,
            &rid('x'),
            NOW + 1
        ),
        Err(OrgError::AdminRequired)
    ));
    assert!(matches!(
        OrganizationService::update_org_info(&mut storage, "org_nope", Some("新名字"), None, &admin, NOW + 1),
        Err(OrgError::OrganizationNotFound)
    ));
    // 名称 trim 后为空：拒绝
    assert!(matches!(
        OrganizationService::update_org_info(&mut storage, &record.org_id, Some("   "), None, &admin, NOW + 1),
        Err(OrgError::Required(_))
    ));

    // 更新名称（含 trim 归一），描述不变
    let updated = OrganizationService::update_org_info(
        &mut storage,
        &record.org_id,
        Some("  星火团队  "),
        None,
        &admin,
        NOW + 2,
    )
    .unwrap();
    assert_eq!(updated.name, "星火团队");
    assert_eq!(updated.description, record.description);
    assert_eq!(updated.updated_at, NOW + 2);
    assert_eq!(
        updated.sync.as_ref().unwrap().versions.summary_version,
        NOW + 2
    );
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].summary, "更新组织信息");
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["name"],
        serde_json::json!("星火团队")
    );

    // 幂等：同值重复设置不 bump 版本
    let same = OrganizationService::update_org_info(
        &mut storage,
        &record.org_id,
        Some("星火团队"),
        None,
        &admin,
        NOW + 99,
    )
    .unwrap();
    assert_eq!(same.updated_at, NOW + 2);

    // 只更新描述（空串 = 清除描述）
    let cleared = OrganizationService::update_org_info(
        &mut storage,
        &record.org_id,
        None,
        Some("   "),
        &admin,
        NOW + 3,
    )
    .unwrap();
    assert_eq!(cleared.name, "星火团队");
    assert_eq!(cleared.description, "");
    assert_eq!(cleared.updated_at, NOW + 3, "清除是一次真实变更，bump 版本");
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["description"],
        serde_json::json!("")
    );
}

#[test]
fn set_org_gateways_rules() {
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

    // 非 admin 拒绝
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &record.org_id,
            &[admin.clone(), member_id.clone()],
            &member_id,
            NOW + 2,
        ),
        Err(OrgError::AdminRequired)
    ));
    // 数量不足 2 / 超过 3
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &record.org_id,
            &[admin.clone()],
            &admin,
            NOW + 2,
        ),
        Err(OrgError::InvalidGateways)
    ));
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &record.org_id,
            &[admin.clone(), member_id.clone(), rid('c'), rid('d')],
            &admin,
            NOW + 2,
        ),
        Err(OrgError::InvalidGateways)
    ));
    // 非成员 / 非法 rootId
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &record.org_id,
            &[admin.clone(), rid('e')],
            &admin,
            NOW + 2,
        ),
        Err(OrgError::InvalidGateways)
    ));
    assert!(matches!(
        OrganizationService::set_org_gateways(
            &mut storage,
            &record.org_id,
            &[admin.clone(), "zz".to_string()],
            &admin,
            NOW + 2,
        ),
        Err(OrgError::InvalidGateways)
    ));

    // 正常设置（含重复项去重 + 大小写/空白归一）
    let messy = vec![
        format!(" {} ", admin.to_uppercase()),
        member_id.clone(),
        member_id.clone(),
    ];
    let updated = OrganizationService::set_org_gateways(
        &mut storage,
        &record.org_id,
        &messy,
        &admin,
        NOW + 3,
    )
    .unwrap();
    assert_eq!(updated.gateways, vec![admin.clone(), member_id.clone()]);
    assert!(updated.is_gateway(&admin));
    assert_eq!(updated.updated_at, NOW + 3);
    assert_eq!(
        updated.sync.as_ref().unwrap().versions.summary_version,
        NOW + 3
    );
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].summary, "更新组织网关（2 个）");
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["gateways"],
        serde_json::json!([admin.clone(), member_id.clone()])
    );

    // 重复设置相同列表：幂等，不 bump 版本
    let same = OrganizationService::set_org_gateways(
        &mut storage,
        &record.org_id,
        &[admin.clone(), member_id.clone()],
        &admin,
        NOW + 99,
    )
    .unwrap();
    assert_eq!(same.updated_at, NOW + 3);
}

#[test]
fn set_org_public_rules() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);

    // 非 admin 拒绝 / 组织不存在
    assert!(matches!(
        OrganizationService::set_org_public(
            &mut storage,
            &record.org_id,
            true,
            None,
            &rid('x'),
            NOW + 1
        ),
        Err(OrgError::AdminRequired)
    ));
    assert!(matches!(
        OrganizationService::set_org_public(&mut storage, "org_nope", true, None, &admin, NOW + 1),
        Err(OrgError::OrganizationNotFound)
    ));

    // 开启公开 + 设置展示名
    let updated = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        Some("  星火 公开组织  "),
        &admin,
        NOW + 2,
    )
    .unwrap();
    assert!(updated.is_public);
    assert_eq!(updated.display_name_override(), Some("星火 公开组织"));
    assert_eq!(updated.updated_at, NOW + 2);
    // 组织根密钥对已存在（创建时生成）→ 不重新生成
    assert_eq!(updated.org_address, record.org_address);
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].summary, "开启组织公开");
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["isPublic"],
        serde_json::json!(true)
    );
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["displayName"],
        serde_json::json!("星火 公开组织")
    );

    // 幂等：同值重复设置不 bump 版本
    let same = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        Some("星火 公开组织"),
        &admin,
        NOW + 99,
    )
    .unwrap();
    assert_eq!(same.updated_at, NOW + 2);

    // 只更新展示名（public 参数不变）
    let renamed = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        Some("新展示名"),
        &admin,
        NOW + 3,
    )
    .unwrap();
    assert!(renamed.is_public);
    assert_eq!(renamed.display_name_override(), Some("新展示名"));
    assert_eq!(renamed.updated_at, NOW + 3);

    // 空串展示名 = 清除（删除 orgDisplayName 覆盖键，地址记录回退用组织名）
    let cleared = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        Some("   "),
        &admin,
        NOW + 4,
    )
    .unwrap();
    assert_eq!(cleared.display_name_override(), None);
    assert_eq!(cleared.updated_at, NOW + 4, "清除是一次真实变更，bump 版本");

    // 已清除后再传空串：无变化，幂等不 bump
    let same = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        Some(""),
        &admin,
        NOW + 99,
    )
    .unwrap();
    assert_eq!(same.updated_at, NOW + 4);

    // 关闭公开
    let closed = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        false,
        None,
        &admin,
        NOW + 5,
    )
    .unwrap();
    assert!(!closed.is_public);
    // 展示名清除态与 orgAddress/根私钥保留（重开不丢、也不复活旧展示名）
    assert_eq!(closed.display_name_override(), None);
    assert_eq!(closed.org_address, record.org_address);
    assert!(closed.org_root_secret().is_some());
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].summary, "关闭组织公开");
}

#[test]
fn set_org_public_lazy_backfills_root_keypair() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    // 手工抹掉根密钥对与 orgSecret 模拟存量组织
    let mut bare = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    bare.org_address = None;
    bare.extra
        .remove(spark_core::org::types::OrganizationRecord::ORG_ROOT_SECRET_KEY);
    bare.extra
        .remove(spark_core::org::types::OrganizationRecord::ORG_SECRET_KEY);
    OrganizationService::save_record(&mut storage, &bare).unwrap();

    let updated = OrganizationService::set_org_public(
        &mut storage,
        &record.org_id,
        true,
        None,
        &admin,
        NOW + 1,
    )
    .unwrap();
    // 懒补齐：orgAddress 生成、orgSecret 补齐、根私钥密文可解密且闭环
    let org_address = updated.org_address.clone().expect("backfilled orgAddress");
    assert!(spark_core::org::org_address::is_valid_org_address(&org_address));
    assert_eq!(updated.org_secret().map(str::len), Some(64));
    let signing = spark_core::org::org_address::org_root_signing_key(&updated).expect("opens");
    let digest = spark_core::org::org_address::decode_org_address(&org_address).unwrap();
    assert_eq!(
        <sha2::Sha256 as sha2::Digest>::digest(signing.verifying_key().to_bytes()).as_slice(),
        digest
    );
}
