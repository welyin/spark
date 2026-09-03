//! 阶段四A（org-member-split）单测：P1 create/delete 入口双写、装配视图
//! （P1-b 读切换）与回滚开关、存量迁移幂等/半态扫尾；P2 成员自写条目
//! （claim 退役取代通道）、投影并发合并口径（P1 挂账收口）、本地擦除。

use super::*;

use spark_core::org::service::{migrate_org_members_split, set_member_read_assembled};
use spark_core::org::types::{
    ORG_MEMBER_PREFIX, OrganizationMember, OrganizationRole, org_member_key,
};
use spark_core::storage::{ScanOptions, StorageBackend};

fn bare_member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role,
        joined_at: 1000,
        added_by: "creator".to_string(),
        ..Default::default()
    }
}

/// READ_ASSEMBLED 是进程级全局开关——本文件两个「装配发散」用例（条目覆盖/
/// 墓碑排除 与 开关回滚）在测试线程并行下必须互斥，否则开关翻转窗口可
/// 击穿另一用例的装配断言（间歇失败）。
static ASSEMBLY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// P1-a create/delete 入口双写：create 落初始成员（creator）条目；delete
/// 连同成员条目一并删除。
#[test]
fn create_and_delete_dual_write_member_entries() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    // create：初始成员条目已写，内容与 whole.members[0] 逐字段等价
    let entry_key = org_member_key(&record.org_id, &admin);
    let raw = storage.get(&entry_key).unwrap().expect("初始成员条目已双写");
    assert_eq!(
        raw,
        serde_json::to_string(&record.members[0]).unwrap(),
        "条目与 whole 同名成员逐字段等价"
    );
    // delete：组织记录与成员条目一并删除
    OrganizationService::delete_organization(&mut storage, &record.org_id, &admin, NOW + 1)
        .unwrap();
    assert!(
        storage.get(&entry_key).unwrap().is_none(),
        "成员条目随组织删除"
    );
    let leftover = storage
        .scan(&ScanOptions::prefix(ORG_MEMBER_PREFIX))
        .unwrap();
    assert!(leftover.is_empty(), "无残留成员条目");
}

/// P1-b 装配视图：成员条目逐 rootId 覆盖 whole 同名成员（条目为权威）；
/// whole 独有成员保留（迁移窗口/混跑）；墓碑条目排除。
#[test]
fn assembled_view_overrides_whole_and_excludes_tombstones() {
    let _guard = ASSEMBLY_LOCK.lock().unwrap();
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    let b_root = rid('b');
    // whole 里补一个成员 B（模拟旧端只有 whole 的形态），并清掉 create 双写
    // 的 A 条目（隔离变量：本用例聚焦条目覆盖/排除语义）
    let mut whole = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    whole.members.push(bare_member(&b_root, OrganizationRole::Member));
    OrganizationService::save_record(&mut storage, &whole).unwrap();
    storage.delete(&org_member_key(org_id, &admin)).unwrap();

    // 1. 无 B 条目时：whole 读原样（whole 独有成员保留）
    let view = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    assert!(view.find_member(&b_root).is_some(), "whole 独有成员保留");
    assert!(
        view.find_member(&b_root).unwrap().nickname.is_none(),
        "无条目时 whole 字段原样"
    );

    // 2. B 条目（nickname 改过）→ 逐 rootId 覆盖 whole 同名成员
    let mut b_entry = bare_member(&b_root, OrganizationRole::Member);
    b_entry.nickname = Some("条目昵称".to_string());
    storage
        .put(&org_member_key(org_id, &b_root), &serde_json::to_string(&b_entry).unwrap())
        .unwrap();
    let view = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    assert_eq!(
        view.find_member(&b_root).unwrap().nickname.as_deref(),
        Some("条目昵称"),
        "条目覆盖 whole 同名成员"
    );
    assert!(view.find_member(&admin).is_some(), "whole 独有的 A 仍保留");

    // 3. B 条目墓碑（值删除 + pmeta 墓碑）→ 装配排除
    storage.delete(&org_member_key(org_id, &b_root)).unwrap();
    spark_core::sync::set_personal_meta(
        &mut storage,
        &org_member_key(org_id, &b_root),
        &spark_core::sync::meta::DocMeta {
            vv: Default::default(),
            ts: NOW,
            node_id: None,
            tombstone: Some(true),
        },
    )
    .unwrap();
    let view = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    assert!(
        view.find_member(&b_root).is_none(),
        "墓碑条目从装配视图排除"
    );
}

/// P1-b 回滚开关：关开关 → 读回 whole 记录（成员条目残留无害）；开开关 →
/// 装配视图。开关翻转紧贴本用例窗口（全局 AtomicBool，测试进程内并行——
/// 与另一装配发散用例经 `ASSEMBLY_LOCK` 互斥）。
#[test]
fn read_switch_rolls_back_to_whole() {
    let _guard = ASSEMBLY_LOCK.lock().unwrap();
    let mut storage = MemoryStorage::new();
    let (_admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    let b_root = rid('b');
    // whole 不含 B，仅条目含 B（迁移窗口形态）
    let b_entry = bare_member(&b_root, OrganizationRole::Member);
    storage
        .put(&org_member_key(org_id, &b_root), &serde_json::to_string(&b_entry).unwrap())
        .unwrap();
    assert!(
        OrganizationService::get_record(&storage, org_id)
            .unwrap()
            .unwrap()
            .find_member(&b_root)
            .is_some(),
        "开关开：装配视图含条目成员"
    );
    set_member_read_assembled(false);
    let whole_view = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    set_member_read_assembled(true); // 立即恢复（缩小全局翻转窗口）
    assert!(
        whole_view.find_member(&b_root).is_none(),
        "开关关：回滚读 whole（条目成员不可见，残留无害）"
    );
    // 恢复后装配视图复现
    assert!(
        OrganizationService::get_record(&storage, org_id)
            .unwrap()
            .unwrap()
            .find_member(&b_root)
            .is_some(),
        "开关恢复：装配视图复原"
    );
}

/// P1 存量迁移（设计 §2.4）：逐成员写 org:member 条目；幂等（二轮 0 写）；
/// 半态扫尾（删掉一条后下轮补齐）；**不动 org:meta 的 members 段**。
#[test]
fn migrate_org_members_split_idempotent_and_sweeps_half_state() {
    let mut storage = MemoryStorage::new();
    // 未迁移形态：只有 whole（save_record 直写，无双写条目）
    let (admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    storage.delete(&org_member_key(org_id, &admin)).unwrap();
    let b_root = rid('b');
    let mut whole = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    whole.members.push(bare_member(&b_root, OrganizationRole::Member));
    OrganizationService::save_record(&mut storage, &whole).unwrap();
    let whole_bytes_before = storage
        .get(&spark_core::org::types::organization_key(org_id))
        .unwrap()
        .unwrap();

    // 首轮：补齐 2 条
    let written = migrate_org_members_split(&mut storage).unwrap();
    assert_eq!(written, 2, "首轮逐成员写条目");
    for m in &whole.members {
        let raw = storage
            .get(&org_member_key(org_id, &m.root_id))
            .unwrap()
            .expect("迁移写出条目");
        assert_eq!(raw, serde_json::to_string(m).unwrap(), "条目与 whole 成员等价");
    }
    // 幂等：二轮 0 写
    let written = migrate_org_members_split(&mut storage).unwrap();
    assert_eq!(written, 0, "幂等：已存在跳过");
    // 半态扫尾：删掉一条（模拟迁移中断半态）→ 下轮补齐
    storage.delete(&org_member_key(org_id, &b_root)).unwrap();
    let written = migrate_org_members_split(&mut storage).unwrap();
    assert_eq!(written, 1, "半态扫尾只补缺失条目");
    // org:meta 的 members 段不动（双写期保留，旧端无感）
    let whole_bytes_after = storage
        .get(&spark_core::org::types::organization_key(org_id))
        .unwrap()
        .unwrap();
    assert_eq!(whole_bytes_before, whole_bytes_after, "迁移不动 org:meta");
}

// ── 阶段四A P2 ─────────────────────────────────────────────────────

/// P2 L2：成员自写条目（claim 退役取代通道）——端点 upsert 进自己的
/// org:member 条目；**不动 whole**（whole 的 members 段保持原值，装配视图
/// 以条目为权威对外呈现新端点）；无变化幂等不写；非成员/无记录无操作。
#[test]
fn upsert_own_member_entry_entry_only_write() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    let endpoint = spark_core::org::types::OrganizationNodeInfo {
        device_uid: Some("uid-1".to_string()),
        peer_id: Some("peer-new".to_string()),
        addresses: vec!["/ip4/1.1.1.1/tcp/1".to_string()],
    };
    // 自写：条目出现且含端点
    let changed =
        OrganizationService::upsert_own_member_entry(&mut storage, org_id, &admin, &endpoint)
            .unwrap();
    assert!(changed, "端点新增有写");
    let raw = storage
        .get(&org_member_key(org_id, &admin))
        .unwrap()
        .unwrap();
    let entry: OrganizationMember = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        entry.node_info.as_ref().unwrap().iter().next().unwrap().peer_id.as_deref(),
        Some("peer-new")
    );
    // whole 不动（条目单写口径）
    let whole = OrganizationService::get_record(&storage, org_id).unwrap().unwrap();
    let whole_raw = storage
        .get(&spark_core::org::types::organization_key(org_id))
        .unwrap()
        .unwrap();
    assert!(
        !whole_raw.contains("peer-new"),
        "whole 的 members 段不被自写触碰"
    );
    // 装配视图对外可见新端点（条目权威覆盖 whole）
    assert!(
        whole
            .find_member(&admin)
            .unwrap()
            .node_info
            .as_ref()
            .is_some_and(|set| set.iter().any(|e| e.peer_id.as_deref() == Some("peer-new")))
    );
    // 幂等：同端点再写无变化
    let changed =
        OrganizationService::upsert_own_member_entry(&mut storage, org_id, &admin, &endpoint)
            .unwrap();
    assert!(!changed, "端点无变化幂等不写");
    // 非成员无操作
    let changed =
        OrganizationService::upsert_own_member_entry(&mut storage, org_id, &rid('z'), &endpoint)
            .unwrap();
    assert!(!changed, "非成员不写");
}

/// P2 投影口径重审（P1 挂账收口）：whole 合入投影遇**并发**条目（成员自写
/// 的更新 whole 尚未合入）→ 条目级合并而非覆盖——自写的本人字段组存活，
/// whole 侧新值按秩生效；合并 pmeta vv 支配双输入。
#[test]
fn projection_concurrent_merges_instead_of_clobbering() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    // 成员自写：条目带新昵称 + 端点（本机分量 node-m，ts 高）
    let mut entry = bare_member(&admin, OrganizationRole::Admin);
    entry.nickname = Some("自写昵称".to_string());
    entry.extra.insert("self".to_string(), serde_json::json!(1));
    let key = org_member_key(org_id, &admin);
    spark_core::sync::set_personal_meta(
        &mut storage,
        &key,
        &spark_core::sync::meta::DocMeta {
            vv: [("node-m".to_string(), 3)].into_iter().collect(),
            ts: 9000,
            node_id: None,
            tombstone: None,
        },
    )
    .unwrap();
    storage.put(&key, &serde_json::to_string(&entry).unwrap()).unwrap();

    // whole 侧（旧端/管理员路径）：同成员 role 段更新但无自写字段（vv
    // node-a 与条目并发，ts 低）
    let mut whole_member = bare_member(&admin, OrganizationRole::Admin);
    whole_member.added_by = "admin-2".to_string();
    let whole_meta = spark_core::sync::meta::DocMeta {
        vv: [("node-a".to_string(), 7)].into_iter().collect(),
        ts: 8000,
        node_id: Some("node-a".to_string()),
        tombstone: None,
    };
    let written = spark_core::org::service::project_member_entries_from_whole(
        &mut storage,
        org_id,
        &[whole_member.clone()],
        &whole_meta,
    )
    .unwrap();
    assert_eq!(written, 1, "并发投影产生一次合并写");
    let merged: OrganizationMember =
        serde_json::from_str(&storage.get(&key).unwrap().unwrap()).unwrap();
    // 条目 ts 高（9000 > 8000）→ 秩高侧 = 条目：本人字段组/管理员字段组
    // 均取条目侧；whole 侧 ts 低被整组覆盖——但 whole 的 added_by 变更丢失
    // 是字段组级秩选取的既有口径（merge_member_record 同语义）
    assert_eq!(merged.nickname.as_deref(), Some("自写昵称"), "自写本人字段组存活");
    assert_eq!(merged.extra["self"], serde_json::json!(1), "extra 并集保留");
    let meta = spark_core::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
    assert_eq!(meta.vv.get("node-m"), Some(&3), "合并 vv 支配双输入");
    assert_eq!(meta.vv.get("node-a"), Some(&7));
    assert_eq!(meta.ts, 9000, "ts 取大");
}

/// P2 投影口径反向：条目自写 ts **低**于 whole（whole 是更新的一方）→
/// 合并取 whole 侧字段组（混跑期旧端 whole 更新不被陈旧条目盖住）。
#[test]
fn projection_concurrent_whole_side_newer_wins_groups() {
    let mut storage = MemoryStorage::new();
    let (admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    // 陈旧条目（无昵称，ts 低）
    let entry = bare_member(&admin, OrganizationRole::Admin);
    let key = org_member_key(org_id, &admin);
    spark_core::sync::set_personal_meta(
        &mut storage,
        &key,
        &spark_core::sync::meta::DocMeta {
            vv: [("node-m".to_string(), 1)].into_iter().collect(),
            ts: 1000,
            node_id: None,
            tombstone: None,
        },
    )
    .unwrap();
    storage.put(&key, &serde_json::to_string(&entry).unwrap()).unwrap();
    // whole 侧更新（昵称，ts 高，vv 与条目并发）
    let mut whole_member = bare_member(&admin, OrganizationRole::Admin);
    whole_member.nickname = Some("whole 新昵称".to_string());
    let whole_meta = spark_core::sync::meta::DocMeta {
        vv: [("node-a".to_string(), 7)].into_iter().collect(),
        ts: 8000,
        node_id: None,
        tombstone: None,
    };
    spark_core::org::service::project_member_entries_from_whole(
        &mut storage,
        org_id,
        &[whole_member],
        &whole_meta,
    )
    .unwrap();
    let merged: OrganizationMember =
        serde_json::from_str(&storage.get(&key).unwrap().unwrap()).unwrap();
    assert_eq!(
        merged.nickname.as_deref(),
        Some("whole 新昵称"),
        "whole ts 高 → 其字段组生效（陈旧条目不盖新 whole）"
    );
}

/// P2 L3：wipe_org_local —— whole + 成员条目（值与 pmeta）+ orgq 现场
/// 一并擦除，幂等。
#[test]
fn wipe_org_local_clears_everything_idempotent() {
    let mut storage = MemoryStorage::new();
    let (_admin, record) = setup_org(&mut storage);
    let org_id = &record.org_id;
    // 造 orgq 现场
    storage
        .put(
            &spark_core::sync::orgsync::orgq_cache_key(org_id, "c@v1", "k1"),
            "\"v\"",
        )
        .unwrap();
    let wiped = spark_core::org::service::wipe_org_local(&mut storage, org_id, "node-t", 1000).unwrap();
    assert_eq!(wiped, 1, "create 双写的初始条目被擦");
    assert!(
        OrganizationService::get_record(&storage, org_id).unwrap().is_none(),
        "whole 擦除"
    );
    let entry_key = org_member_key(org_id, &_admin);
    assert!(storage.get(&entry_key).unwrap().is_none(), "条目值擦除");
    assert!(
        storage
            .get(&spark_core::sync::personal_meta_key(&entry_key))
            .unwrap()
            .is_none(),
        "条目 pmeta 擦除"
    );
    assert!(
        storage
            .get(&spark_core::sync::orgsync::orgq_cache_key(org_id, "c@v1", "k1"))
            .unwrap()
            .is_none(),
        "orgq 现场擦除"
    );
    // 幂等
    let wiped = spark_core::org::service::wipe_org_local(&mut storage, org_id, "node-t", 1000).unwrap();
    assert_eq!(wiped, 0);
}
