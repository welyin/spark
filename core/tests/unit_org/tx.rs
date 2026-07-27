use spark_core::org::tx::*;
use spark_core::storage::{MemoryStorage, StorageBackend};

fn rid(ch: char) -> String {
    ch.to_string().repeat(64)
}

fn tx(org_id: &str, created_at: i64) -> OrganizationTransactionRecord {
    OrganizationTransactionRecord {
        tx_id: String::new(),
        org_id: org_id.to_string(),
        type_: OrganizationTransactionType::MemberAdd,
        created_at,
        actor_root_id: rid('a'),
        target_root_id: Some(rid('b')),
        summary: "添加成员".to_string(),
        payload: None,
    }
}

#[test]
fn append_generates_tx_id_and_persists() {
    let mut storage = MemoryStorage::new();
    let appended = append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
    assert_eq!(appended.tx_id.len(), 16);
    assert!(appended.tx_id.bytes().all(|b| b.is_ascii_hexdigit()));
    let key = organization_transaction_key("org_x", 1000, &appended.tx_id);
    assert_eq!(key, format!("org:tx:org_x:1000:{}", appended.tx_id));
    let raw = storage.get(&key).unwrap().unwrap();
    let parsed: OrganizationTransactionRecord = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed, appended);
    // 无 targetRootId/payload 时丢键（对齐 TS 可选字段）
    let sparse = OrganizationTransactionRecord {
        target_root_id: None,
        ..tx("org_x", 1001)
    };
    let json = serde_json::to_string(&sparse).unwrap();
    assert!(!json.contains("targetRootId"));
    assert!(!json.contains("payload"));
}

#[test]
fn list_reverse_chronological_with_limit() {
    let mut storage = MemoryStorage::new();
    for created_at in [1000, 3000, 2000, 5000, 4000] {
        append_organization_transaction(&mut storage, tx("org_x", created_at)).unwrap();
    }
    // 其他组织的事务不被扫到
    append_organization_transaction(&mut storage, tx("org_y", 9999)).unwrap();

    let all = list_organization_transactions(&storage, "org_x", 20).unwrap();
    let times: Vec<i64> = all.iter().map(|t| t.created_at).collect();
    assert_eq!(times, vec![5000, 4000, 3000, 2000, 1000]);

    let top2 = list_organization_transactions(&storage, "org_x", 2).unwrap();
    assert_eq!(top2.len(), 2);
    assert_eq!(top2[0].created_at, 5000);
    assert_eq!(top2[1].created_at, 4000);
}

#[test]
fn list_skips_corrupted_rows() {
    let mut storage = MemoryStorage::new();
    append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
    storage.put("org:tx:org_x:2000:bad", "{not json").unwrap();
    let list = list_organization_transactions(&storage, "org_x", 20).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].created_at, 1000);
}

#[test]
fn latest_version() {
    let mut storage = MemoryStorage::new();
    assert_eq!(
        get_latest_organization_transaction_version(&storage, "org_x").unwrap(),
        0
    );
    append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
    append_organization_transaction(&mut storage, tx("org_x", 3000)).unwrap();
    append_organization_transaction(&mut storage, tx("org_x", 2000)).unwrap();
    assert_eq!(
        get_latest_organization_transaction_version(&storage, "org_x").unwrap(),
        3000
    );
    // 首条损坏 → 0
    storage.put("org:tx:org_x:9999:bad", "oops").unwrap();
    assert_eq!(
        get_latest_organization_transaction_version(&storage, "org_x").unwrap(),
        0
    );
}
