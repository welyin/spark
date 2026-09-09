//! A16 切片三双写迁移单测（membership §4.4-4）：存量成员 accessKey 补齐迁移
//! （`migrate_access_key_backfill`）与名册全员映射判定（`roster_fully_mapped`
//! ——名册键切换窗口条件①的可判定形式，见 `org_member_key` 注释）。

use super::*;

use spark_core::org::access_key::{
    derive_access_key, member_org_user_id, roster_fully_mapped, verify_access_key_binding,
};
use spark_core::org::service::migrate_access_key_backfill;
use spark_core::org::types::{MemberKind, OrganizationMember, OrganizationRole, org_member_key};
use spark_core::storage::StorageBackend;

fn bare_member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role,
        joined_at: 1000,
        added_by: "creator".to_string(),
        ..Default::default()
    }
}

fn seed_of(mnemonic: &str) -> [u8; 64] {
    parse_mnemonic(mnemonic).unwrap().seed
}

/// 补齐迁移：本机在册未发布的组织 → 派生 + 写一次发布（whole 与 per-member
/// 条目双写）；他成员不动；验绑通过、org_user_id 双键可解析；幂等。
#[test]
fn backfill_publishes_self_access_key_whole_and_entry_idempotent() {
    let mut storage = MemoryStorage::new();
    // 存量形态：服务层直建（不经 kernel 自发布挂点）→ 名册无 accessKey
    let (admin, record) = setup_org(&mut storage);
    let org_id = record.org_id.clone();
    let b_root = rid('b');
    let mut whole = OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    whole.members.push(bare_member(&b_root, OrganizationRole::Member));
    OrganizationService::save_record(&mut storage, &whole).unwrap();
    assert!(!roster_fully_mapped(&whole), "存量名册未全员映射");

    let seed = seed_of(MNEMONIC);
    let backfilled = migrate_access_key_backfill(&mut storage, &seed, &admin, NOW).unwrap();
    assert_eq!(backfilled, 1, "本机在册未发布 → 补齐 1 个组织");

    // whole：accessKey 落库、验绑通过、org_user_id 与 seed/org 确定性一致
    let record = OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    let me = record.find_member(&admin).unwrap();
    let access_key = me.access_key.as_ref().expect("迁移后 accessKey 已发布");
    assert!(verify_access_key_binding(&org_id, &admin, access_key));
    let expected = derive_access_key(&seed, &org_id);
    assert_eq!(access_key, &expected, "seed 确定性派生（多设备一致）");
    let uid = member_org_user_id(access_key).unwrap();
    assert!(
        record.find_member_any_key(&uid).is_some(),
        "org_user_id 双键命中名册"
    );

    // per-member 条目双写同步补齐（装配视图权威段）
    let raw = storage
        .get(&org_member_key(&org_id, &admin))
        .unwrap()
        .expect("条目存在（create 双写）");
    let entry: OrganizationMember = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        entry.access_key.as_ref(),
        Some(&expected),
        "条目与 whole 同步补齐"
    );

    // 他成员不动（只有本人能发布自己的 accessKey）
    assert!(
        record.find_member(&b_root).unwrap().access_key.is_none(),
        "他成员条目不被代办"
    );
    // 幂等：二轮 0 写
    let backfilled = migrate_access_key_backfill(&mut storage, &seed, &admin, NOW).unwrap();
    assert_eq!(backfilled, 0, "幂等：已发布跳过");
}

/// 写一次语义：已发布的 accessKey 不被迁移覆盖（哪怕与当前 seed 派生值不同
/// ——异常态如实保留，发布面只有一个）。
#[test]
fn backfill_never_overwrites_published_key() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = record.org_id.clone();
    let foreign = derive_access_key(&[9u8; 32], &org_id);
    assert!(
        OrganizationService::publish_access_key(
            &mut storage,
            &org_id,
            &admin,
            foreign.clone(),
            NOW,
        )
        .unwrap()
    );

    let backfilled =
        migrate_access_key_backfill(&mut storage, &seed_of(MNEMONIC), &admin, NOW).unwrap();
    assert_eq!(backfilled, 0, "已发布不补齐");
    let record = OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.find_member(&admin).unwrap().access_key.as_ref(),
        Some(&foreign),
        "既有发布不被覆盖（写一次）"
    );
}

/// 多组织：本机在册的每个组织独立派生（域串含 orgId，跨组织不可关联）；
/// 非本机在册的组织跳过。
#[test]
fn backfill_spans_all_memberships_org_scoped() {
    let mut storage = MemoryStorage::new();
    let (admin, first) = setup_org(&mut storage);
    let second = OrganizationService::create_organization(&mut storage, &input(), &admin, NOW + 1)
        .unwrap();
    let outsider = root_id_of(MNEMONIC2);
    let third =
        OrganizationService::create_organization(&mut storage, &input(), &outsider, NOW + 2)
            .unwrap();

    let seed = seed_of(MNEMONIC);
    let backfilled = migrate_access_key_backfill(&mut storage, &seed, &admin, NOW).unwrap();
    assert_eq!(backfilled, 2, "两个在册组织各补齐一次");

    let uid_first = member_org_user_id(
        &OrganizationService::get_record(&storage, &first.org_id)
            .unwrap()
            .unwrap()
            .find_member(&admin)
            .unwrap()
            .access_key
            .clone()
            .unwrap(),
    )
    .unwrap();
    let uid_second = member_org_user_id(
        &OrganizationService::get_record(&storage, &second.org_id)
            .unwrap()
            .unwrap()
            .find_member(&admin)
            .unwrap()
            .access_key
            .clone()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(uid_first, uid_second, "不同组织不同 org_user_id（无关联性）");
    // 非本机在册的组织不动
    let third_record = OrganizationService::get_record(&storage, &third.org_id)
        .unwrap()
        .unwrap();
    assert!(third_record.find_member(&admin).is_none());
    assert!(
        third_record
            .find_member(&outsider)
            .unwrap()
            .access_key
            .is_none(),
        "他人组织的他人成员不被代办"
    );
}

/// `roster_fully_mapped` 真值表（切换窗口条件①）：个人成员全部可派生
/// org_user_id 才为真；kind=org 成员不参与判定（其 rootId 槽位是域身份 id）。
#[test]
fn roster_fully_mapped_truth_table() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = record.org_id.clone();

    // 本人补齐后为真（单成员组织）
    let seed = seed_of(MNEMONIC);
    migrate_access_key_backfill(&mut storage, &seed, &admin, NOW).unwrap();
    let record = OrganizationService::get_record(&storage, &org_id)
        .unwrap()
        .unwrap();
    assert!(roster_fully_mapped(&record), "全员已发布 → 真");

    // 加入未发布的个人成员 → 假
    let mut whole = record;
    whole.members.push(bare_member(&rid('b'), OrganizationRole::Member));
    assert!(!roster_fully_mapped(&whole), "存在未发布个人成员 → 假");

    // kind=org 成员（无 accessKey 义务）不影响判定
    let mut whole = whole;
    whole.members.retain(|m| m.root_id != rid('b'));
    whole.members.push(OrganizationMember {
        root_id: rid('c'),
        kind: Some(MemberKind::Org),
        ..bare_member(&rid('c'), OrganizationRole::Member)
    });
    assert!(
        roster_fully_mapped(&whole),
        "kind=org 成员不参与全员映射判定"
    );
}
