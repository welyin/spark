//! orgsync 入站：GC 等待集合、墓碑接力 A→B→C、删除后重建不误推墓碑。

use super::*;

// ── 4. GC 等待集合：部分推进阻塞、全部推进清理 ─────────────────────────

#[test]
fn org_dlog_gc_blocks_until_all_replication_members_advance() {
    // 复制组 3 账号（all-members）：A（本机/GC 发起者）、B、C。
    let (b_key, b_root) = self_identity(2);
    let (c_key, c_root) = self_identity(3);
    let (_a_key, a_root) = self_identity(1);
    let mut a = MemoryStorage::new();

    save_org(
        &mut a,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Admin),
            (c_root.as_str(), OrganizationRole::Admin),
        ],
        &[],
    );
    declare_org_collection(&mut a, "node-a", ORG_ID, NAME, VERSION, Accounts::AllMembers, &a_root, NOW);

    // A 写入两条数据后删除 → A 的 org dlog 两条墓碑条目（seq 1,2）
    write_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    write_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k2", "\"v2\"", NOW + 1);
    let (seq1, _) = delete_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", NOW + 2);
    let (seq2, _) = delete_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k2", NOW + 3);
    assert_eq!(seq1, 1);
    assert_eq!(seq2, 2);
    assert_eq!(org_dlog_entries_after(&a, ORG_ID, NAME, VERSION, 0).unwrap().len(), 2);

    // 复制组成员 = [A,B,C]，等待集合 = 除 A 外的 [B, C]。
    let members = vec![
        a_root.clone(),
        b_root.clone(),
        c_root.clone(),
    ];

    // 仅 B 推进水位（B 发 hello 带 dlogAck=2，B 已确认 A 的删除日志到 seq2）
    // → C 无水位记录 → 阻塞不清
    let hello_b = hello_with_dlog_ack(2);
    let _r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_b,
        "peer-b",
        "node-a",
    );
    let threshold = spark_core::sync::orgsync::org_dlog_gc_threshold(
        &a,
        ORG_ID,
        NAME,
        VERSION,
        &members,
        &a_root,
    )
    .unwrap();
    assert_eq!(threshold, 0, "C 无水位记录 → 阻塞不清");
    assert_eq!(
        org_dlog_entries_after(&a, ORG_ID, NAME, VERSION, 0).unwrap().len(),
        2,
        "条目保留（未清理）"
    );

    // C 也推进水位（dlogAck=2）→ 全部推进 → threshold=2 → 清理生效
    let hello_c = hello_with_dlog_ack(2);
    let _r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &c_key,
        &c_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_c,
        "peer-c",
        "node-a",
    );
    let threshold = spark_core::sync::orgsync::org_dlog_gc_threshold(
        &a,
        ORG_ID,
        NAME,
        VERSION,
        &members,
        &a_root,
    )
    .unwrap();
    assert_eq!(threshold, 2, "全部成员推进 → threshold=2");
    // wm 键按 (rootId, peerId) 设备粒度
    assert!(
        a.get(&spark_core::plugindata::org_dlog_wm_key(ORG_ID, NAME, VERSION, &b_root, "peer-b"))
            .unwrap()
            .is_some(),
        "B 水位已按 (rootId,peerId) 记录"
    );
    assert!(
        a.get(&spark_core::plugindata::org_dlog_wm_key(ORG_ID, NAME, VERSION, &c_root, "peer-c"))
            .unwrap()
            .is_some(),
        "C 水位已按 (rootId,peerId) 记录"
    );
    // 清理生效：C 的 hello 触发的 hello 处理器内 GC 已按 threshold=2 清掉 seq<=2
    // （`org_dlog_gc` 在处理器内随水位推进自动执行，无需测试侧手动触发）。
    assert_eq!(
        org_dlog_entries_after(&a, ORG_ID, NAME, VERSION, 0).unwrap().len(),
        0,
        "清理生效：seq<=2 两条条目被删"
    );
}

// ── 5. 墓碑接力 A→B→C ──────────────────────────────────────────────────

#[test]
fn org_tombstone_relays_a_to_b_to_c_without_personal_dlog_pollution() {
    let (a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let (c_key, c_root) = self_identity(3);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    let mut c = MemoryStorage::new();

    for s in [&mut a, &mut b, &mut c] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Admin),
                (c_root.as_str(), OrganizationRole::Admin),
            ],
            &[],
        );
    }
    // 各端声明集合（声明随流量同步，但此测试聚焦墓碑接力，端侧预声明）
    declare_org_collection(&mut a, "node-a", ORG_ID, NAME, VERSION, Accounts::AllMembers, &a_root, NOW);
    declare_org_collection(&mut b, "node-b", ORG_ID, NAME, VERSION, Accounts::AllMembers, &b_root, NOW);
    declare_org_collection(&mut c, "node-c", ORG_ID, NAME, VERSION, Accounts::AllMembers, &c_root, NOW);

    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    write_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    // A 删除 → A org dlog seq1 墓碑
    delete_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", NOW + 1);

    // A → B：hello → B need → A 回 data（含墓碑）→ B 合入墓碑
    let hello_a = build_hello_for(&a, ORG_ID, &b_root, "peer-b");
    let rb = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_a,
        "peer-a",
        "node-b",
    );
    let need_b = rb
        .orgsync_out
        .iter()
        .find(|o| o.body().get("knownVv").is_some())
        .expect("B 落后回 need");
    let ra = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        need_b.body().clone(),
        "peer-b",
        "node-a",
    );
    let data_a = ra
        .orgsync_out
        .iter()
        .find(|o| o.body().get("records").is_some())
        .expect("A 回 data");
    // A 回的数据必须含墓碑（data 应带 dseq 的墓碑记录）
    let (_, _, recs) = spark_core::sync::orgsync::parse_orgsync_data(data_a.body()).unwrap();
    assert!(
        recs.iter().any(|r| r.dseq.is_some()),
        "A 回 data 携带墓碑（dseq）"
    );
    deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_a.body().clone(),
        "peer-a",
        "node-b",
    );

    // B 合入墓碑：本体删、pmeta 墓碑
    assert!(b.get(&data_key).unwrap().is_none(), "B 删除本体");
    let b_meta = get_personal_meta(&b, &data_key).unwrap().unwrap();
    assert!(is_tombstone(&b_meta), "B pmeta 墓碑");
    // B 的 org dlog 有条目（接力）
    let b_org = org_dlog_entries_after(&b, ORG_ID, NAME, VERSION, 0).unwrap();
    assert_eq!(b_org.len(), 1, "B org dlog 有条目");
    assert_eq!(b_org[0].1, data_key);
    // B 个人域 dlog（dlog:entry:）无 orgd 污染
    let b_personal: Vec<_> = b
        .scan(&ScanOptions::prefix("dlog:entry:"))
        .unwrap()
        .into_iter()
        .collect();
    assert!(b_personal.is_empty(), "B 个人域 dlog 无 orgd 污染");

    // B → C：中继墓碑
    let hello_b = build_hello_for(&b, ORG_ID, &c_root, "peer-c");
    let rc = deliver_orgsync(
        &mut c,
        &c_root,
        "C",
        &b_key,
        &b_root,
        &c_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_b,
        "peer-b",
        "node-c",
    );
    let need_c = rc
        .orgsync_out
        .iter()
        .find(|o| o.body().get("knownVv").is_some())
        .expect("C 落后回 need");
    let rb2 = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &c_key,
        &c_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        need_c.body().clone(),
        "peer-c",
        "node-b",
    );
    let data_b = rb2
        .orgsync_out
        .iter()
        .find(|o| o.body().get("records").is_some())
        .expect("B 回 data");
    let (_, _, recs_c) = spark_core::sync::orgsync::parse_orgsync_data(data_b.body()).unwrap();
    assert!(
        recs_c.iter().any(|r| r.dseq.is_some()),
        "B 向 C 中继墓碑"
    );
    deliver_orgsync(
        &mut c,
        &c_root,
        "C",
        &b_key,
        &b_root,
        &c_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_b.body().clone(),
        "peer-b",
        "node-c",
    );
    assert!(c.get(&data_key).unwrap().is_none(), "C 也删除本体");
    let c_meta = get_personal_meta(&c, &data_key).unwrap().unwrap();
    assert!(is_tombstone(&c_meta), "C pmeta 墓碑");
}

// ── 6. 删除后重建不误推墓碑 ────────────────────────────────────────────

#[test]
fn collect_org_tombstones_after_skips_rebuilt_key() {
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![(self_identity(1).1.as_str(), OrganizationRole::Admin)],
        &[],
    );
    declare_org_collection(&mut s, "node-a", ORG_ID, NAME, VERSION, Accounts::AllMembers, &self_identity(1).1, NOW);
    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));

    // 写入 → 删除（seq1 墓碑）→ 重建（新 pmeta 非墓碑）
    write_org_data(&mut s, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    delete_org_data(&mut s, "node-a", ORG_ID, NAME, VERSION, "k1", NOW + 1);
    write_org_data(&mut s, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v2-rebuilt\"", NOW + 2);

    let meta = get_personal_meta(&s, &data_key).unwrap().unwrap();
    assert!(!is_tombstone(&meta), "重建后 pmeta 非墓碑");
    assert_eq!(meta.vv.get("node-a"), Some(&4), "重建 bump 到 node-a:4（声明1+写2+删3+重建4）");
    assert_eq!(
        s.get(&data_key).unwrap().as_deref(),
        Some("\"v2-rebuilt\""),
        "重建本体在"
    );

    // dlog 仍有一条旧墓碑条目（seq1），但其 key 的 pmeta 已非墓碑 → 不误推
    let tombs = collect_org_tombstones_after(&s, ORG_ID, NAME, VERSION, 0).unwrap();
    assert!(
        tombs.iter().all(|t| t.key != data_key),
        "重建后的 key 不得作为墓碑补推"
    );

    // 增量采集（knownVv 空）→ 该 key 作为普通数据（非墓碑）随增量走
    let inc = collect_org_incremental(&s, ORG_ID, NAME, VERSION, &Default::default(), 0).unwrap();
    let rec = inc.iter().find(|r| r.key == data_key).expect("重建记录在增量中");
    assert!(rec.dseq.is_none(), "重建记录不带墓碑 dseq");
}
