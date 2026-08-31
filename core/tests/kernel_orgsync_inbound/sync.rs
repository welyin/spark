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
    declare_org_collection(&mut a, "node-a", ORG_ID, NAME, VERSION, Accounts::AllMembers, &a_root, NOW);
    declare_org_collection(&mut b, "node-b", ORG_ID, NAME, VERSION, Accounts::AllMembers, &b_root, NOW);

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
    let needs: Vec<_> = r.orgsync_out.iter().filter(|o| o.body().get("knownVv").is_some()).collect();
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
    let datas: Vec<_> = r2.orgsync_out.iter().filter(|o| o.body().get("records").is_some()).collect();
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

// ── R3. acl 走 org:structure（all-members）全员可达 ─────────────────────

/// R3：acl（org:acl:）是 all-members 系统数据（§20.7），复制组=全体成员——
/// 经 org:structure 集合流量同步（而非随 encrypted 集合的 data-accounts
/// 复制组流动）。普通成员读者（非数据账号）因此能收到 acl，orgkey-deliver
/// 入站可验 sender∈owners。
#[test]
fn acl_travels_via_all_members_org_structure_to_regular_member() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use spark_core::identity::derive_domain_identity;
    use spark_core::org::types::OrganizationAccessKey;
    use spark_core::plugindata::Accounts;

    let (a_key, a_root) = self_identity(1);
    let (_, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    let enc_name = "ai-chat:secret";
    let enc_version = "1.0.0";

    // A = 数据账号（owner），B = 普通成员（非数据账号，reader）
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Member),
            ],
            &[a_root.as_str()],
        );
    }
    // A 声明 encrypted 集合（data-accounts 复制组——普通成员不在组内）
    declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        enc_name,
        enc_version,
        Accounts::DataAccounts,
        &a_root,
        NOW,
    );
    // B 也声明同集合（读到声明即可；B 不是数据账号）
    declare_org_collection(
        &mut b,
        "node-b",
        ORG_ID,
        enc_name,
        enc_version,
        Accounts::DataAccounts,
        &a_root,
        NOW,
    );

    // A 发布本人 accessKey（acl 签名者）。B 侧也需持有 A 的 accessKey（acl
    // 验签取 signer=from 的成员表 accessKey 公钥）。
    let owner_domain = derive_domain_identity(&[7u8; 64], &format!("org-access:{ORG_ID}"));
    let owner_pk_b64 = B64.encode(owner_domain.public_key());
    let owner_ak = OrganizationAccessKey {
        public_key: owner_pk_b64.clone(),
        bind_sig: "bind".to_string(),
    };
    {
        let mut rec = OrganizationService::get_record(&mut a, ORG_ID).unwrap().unwrap();
        if let Some(m) = rec.members.iter_mut().find(|m| m.root_id == a_root) {
            m.access_key = Some(owner_ak.clone());
        }
        OrganizationService::save_record(&mut a, &rec).unwrap();
    }
    {
        let mut rec = OrganizationService::get_record(&mut b, ORG_ID).unwrap().unwrap();
        if let Some(m) = rec.members.iter_mut().find(|m| m.root_id == a_root) {
            m.access_key = Some(owner_ak.clone());
        }
        OrganizationService::save_record(&mut b, &rec).unwrap();
    }

    // A 写 acl：owners=[a_root]，签名者=a_root 组织域身份
    let acl_key = spark_core::sync::orgsync::acl_key(ORG_ID, enc_name, enc_version);
    let col_full = format!("{enc_name}@v{enc_version}");
    let payload = spark_core::sync::orgsync::acl_sign_payload(
        1,
        ORG_ID,
        &col_full,
        &[a_root.clone()],
        &[b_root.clone()],
        None,
        NOW,
    );
    let sig = spark_core::sync::orgsync::acl_sign(&owner_domain.signing_key, &payload);
    let acl_json = serde_json::json!({
        "owners": [a_root],
        "readers": [b_root],
        "epoch": 1,
        "updatedAt": NOW,
        "sig": sig,
    });
    // acl 写 pmeta（受管 org:acl: 键，复刻 VersionedStorage 记账）
    put_personal(&mut a, "node-a", &acl_key, &acl_json.to_string(), NOW).unwrap();

    // 关键断言：org:structure（all-members）的增量采集**包含 acl 记录**——
    // 普通成员经 org:structure 反熵即可收到 acl（不依赖 encrypted 集合的
    // data-accounts 复制组）。
    let struct_inc = spark_core::sync::orgsync::collect_org_incremental(
        &a,
        ORG_ID,
        "org:structure",
        "1",
        &Default::default(),
        0,
    )
    .unwrap();
    assert!(
        struct_inc.iter().any(|r| r.key == acl_key),
        "org:structure 增量采集包含 acl（全员可达）"
    );

    // 成员 B（非数据账号）通过 org:structure 拉取即得 acl：装配 org:structure
    // data 发 B，B 合入 acl（签名者 a_root ∈ owners，acl 验签通过）。
    let acl_record = struct_inc.iter().find(|r| r.key == acl_key).unwrap().clone();
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        "org:structure@v1",
        &[acl_record],
        0,
        1,
    );
    let r = deliver_orgsync(
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
    assert_eq!(r.response, json!({ "ok": true }), "acl 经 org:structure 全员合入");
    let stored: spark_core::sync::orgsync::AclRecord =
        serde_json::from_str(&b.get(&acl_key).unwrap().unwrap()).unwrap();
    assert!(stored.is_reader(&b_root), "B 收到 acl（readers 含自己）");
    assert!(stored.is_owner(&a_root), "acl owner 正确");
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
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        &format!("{NAME}@v{VERSION}"),
        &[decl_record],
        0,
        1,
    );
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
    assert_eq!(r.response, json!({ "ok": true }), "声明记录 org:coll: 白名单放行");

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
    assert!(
        conflict.is_err(),
        "代际内冲突声明应被拒绝"
    );
    let err = conflict.unwrap_err();
    assert!(
        err.to_string().contains("already declared"),
        "冲突文案指明既有声明：{err}"
    );
}
