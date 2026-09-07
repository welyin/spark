//! orgsync 入站：双节点信封往返收敛 + 声明先行随流量同步。

use super::*;

// ── 1. 双节点信封往返收敛 ───────────────────────────────────────────────

#[test]
fn orgsync_hello_need_data_converges_two_nodes() {
    // A 与 B 均为组织成员，集合 all-members（复制组 = 全体成员）。
    let (a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();

    save_org(
        &mut a,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Admin),
        ],
        &[],
    );
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Admin),
        ],
        &[],
    );
    declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &a_root,
        NOW,
    );
    declare_org_collection(
        &mut b,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &b_root,
        NOW,
    );

    // A 写入一条 orgd 数据（per-node 序号：集合声明耗 seq 1，数据为 seq 2）
    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    write_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);

    // 第 1 跳：A 装配 hello 发 B → B 落后回 need
    let hello_a = build_hello_for(&a, ORG_ID, &b_root, "peer-b");
    let r = deliver_orgsync(
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
    assert_eq!(r.response, json!({ "ok": true }));
    // orgsync-out 用 body 形状区分：Need 带 knownVv，Data 带 records
    let needs: Vec<_> = r
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("knownVv").is_some())
        .collect();
    assert_eq!(needs.len(), 1, "B 落后应回 need");
    assert_eq!(needs[0].to_root_id(), &a_root);

    // 第 2 跳：B 的 need 发 A → A 采集增量 → 回 data
    let need_body = needs[0].body().clone();
    let r2 = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        need_body,
        "peer-b",
        "node-a",
    );
    assert_eq!(r2.response, json!({ "ok": true }));
    let datas: Vec<_> = r2
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("records").is_some())
        .collect();
    assert_eq!(datas.len(), 1, "A 应回 data 批次");
    assert_eq!(datas[0].to_root_id(), &b_root);

    // 第 3 跳：A 的 data 发 B → B 合入
    let data_body = datas[0].body().clone();
    let r3 = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-a",
        "node-b",
    );
    assert_eq!(r3.response, json!({ "ok": true }));
    assert_eq!(
        b.get(&data_key).unwrap().as_deref(),
        Some("\"v1\""),
        "B 合入 A 的数据"
    );

    // 两边 vv 收敛一致（node-a:2 —— 声明 seq 1 + 数据 seq 2；node-b:0）
    let meta_a = get_personal_meta(&a, &data_key).unwrap().unwrap();
    let meta_b = get_personal_meta(&b, &data_key).unwrap().unwrap();
    assert_eq!(meta_a.vv, meta_b.vv, "两端 vv 一致");
    assert_eq!(meta_a.vv.get("node-a"), Some(&2));
}

/// org-vv-fix §4.2 双端收敛（联调前置验收）：A 写 k1 → 删 k1（墓碑经 org
/// dlog 传播）→ B 收讫墓碑 → A 再写 k2 → B 经 hello→need→data 收讫 k2，
/// 两端记录集与折叠 vv（node-a 分量）收敛一致。修复前 k2 与墓碑序号碰撞，
/// 对已持墓碑的 B 永久失明。
#[test]
fn orgsync_converges_after_tombstone_then_write() {
    let (a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for (s, rid) in [(&mut a, &a_root), (&mut b, &b_root)] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Admin),
            ],
            &[],
        );
        let _ = rid;
    }
    declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &a_root,
        NOW,
    );
    declare_org_collection(
        &mut b,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &b_root,
        NOW,
    );

    let k1 = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    let k2 = format!("{}k2", org_data_prefix(ORG_ID, NAME, VERSION));

    // A 写 k1 → 删 k1（墓碑 + org dlog）
    write_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    let (_, tomb_a) = delete_org_data(&mut a, "node-a", ORG_ID, NAME, VERSION, "k1", NOW + 1);
    let tomb_seq = *tomb_a.vv.get("node-a").unwrap();

    // 第 1 轮：A hello → B 回 need → A 回 data → B 合入（收讫 k1 墓碑）
    let hello_a = build_hello_for(&a, ORG_ID, &b_root, "peer-b");
    let r = deliver_orgsync(
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
    let needs: Vec<_> = r
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("knownVv").is_some())
        .collect();
    assert_eq!(needs.len(), 1, "B 回 need");
    let r2 = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        needs[0].body().clone(),
        "peer-b",
        "node-a",
    );
    let datas: Vec<_> = r2
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("records").is_some())
        .collect();
    assert!(!datas.is_empty(), "A 回 data");
    for d in &datas {
        let r3 = deliver_orgsync(
            &mut b,
            &b_root,
            "B",
            &a_key,
            &a_root,
            &b_root,
            dm_envelope::KIND_ORGSYNC_DATA,
            d.body().clone(),
            "peer-a",
            "node-b",
        );
        assert_eq!(r3.response, json!({ "ok": true }));
    }
    // B 端 k1 墓碑已落（B 折叠 vv 含墓碑序号）
    let b_tomb = get_personal_meta(&b, &k1)
        .unwrap()
        .expect("B 端 k1 墓碑 pmeta");
    assert!(is_tombstone(&b_tomb), "B 收讫 k1 墓碑");
    assert_eq!(b_tomb.vv.get("node-a"), Some(&tomb_seq));
    assert!(b.get(&k1).unwrap().is_none(), "B 端 k1 本体已删");

    // A 受理删除后再写 k2（修复前与墓碑同序号碰撞 → 对 B 失明）
    write_org_data(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        "k2",
        "\"v2\"",
        NOW + 2,
    );
    let a_k2 = get_personal_meta(&a, &k2).unwrap().unwrap();
    assert!(
        a_k2.vv.get("node-a").unwrap() > &tomb_seq,
        "k2 序号 > 墓碑序号（无碰撞）"
    );

    // 第 2 轮：A hello → B（折叠已含墓碑序号）回 need → A 回 data → B 合入
    let hello_a2 = build_hello_for(&a, ORG_ID, &b_root, "peer-b");
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_a2,
        "peer-a",
        "node-b",
    );
    let needs: Vec<_> = r
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("knownVv").is_some())
        .collect();
    assert_eq!(needs.len(), 1, "B 回 need");
    let r2 = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        needs[0].body().clone(),
        "peer-b",
        "node-a",
    );
    let datas: Vec<_> = r2
        .orgsync_out
        .iter()
        .filter(|o| o.body().get("records").is_some())
        .collect();
    assert!(
        datas.iter().any(|d| d.body()["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rec| rec["key"].as_str() == Some(k2.as_str()))),
        "A 的 data 批次必含 k2（修复前 Equal 跳过 → 永久失明）"
    );
    for d in &datas {
        deliver_orgsync(
            &mut b,
            &b_root,
            "B",
            &a_key,
            &a_root,
            &b_root,
            dm_envelope::KIND_ORGSYNC_DATA,
            d.body().clone(),
            "peer-a",
            "node-b",
        );
    }

    // 收敛断言：B 收到 k2，两端记录集与折叠 vv（node-a 分量）一致
    assert_eq!(b.get(&k2).unwrap().as_deref(), Some("\"v2\""), "B 合入 k2");
    let b_k2 = get_personal_meta(&b, &k2).unwrap().unwrap();
    assert_eq!(b_k2.vv, a_k2.vv, "两端 k2 vv 一致");
    let fold_a =
        spark_core::sync::orgsync::collect_org_collection_vv(&a, ORG_ID, NAME, VERSION).unwrap();
    let fold_b =
        spark_core::sync::orgsync::collect_org_collection_vv(&b, ORG_ID, NAME, VERSION).unwrap();
    assert_eq!(
        fold_a.get("node-a"),
        fold_b.get("node-a"),
        "两端折叠 vv 的 node-a 分量收敛一致（{fold_a:?} vs {fold_b:?}）"
    );
}

// ── 7. 声明先行：org:coll: 随流量同步 + 冲突声明拒绝 ───────────────────

#[test]
fn org_declaration_syncs_and_conflicts_rejected() {
    let (a_key, a_root) = self_identity(1);
    let (_self_key, self_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();

    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (self_root.as_str(), OrganizationRole::Admin),
            ],
            &[],
        );
    }
    // A 声明集合（声明记录 + pmeta 在 A 侧）
    let decl = declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &a_root,
        NOW,
    );
    let decl_key = org_decl_key(ORG_ID, NAME, VERSION);

    // A 装配 orgsync-data 携带声明记录（声明先行），发 B
    let decl_record = spark_core::sync::orgsync::OrgsyncRecord {
        key: decl_key.clone(),
        value: serde_json::to_value(&decl).unwrap(),
        meta: get_personal_meta(&a, &decl_key).unwrap().unwrap(),
        dseq: None,
    };
    let data_body =
        build_orgsync_data_batch(ORG_ID, &format!("{NAME}@v{VERSION}"), &[decl_record], 0, 1);
    let r = deliver_orgsync(
        &mut b,
        &self_root,
        "B",
        &a_key,
        &a_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-a",
        "node-b",
    );
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "声明记录 org:coll: 白名单放行"
    );

    // B 合入声明 → resolve_org 可读
    let resolved = spark_core::plugindata::resolve_org(&b, ORG_ID, NAME, Some(VERSION)).unwrap();
    assert_eq!(resolved.name, NAME);
    assert_eq!(resolved.accounts, Accounts::AllMembers, "B 读到声明 axes");
    assert_eq!(resolved.org_id.as_deref(), Some(ORG_ID));

    // B 侧代际内冲突声明被拒绝（同 name@version，策略不同）
    let conflict = declare(
        &mut b,
        "ai-chat",
        DeclareInput {
            name: NAME.to_string(),
            version: Some(VERSION.to_string()),
            space: Some(Space::Org),
            accounts: Some(Accounts::DataAccounts), // 冲突：accounts 不同
            scope: Some(spark_core::plugindata::Scope::Sync),
            declared_by: Some(self_root.clone()),
            ..Default::default()
        },
        NOW,
        Some(ORG_ID),
    );
    assert!(conflict.is_err(), "代际内冲突声明应被拒绝");
    let err = conflict.unwrap_err();
    assert!(
        err.to_string().contains("already declared"),
        "冲突文案指明既有声明：{err}"
    );
}
