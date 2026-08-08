//! 创建/删除组织：`createOrganization` 归一化与落库、输入校验、组织根密钥对
//! 生成（org.md §15）、`deleteOrganization` 流程。

use super::*;

use spark_core::org::OrgError;
use spark_core::org::tx::OrganizationTransactionType;
use spark_core::org::types::OrganizationRole;

#[test]
fn create_organization_normalizes_and_persists() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    assert_eq!(record.name, "星火 组织");
    assert_eq!(record.description, "描述");
    assert_eq!(record.base_plugin_domain.as_deref(), Some("plugin:chat"));
    assert!(record.org_id.starts_with("org_") && record.org_id.len() == 20);
    assert_eq!(record.recovery_secret().map(str::len), Some(64));
    // orgSecret：创建时生成（org.md §13），与 recoverySecret 相互独立
    assert_eq!(record.org_secret().map(str::len), Some(64));
    assert_ne!(record.org_secret(), record.recovery_secret());
    assert!(record.gateways.is_empty());
    assert_eq!(record.members.len(), 1);
    assert_eq!(record.members[0].role, OrganizationRole::Admin);
    assert_eq!(record.members[0].root_id, admin);
    assert_eq!(record.created_at, NOW);
    assert_eq!(record.updated_at, NOW);
    // sync：versions 的 transactionsVersion 取 create 事务 createdAt
    let sync = record.sync.as_ref().unwrap();
    assert_eq!(sync.versions.summary_version, NOW);
    assert_eq!(sync.versions.transactions_version, NOW);
    assert_eq!(sync.last_synced_at, 0);
    // 落库可读回（字节一致）
    let loaded = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded, record);
    // create 事务已写入
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 20).unwrap();
    assert_eq!(txs.len(), 1);
    assert_eq!(txs[0].type_, OrganizationTransactionType::Create);
    assert_eq!(txs[0].summary, "创建组织 星火 组织");
}

#[test]
fn create_organization_validates_input() {
    let mut storage = MemoryStorage::new();
    let admin = rid('a');
    let mut bad = input();
    bad.name = "   ".to_string();
    assert!(matches!(
        OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
        Err(OrgError::Required(label)) if label == "Organization name"
    ));
    let mut bad = input();
    bad.base_plugin_domain = Some("chat".to_string());
    assert!(matches!(
        OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
        Err(OrgError::InvalidBasePluginDomain)
    ));
}

#[test]
fn create_organization_without_base_plugin_domain() {
    let mut storage = MemoryStorage::new();
    let admin = rid('a');
    // base_plugin_domain 省略（None）或空白均视为未设置：组织与插件不再强关联（设计 §7.2）
    for base_plugin_domain in [None, Some("   ".to_string())] {
        let input = CreateOrganizationInput {
            base_plugin_domain,
            ..input()
        };
        let record =
            OrganizationService::create_organization(&mut storage, &input, &admin, NOW).unwrap();
        assert_eq!(record.base_plugin_domain, None);
        // create 事务 payload 不含 basePluginDomain 键
        let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1)
            .unwrap();
        let payload = txs[0].payload.as_ref().unwrap();
        assert!(!payload.contains_key("basePluginDomain"));
    }
}

#[test]
fn create_organization_avatar_rules() {
    let mut storage = MemoryStorage::new();
    let admin = rid('a');
    let logo = "data:image/png;base64,iVBORw0KGgo=";

    // 合法 logo：trim 后落记录，create 事务 payload 携带 avatar
    let mut with_logo = input();
    with_logo.avatar = Some(format!("  {logo}  "));
    let record =
        OrganizationService::create_organization(&mut storage, &with_logo, &admin, NOW).unwrap();
    assert_eq!(record.avatar, logo);
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1)
        .unwrap();
    assert_eq!(
        txs[0].payload.as_ref().unwrap()["avatar"],
        serde_json::json!(logo)
    );

    // 省略（None）或空白均视为未设置：空串落记录，payload 不含 avatar 键
    for avatar in [None, Some("   ".to_string())] {
        let input = CreateOrganizationInput {
            avatar,
            ..input()
        };
        let record =
            OrganizationService::create_organization(&mut storage, &input, &admin, NOW).unwrap();
        assert_eq!(record.avatar, "");
        let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1)
            .unwrap();
        assert!(!txs[0].payload.as_ref().unwrap().contains_key("avatar"));
    }

    // 非法 logo（非 data:image/ 前缀）：拒绝，不落库
    let mut bad = input();
    bad.avatar = Some("https://example.com/logo.png".to_string());
    assert!(matches!(
        OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
        Err(OrgError::InvalidAvatar(_))
    ));
    // 超限 logo（JSON 序列化后 > 200KB）：拒绝
    let mut bad = input();
    bad.avatar = Some(format!("data:image/png;base64,{}", "A".repeat(300 * 1024)));
    assert!(matches!(
        OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
        Err(OrgError::InvalidAvatar(_))
    ));
}

#[test]
fn create_organization_generates_org_root_keypair() {
    let mut storage = MemoryStorage::new();
    let (_admin, record) = setup_org(&mut storage);
    // orgAddress：创建时生成，55 字符可解码（org.md §15）
    let org_address = record.org_address.clone().expect("orgAddress generated");
    assert_eq!(org_address.len(), 55);
    assert!(spark_core::org::org_address::is_valid_org_address(&org_address));
    // 默认不公开
    assert!(!record.is_public);
    // 根私钥密文存 extra，可解密回 SigningKey 且公钥与 orgAddress 闭环
    let signing = spark_core::org::org_address::org_root_signing_key(&record).expect("root key opens");
    let digest = spark_core::org::org_address::decode_org_address(&org_address).unwrap();
    assert_eq!(
        <sha2::Sha256 as sha2::Digest>::digest(signing.verifying_key().to_bytes()).as_slice(),
        digest
    );
    // 密文不是明文种子（base64 且长度对：12 nonce + 32 seed + 16 tag = 60 字节）
    let sealed = record.org_root_secret().unwrap();
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, sealed).unwrap();
    assert_eq!(raw.len(), 60);
}

#[test]
fn delete_organization_flow() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    assert!(matches!(
        OrganizationService::delete_organization(&mut storage, &record.org_id, &rid('x'), NOW),
        Err(OrgError::AdminRequired)
    ));
    OrganizationService::delete_organization(&mut storage, &record.org_id, &admin, NOW + 1)
        .unwrap();
    assert!(
        OrganizationService::get_record(&storage, &record.org_id)
            .unwrap()
            .is_none()
    );
    let txs = spark_core::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
    assert_eq!(txs[0].type_, OrganizationTransactionType::Delete);
}

#[test]
fn create_delete_pdsync_write_pmeta_and_tombstone() {
    use spark_core::sync::{get_personal_meta, is_tombstone};

    let mut storage = MemoryStorage::new();
    let (admin, record) = {
        let admin = root_id_of(MNEMONIC);
        let record = OrganizationService::create_organization_pdsync(
            &mut storage,
            &input(),
            &admin,
            NOW,
            "node-a",
        )
        .unwrap();
        (admin, record)
    };
    // 创建：org:meta 记录落库 + pmeta（vv 含 node-a:1，非 tombstone）
    let key = format!("org:meta:{}", record.org_id);
    let meta = get_personal_meta(&storage, &key).unwrap().unwrap();
    assert_eq!(meta.vv.get("node-a"), Some(&1));
    assert!(!is_tombstone(&meta));

    // 删除：记录消失，pmeta 留 tombstone（删除可经 pdsync 传播）
    OrganizationService::delete_organization_pdsync(&mut storage, &record.org_id, &admin, NOW + 1, "node-a")
        .unwrap();
    assert!(OrganizationService::get_record(&storage, &record.org_id).unwrap().is_none());
    let meta = get_personal_meta(&storage, &key).unwrap().unwrap();
    assert!(is_tombstone(&meta));
    assert_eq!(meta.vv.get("node-a"), Some(&2));
}
