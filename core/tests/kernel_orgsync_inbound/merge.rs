//! F1/F2 集成测试：org:meta 成员表并发合并收敛（org-meta-lww-fix）+
//! 集合声明全员同步与确定性收敛（org-acl-genesis-fix）。

use super::*;

const STRUCT: &str = "org:structure";
const V1: &str = "1";

/// 构造 org:structure 集合的 hello（以 sender 视角，面向 recipient 成员）。
fn build_structure_hello_for(
    storage: &MemoryStorage,
    org_id: &str,
    recipient_root: &str,
    recipient_peer: &str,
) -> serde_json::Value {
    let collections = collect_org_collections(
        storage,
        org_id,
        &[(STRUCT.to_string(), V1.to_string())],
        recipient_root,
        recipient_peer,
    )
    .unwrap();
    build_orgsync_hello(org_id, collections, &["data".to_string()], "pc")
}

/// 一轮 hello→need→data 交换：sender 侧装配 hello 发 receiver，驱动完
/// need/data 链路（返回各阶段产出供断言）。
fn exchange_structure(
    sender: &mut MemoryStorage,
    sender_root: &str,
    sender_key: &SigningKey,
    sender_node: &str,
    receiver: &mut MemoryStorage,
    receiver_root: &str,
    receiver_key: &SigningKey,
    receiver_node: &str,
) {
    let hello = build_structure_hello_for(
        sender,
        ORG_ID,
        receiver_root,
        &format!("peer-{receiver_node}"),
    );
    let r = deliver_orgsync(
        receiver,
        receiver_root,
        "R",
        sender_key,
        sender_root,
        receiver_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello,
        &format!("peer-{sender_node}"),
        receiver_node,
    );
    assert_eq!(r.response, json!({ "ok": true }));
    // receiver 可能回 need（落后/并发）或主动推 data（领先）
    for out in &r.orgsync_out {
        let body = out.body().clone();
        if body.get("knownVv").is_some() {
            // need → sender 采集增量回 data
            let r2 = deliver_orgsync(
                sender,
                sender_root,
                "S",
                receiver_key,
                receiver_root,
                sender_root,
                dm_envelope::KIND_ORGSYNC_NEED,
                body,
                &format!("peer-{receiver_node}"),
                sender_node,
            );
            for out2 in &r2.orgsync_out {
                if out2.body().get("records").is_some() {
                    let r3 = deliver_orgsync(
                        receiver,
                        receiver_root,
                        "R",
                        sender_key,
                        sender_root,
                        receiver_root,
                        dm_envelope::KIND_ORGSYNC_DATA,
                        out2.body().clone(),
                        &format!("peer-{sender_node}"),
                        receiver_node,
                    );
                    assert_eq!(r3.response, json!({ "ok": true }));
                }
            }
        } else if body.get("records").is_some() {
            // sender 主动推的 data → 本函数即 sender 侧视角的反向……
            // （orgsync-hello 入站的 LocalAhead 分支产出 data 回推 receiver）
            let _ = body;
        }
    }
}

/// 本地发布成员 accessKey：改成员记录 + 写 org:meta（put_personal 记本机
/// 分量，复刻 VersionedStorage 受管写）。
fn publish_access_key_local(
    storage: &mut MemoryStorage,
    node_id: &str,
    org_id: &str,
    root_id: &str,
    tag: &str,
    now: i64,
) {
    let mut record = OrganizationService::get_record(storage, org_id)
        .unwrap()
        .unwrap();
    let m = record
        .members
        .iter_mut()
        .find(|m| m.root_id == root_id)
        .expect("成员存在");
    m.access_key = Some(spark_core::org::types::OrganizationAccessKey {
        public_key: format!("pk-{tag}"),
        bind_sig: "bind".to_string(),
    });
    record.updated_at = now;
    put_personal(
        storage,
        node_id,
        &format!("org:meta:{org_id}"),
        &serde_json::to_string(&record).unwrap(),
        now,
    )
    .unwrap();
}

fn member_access_key(storage: &MemoryStorage, org_id: &str, root_id: &str) -> Option<String> {
    OrganizationService::get_record(storage, org_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .find(|m| m.root_id == root_id)
        .and_then(|m| m.access_key.as_ref())
        .map(|ak| ak.public_key.clone())
}

/// F1 靶心回归（联调实测序列单测化）：A、B 各自本地 publish_access_key
/// （互不知情，并发）→ orgsync（org:structure）交换 → 两端 members 均含
/// 双方 accessKey、折叠 vv 收敛一致、再交换判 Equal（不动点）；合并不写
/// 本机序号键（无回声）。
#[test]
fn org_meta_concurrent_access_key_merge_converges() {
    let (a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Member),
            ],
            &[],
        );
    }
    // 内建 org:structure 集合声明（复刻生产：组织创建时内核自动注册；
    // 缺声明时 accounts 缺省 data-accounts，普通成员 B 不在复制组）
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();
    // 背靠背并发发布（修复前：whole-record LWW，后写方抹掉对方 accessKey）
    publish_access_key_local(&mut a, "node-a", ORG_ID, &a_root, "a", NOW);
    publish_access_key_local(&mut b, "node-b", ORG_ID, &b_root, "b", NOW);

    // 第 1 轮：A → B（并发 → B 结构化合并）
    let meta_a_org = get_personal_meta(&a, &format!("org:meta:{ORG_ID}"))
        .unwrap()
        .unwrap();
    let meta_b_org = get_personal_meta(&b, &format!("org:meta:{ORG_ID}"))
        .unwrap()
        .unwrap();
    exchange_structure(
        &mut a, &a_root, &a_key, "node-a", &mut b, &b_root, &b_key, "node-b",
    );
    // members 并集去重：双侧共有成员不得重复落库（修复回归钉：合并键迭代
    // 未去重曾使成员表翻倍，find 式断言对此失明）
    let b_member_count = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap()
        .members
        .len();
    assert_eq!(b_member_count, 2, "合并后成员数 = 并集大小（无重复）");
    assert_eq!(
        member_access_key(&b, ORG_ID, &a_root).as_deref(),
        Some("pk-a"),
        "B 合并出 A 的 accessKey"
    );
    assert_eq!(
        member_access_key(&b, ORG_ID, &b_root).as_deref(),
        Some("pk-b"),
        "B 自己的 accessKey 不丢"
    );
    // 合并 vv 支配两个输入（值/vv 不脱节）：org:meta 的 pmeta 含双分量
    let meta_b = get_personal_meta(&b, &format!("org:meta:{ORG_ID}"))
        .unwrap()
        .unwrap();
    assert_eq!(
        meta_b.vv.get("node-a"),
        meta_a_org.vv.get("node-a"),
        "A 侧分量并入合并 vv"
    );
    // 合并不 bump 本机分量（无回声）：node-b 分量仍是本地发布时的值
    // （若合并误走本地写记账会被进一步推进）
    assert_eq!(
        meta_b.vv.get("node-b"),
        meta_b_org.vv.get("node-b"),
        "合并是合入语义，本机分量不被推进"
    );

    // 第 2 轮：B → A（B 的合并结果对 A 是 Remote 快路径整值覆盖）
    exchange_structure(
        &mut b, &b_root, &b_key, "node-b", &mut a, &a_root, &a_key, "node-a",
    );
    assert_eq!(
        member_access_key(&a, ORG_ID, &a_root).as_deref(),
        Some("pk-a")
    );
    assert_eq!(
        member_access_key(&a, ORG_ID, &b_root).as_deref(),
        Some("pk-b")
    );

    // 两端记录逐字节一致 + 折叠 vv 收敛一致
    let rec_a = a.get(&format!("org:meta:{ORG_ID}")).unwrap().unwrap();
    let rec_b = b.get(&format!("org:meta:{ORG_ID}")).unwrap().unwrap();
    assert_eq!(rec_a, rec_b, "两端 org:meta 收敛到同一记录");
    let fold_a = collect_org_collection_vv(&a, ORG_ID, STRUCT, V1).unwrap();
    let fold_b = collect_org_collection_vv(&b, ORG_ID, STRUCT, V1).unwrap();
    assert_eq!(fold_a, fold_b, "两端折叠 vv 收敛一致");

    // 不动点：再交换无任何 need/data 产出
    let hello = build_structure_hello_for(&a, ORG_ID, &b_root, "peer-node-b");
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello,
        "peer-node-a",
        "node-b",
    );
    assert!(r.orgsync_out.is_empty(), "收敛后再交换判 Equal（不动点）");
}

/// F2-P1：data-accounts 集合的声明并入 org:structure 键域——普通成员
/// （非复制组成员）经 org:structure 反熵即可收讫声明（hello 裁剪不影响）。
#[test]
fn decl_travels_via_org_structure_to_plain_member() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
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
    // A 声明 data-accounts 集合（B 不在复制组）
    declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::DataAccounts,
        &a_root,
        NOW,
    );
    let decl_key = org_decl_key(ORG_ID, NAME, VERSION);

    // org:structure 增量采集包含声明（B 经此通道可得）
    let inc = collect_org_incremental(&a, ORG_ID, STRUCT, V1, &Default::default(), 0).unwrap();
    let decl_record = inc
        .iter()
        .find(|r| r.key == decl_key)
        .expect("声明并入 org:structure 增量（全员可达）")
        .clone();
    // 声明不再随所属集合（data-accounts）流量内联携带
    let inc_plugin =
        collect_org_incremental(&a, ORG_ID, NAME, VERSION, &Default::default(), 0).unwrap();
    assert!(
        !inc_plugin.iter().any(|r| r.key == decl_key),
        "声明不再随所属集合流量携带"
    );

    // org:structure data 批次发 B（普通成员）：B3 白名单经 org:structure
    // 键域放行 org:coll: 键，合入后 B 可读声明
    let data_body =
        build_orgsync_data_batch(ORG_ID, &format!("{STRUCT}@v{V1}"), &[decl_record], 0, 1);
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
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "声明经 org:structure 放行合入"
    );
    let resolved = spark_core::plugindata::resolve_org(&b, ORG_ID, NAME, Some(VERSION)).unwrap();
    assert_eq!(resolved.name, NAME, "普通成员读到声明");
}

/// F2-P2：策略真实分歧（accounts 不同）→ 维持「保留先见者 + 冲突日志」，
/// 不自动调和（先到者保留，后到的不替换）。
#[test]
fn decl_strategy_conflict_keeps_first_seen() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Member),
            ],
            &[],
        );
    }
    // B 本地声明 all-members；A 的同名声明是 data-accounts（策略冲突）
    let decl_b = declare_org_collection(
        &mut b,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &b_root,
        NOW + 100,
    );
    let decl_a = declare_org_collection(
        &mut a,
        "node-a",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::DataAccounts,
        &a_root,
        NOW,
    );
    let decl_key = org_decl_key(ORG_ID, NAME, VERSION);

    let decl_record = spark_core::sync::orgsync::OrgsyncRecord {
        key: decl_key.clone(),
        value: serde_json::to_value(&decl_a).unwrap(),
        meta: get_personal_meta(&a, &decl_key).unwrap().unwrap(),
        dseq: None,
    };
    let data_body =
        build_orgsync_data_batch(ORG_ID, &format!("{STRUCT}@v{V1}"), &[decl_record], 0, 1);
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
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "冲突声明不整批拒收（跳过该条）"
    );
    let stored: spark_core::plugindata::CollectionDeclaration =
        serde_json::from_str(&b.get(&decl_key).unwrap().unwrap()).unwrap();
    assert_eq!(
        stored.declared_by.as_deref(),
        Some(b_root.as_str()),
        "策略冲突保留先见者（B 本地声明不被替换）"
    );
    assert_eq!(stored.accounts, decl_b.accounts, "策略轴维持本地值");
}

// ── F8. 写侧脱节（RMW vv/内容脱节）自愈链 ──────────────────────────────

/// F8 §4 自愈链集成测试（org-meta-rmw-fix §2.3）：B 产脱节记录（旧内容无 C
/// + vv 覆盖 A 的 add-C 分量）→ A 判 Remote 整值覆盖丢 C（收侧 WARN 路径，
/// 不拦截——合法踢出同触发）→ C（有自己本地写分量）收 B 记录判 Concurrent
/// → 结构化合并复活 C → C 回推 → A 判 Remote 覆盖为含 C 的合并版康复。
#[test]
fn org_meta_detached_record_self_heals_via_third_party_merge() {
    let (_a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let (_c_key, c_root) = self_identity(3);
    let meta_key = format!("org:meta:{ORG_ID}");

    // A/C 持有「含成员 C」的当前版本；B 持有「无 C」的旧内容
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    let mut c = MemoryStorage::new();
    for s in [&mut a, &mut c] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Member),
                (c_root.as_str(), OrganizationRole::Member),
            ],
            &[],
        );
    }
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    // 内建集合声明（复制组判定需要；A/C/B 各自的 node 分量入 fold）
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();
    spark_core::plugindata::declare_builtin_org_collections(&mut c, ORG_ID, &c_root, NOW, "node-c")
        .unwrap();
    // A/C 的 org:meta 写 pmeta（含 C 的当前内容）；B 的 org:meta 是旧内容
    let a_content = a.get(&meta_key).unwrap().unwrap();
    let a_meta = put_personal(&mut a, "node-a", &meta_key, &a_content, NOW).unwrap();
    let c_content = c.get(&meta_key).unwrap().unwrap();
    put_personal(&mut c, "node-c", &meta_key, &c_content, NOW).unwrap();
    let a_seq_covered = *a_meta.vv.get("node-a").unwrap();
    let old_content_b = b.get(&meta_key).unwrap().unwrap();
    let b_meta = put_personal(&mut b, "node-b", &meta_key, &old_content_b, NOW).unwrap();

    // B 产脱节记录：旧内容（无 C）+ vv 覆盖 A 的 add-C 分量（手工拼
    // {node-a:5, node-b:5}——等价于中间件在入站合入后的提交时点 bump）
    let mut detached_meta = b_meta.clone();
    detached_meta.vv.insert("node-a".to_string(), a_seq_covered);
    spark_core::sync::set_personal_meta(&mut b, &meta_key, &detached_meta).unwrap();

    let detached_record = spark_core::sync::orgsync::OrgsyncRecord {
        key: meta_key.clone(),
        value: serde_json::from_str(&old_content_b).unwrap(),
        meta: detached_meta.clone(),
        dseq: None,
    };

    // 1) A 收 B 脱节记录：{node-a:5} vs {node-a:5, node-b:5} → Remote 整值
    //    覆盖 → 丢 C（收侧 WARN 路径触发，不拦截）
    let body = build_orgsync_data_batch(
        ORG_ID,
        &format!("{STRUCT}@v{V1}"),
        &[detached_record.clone()],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        body,
        "peer-b",
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let rec_a = OrganizationService::get_record(&a, ORG_ID)
        .unwrap()
        .unwrap();
    assert!(
        rec_a.find_member(&c_root).is_none(),
        "脱节记录 Remote 覆盖 → A 丢成员 C（F8 脱节形态复现）"
    );

    // 2) C 本地写（自己的 nickname 变更）→ 本地分量推进，持有含 C 的内容
    let mut c_rec = OrganizationService::get_record(&c, ORG_ID)
        .unwrap()
        .unwrap();
    c_rec
        .members
        .iter_mut()
        .find(|m| m.root_id == c_root)
        .unwrap()
        .nickname = Some("C 自称".to_string());
    c_rec.updated_at = NOW + 1;
    put_personal(
        &mut c,
        "node-c",
        &meta_key,
        &serde_json::to_string(&c_rec).unwrap(),
        NOW + 1,
    )
    .unwrap();

    // 3) C 收 B 脱节记录：本地 {node-c:*} vs 远端 {node-a,node-b} → Concurrent
    //    → 结构化合并：members 并集复活 C + C 的 nickname 保留
    let body =
        build_orgsync_data_batch(ORG_ID, &format!("{STRUCT}@v{V1}"), &[detached_record], 0, 1);
    let r = deliver_orgsync(
        &mut c,
        &c_root,
        "C",
        &b_key,
        &b_root,
        &c_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        body,
        "peer-b",
        "node-c",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let rec_c = OrganizationService::get_record(&c, ORG_ID)
        .unwrap()
        .unwrap();
    assert!(
        rec_c.find_member(&c_root).is_some(),
        "C 合并后成员 C 仍在（并集复活）"
    );
    let merged_meta_c = get_personal_meta(&c, &meta_key).unwrap().unwrap();
    assert!(
        merged_meta_c.vv.contains_key("node-a")
            && merged_meta_c.vv.contains_key("node-b")
            && merged_meta_c.vv.contains_key("node-c"),
        "合并 vv 支配双输入（含三方分量）"
    );

    // 4) 自愈回流：orgsync 推送面此时对 C 已关闭（A 的成员表被脱节记录
    //    抹掉 C，C 的信封过不了「from ∈ 成员表」公共前置）——现实自愈通道是
    //    A 主动发起协调后把 C 的整值快照合入本地（合入侧不做该前置校验，
    //    响应的是 A 自己发起的请求）。此处用保留的快照合并原语（normalize →
    //    merge → 原子段落库）模拟该合入：成员级并集合并复活 C。
    //    （设计 §2.3 写的是「C 回推 → A 判 Remote」——该路径被信封鉴权挡住，
    //    以代码现实为准走快照合入面，见交还报告偏差说明。）
    let healed_json: serde_json::Value =
        serde_json::from_str(&c.get(&meta_key).unwrap().unwrap()).unwrap();
    let io_lock = std::sync::Arc::new(std::sync::Mutex::new(()));
    let snapshot = spark_core::org::snapshot::normalize_incoming_snapshot(&healed_json).unwrap();
    OrganizationService::update_record_atomic(&mut a, &io_lock, ORG_ID, |_storage, record| {
        *record = spark_core::org::snapshot::merge_organization_sync_snapshot(
            Some(record),
            &snapshot,
            NOW + 2,
        );
        Ok(true)
    })
    .unwrap();
    let rec_a = OrganizationService::get_record(&a, ORG_ID)
        .unwrap()
        .unwrap();
    assert!(
        rec_a.find_member(&c_root).is_some(),
        "回推康复：A 重新拥有成员 C"
    );
    let c_member = rec_a.find_member(&c_root).unwrap();
    assert_eq!(
        c_member.nickname.as_deref(),
        Some("C 自称"),
        "C 的本地变更随合并版到达"
    );
}

/// 卫生批项2（replica.rs overview 记账切 orgsync 平面）：验签成功的
/// orgsync-hello 交换反哺 org-sync-state（账号口径）——纯 orgsync 双端的
/// overview everSynced 不再恒为「未同步」。
#[test]
fn orgsync_hello_feeds_sync_state_overview() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Member),
            ],
            &[],
        );
    }
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();

    // A hello → B：B 侧应记下「A 账号最近同步过」
    let hello = build_hello_for(&a, ORG_ID, &b_root, "peer-b");
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello,
        "peer-a",
        "node-b",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let state_key = spark_core::org::sync_state::org_sync_state_account_key(&a_root, ORG_ID);
    let state = spark_core::org::sync_state::OrgSyncState::from_json(
        &b.get(&state_key)
            .unwrap()
            .expect("orgsync 交换反哺 sync-state"),
    )
    .expect("state 解析");
    assert_eq!(state.last_synced_at, NOW);

    // overview 口径：A 账号 ever_synced（recentlySynced 窗口内）
    let record = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    let versions = record.sync.as_ref().map(|s| s.versions);
    let mut b2 = b.clone();
    let overview = spark_core::org::compute_org_sync_overview(
        &record,
        Some(&b_root),
        versions.as_ref(),
        |root_id, legacy_peer| {
            spark_core::org::sync_state::read_org_sync_state_account(
                &mut b2,
                root_id,
                ORG_ID,
                legacy_peer,
            )
        },
        &[],
        false,
        |_| "pc",
        NOW,
    );
    let member_a = overview
        .members
        .iter()
        .find(|m| m.root_id == a_root)
        .unwrap();
    assert!(member_a.ever_synced, "反哺后 A 账号计入 everSynced");
    let member_b = overview
        .members
        .iter()
        .find(|m| m.root_id == b_root)
        .unwrap();
    assert!(member_b.ever_synced, "本机恒 everSynced");
}

// ── batch3 §2 管理面邀请投影（org:invitations@v1 / org:invpub:）──────────────

/// 双管理员互见 pending 邀请：A 邀 C 的投影经 orgsync（org:invitations 集合
/// 流量）到达 B；A 侧转 accepted 后 B 侧投影同步转终态；投影不含 inviteCode；
/// 键双维度无碰撞（同邀请人两人 / 两邀请人同一人共存）。
#[test]
fn invpub_projection_syncs_between_admins() {
    let (a_key, a_root) = self_identity(1);
    let (b_key, b_root) = self_identity(2);
    let c_root = self_identity(3).1;
    let d_root = self_identity(4).1;
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Admin),
            ],
            &[],
        );
        spark_core::plugindata::declare_builtin_org_collections(s, ORG_ID, &a_root, NOW, "node-x")
            .unwrap();
    }
    let inv_key_a_c = spark_core::org::service::org_invpub_key(ORG_ID, &a_root, &c_root);
    let invite = |_inviter: &str, invitee: &str, status, now| {
        spark_core::org::invite_record::OrgInviteRecord {
            id: format!("inv-{now}"),
            org_id: ORG_ID.to_string(),
            org_name: "t".to_string(),
            org_avatar: None,
            peer_root_id: invitee.to_string(),
            peer_nickname: "待加入成员".to_string(),
            direction: spark_core::org::OrgInviteDirection::Outgoing,
            status,
            invite_code: None,
            created_at: now,
            updated_at: now,
        }
    };

    // A：出站记录（personal 域）+ 投影（管理面）显式记账（raw 测试口径，
    // 生产 facade 为 put_invite_record_with_projection 单 batch 双写）
    let rec = invite(
        &a_root,
        &c_root,
        spark_core::org::OrgInviteStatus::Pending,
        NOW,
    );
    put_personal(
        &mut a,
        "node-a",
        &format!("org:inv:out:{ORG_ID}:{c_root}"),
        &serde_json::to_string(&rec).unwrap(),
        NOW,
    )
    .unwrap();
    let (pk, pv) = spark_core::org::service::invpub_projection(&rec, &a_root).unwrap();
    assert_eq!(pk, inv_key_a_c);
    assert!(pv.get("inviteCode").is_none(), "投影不含 inviteCode");
    put_personal(
        &mut a,
        "node-a",
        &pk,
        &serde_json::to_string(&pv).unwrap(),
        NOW,
    )
    .unwrap();
    // 碰撞面：A 再邀 D、B 邀 C——三键共存
    let rec2 = invite(
        &a_root,
        &d_root,
        spark_core::org::OrgInviteStatus::Pending,
        NOW + 1,
    );
    let (pk2, pv2) = spark_core::org::service::invpub_projection(&rec2, &a_root).unwrap();
    assert_ne!(pk, pk2, "同邀请人两被邀请人不撞键");
    let rec3 = invite(
        &b_root,
        &c_root,
        spark_core::org::OrgInviteStatus::Pending,
        NOW + 2,
    );
    let (pk3, _) = spark_core::org::service::invpub_projection(&rec3, &b_root).unwrap();
    assert_ne!(pk, pk3, "两邀请人同被邀请人不撞键");
    put_personal(
        &mut a,
        "node-a",
        &pk2,
        &serde_json::to_string(&pv2).unwrap(),
        NOW,
    )
    .unwrap();

    // A → B 交换 org:invitations
    let collections = collect_org_collections(
        &a,
        ORG_ID,
        &[("org:invitations".to_string(), "1".to_string())],
        &b_root,
        "peer-b",
    )
    .unwrap();
    let hello = build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc");
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello,
        "peer-a",
        "node-b",
    );
    for out in &r.orgsync_out {
        let body = out.body().clone();
        if body.get("knownVv").is_some() {
            let r2 = deliver_orgsync(
                &mut a,
                &a_root,
                "A",
                &b_key,
                &b_root,
                &a_root,
                dm_envelope::KIND_ORGSYNC_NEED,
                body,
                "peer-b",
                "node-a",
            );
            for out2 in &r2.orgsync_out {
                if out2.body().get("records").is_some() {
                    let r3 = deliver_orgsync(
                        &mut b,
                        &b_root,
                        "B",
                        &a_key,
                        &a_root,
                        &b_root,
                        dm_envelope::KIND_ORGSYNC_DATA,
                        out2.body().clone(),
                        "peer-a",
                        "node-b",
                    );
                    assert_eq!(r3.response, json!({ "ok": true }));
                }
            }
        }
    }
    // B 互见：两条投影都在（A→C、A→D），pending 状态
    let proj: serde_json::Value =
        serde_json::from_str(&b.get(&inv_key_a_c).unwrap().expect("B 收讫 A→C 投影")).unwrap();
    assert_eq!(proj["inviter"], json!(a_root));
    assert_eq!(proj["invitee"], json!(c_root));
    assert_eq!(proj["status"], json!("pending"));
    assert!(b.get(&pk2).unwrap().is_some(), "A→D 投影同批到达");

    // A 侧转 accepted（回执受理/对账同口径：投影 put_personal 重写）→ B 同步转终态
    let rec_a = spark_core::org::OrgInviteRecord {
        status: spark_core::org::OrgInviteStatus::Accepted,
        updated_at: NOW + 10,
        ..rec
    };
    let (_, pv_a) = spark_core::org::service::invpub_projection(&rec_a, &a_root).unwrap();
    put_personal(
        &mut a,
        "node-a",
        &inv_key_a_c,
        &serde_json::to_string(&pv_a).unwrap(),
        NOW + 10,
    )
    .unwrap();
    let collections = collect_org_collections(
        &a,
        ORG_ID,
        &[("org:invitations".to_string(), "1".to_string())],
        &b_root,
        "peer-b",
    )
    .unwrap();
    let hello = build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc");
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello,
        "peer-a",
        "node-b",
    );
    for out in &r.orgsync_out {
        let body = out.body().clone();
        if body.get("knownVv").is_some() {
            let r2 = deliver_orgsync(
                &mut a,
                &a_root,
                "A",
                &b_key,
                &b_root,
                &a_root,
                dm_envelope::KIND_ORGSYNC_NEED,
                body,
                "peer-b",
                "node-a",
            );
            for out2 in &r2.orgsync_out {
                if out2.body().get("records").is_some() {
                    deliver_orgsync(
                        &mut b,
                        &b_root,
                        "B",
                        &a_key,
                        &a_root,
                        &b_root,
                        dm_envelope::KIND_ORGSYNC_DATA,
                        out2.body().clone(),
                        "peer-a",
                        "node-b",
                    );
                }
            }
        }
    }
    let proj2: serde_json::Value =
        serde_json::from_str(&b.get(&inv_key_a_c).unwrap().unwrap()).unwrap();
    assert_eq!(proj2["status"], json!("accepted"), "B 侧投影同步转终态");
}

/// F4 第二层（batch3 §1.2 成员表对账兜底）× 裁决 §10.2 收紧：仅「在成员表」
/// 不再够（预录模型冲突——预录未应答者也在表内）——合入记录 vv 须含
/// invitee 本人设备分量才标 accepted。本用例钉「预录到达不误标 → 本人分量
/// 到达（legacy join 回流形态）才标」两段式。
#[test]
fn outbound_invite_reconciled_when_invitee_in_member_table() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let c_root = self_identity(3).1;
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    for s in [&mut a, &mut b] {
        save_org(
            s,
            ORG_ID,
            vec![
                (a_root.as_str(), OrganizationRole::Admin),
                (b_root.as_str(), OrganizationRole::Admin),
            ],
            &[],
        );
        spark_core::plugindata::declare_builtin_org_collections(s, ORG_ID, &a_root, NOW, "node-x")
            .unwrap();
    }
    // B 侧：邀 C 的 outbound pending（回执丢失形态）+ 投影
    let rec = spark_core::org::invite_record::OrgInviteRecord {
        id: "inv-c".to_string(),
        org_id: ORG_ID.to_string(),
        org_name: "t".to_string(),
        org_avatar: None,
        peer_root_id: c_root.clone(),
        peer_nickname: "待加入成员".to_string(),
        direction: spark_core::org::OrgInviteDirection::Outgoing,
        status: spark_core::org::OrgInviteStatus::Pending,
        invite_code: None,
        created_at: NOW,
        updated_at: NOW,
    };
    put_personal(
        &mut b,
        "node-b",
        &format!("org:inv:out:{ORG_ID}:{c_root}"),
        &serde_json::to_string(&rec).unwrap(),
        NOW,
    )
    .unwrap();
    let (pk, pv) = spark_core::org::service::invpub_projection(&rec, &b_root).unwrap();
    put_personal(
        &mut b,
        "node-b",
        &pk,
        &serde_json::to_string(&pv).unwrap(),
        NOW,
    )
    .unwrap();

    // A 把 C 加进成员表（预录——携端点 peer-c 但 C 尚未应答）→ org:meta 经
    // org:structure 到 B：vv 只含 A 的分量（无 C 本人分量）→ 不误标
    let mut rec_a = OrganizationService::get_record(&a, ORG_ID)
        .unwrap()
        .unwrap();
    rec_a.members.push({
        let mut m = member(&c_root, OrganizationRole::Member);
        m.node_info = Some(spark_core::org::types::OrganizationDeviceSet::from_single(
            spark_core::org::types::OrganizationNodeInfo {
                device_uid: Some("uid-c".to_string()),
                peer_id: Some("peer-c".to_string()),
                addresses: Vec::new(),
            },
        ));
        m
    });
    rec_a.updated_at = NOW + 100;
    let meta_key = format!("org:meta:{ORG_ID}");
    put_personal(
        &mut a,
        "node-a",
        &meta_key,
        &serde_json::to_string(&rec_a).unwrap(),
        NOW + 100,
    )
    .unwrap();
    let org_meta_record = spark_core::sync::orgsync::OrgsyncRecord {
        key: meta_key.clone(),
        value: serde_json::from_str(&a.get(&meta_key).unwrap().unwrap()).unwrap(),
        meta: get_personal_meta(&a, &meta_key).unwrap().unwrap(),
        dseq: None,
    };
    let body =
        build_orgsync_data_batch(ORG_ID, &format!("{STRUCT}@v{V1}"), &[org_meta_record], 0, 1);
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        body,
        "peer-a",
        "node-b",
    );
    assert_eq!(r.response, json!({ "ok": true }));

    // 裁决 §10.2：预录到达（vv 无 C 本人分量）→ 不误标，停留 pending
    let still: spark_core::org::invite_record::OrgInviteRecord = serde_json::from_str(
        &b.get(&format!("org:inv:out:{ORG_ID}:{c_root}"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        still.status,
        spark_core::org::OrgInviteStatus::Pending,
        "预录形态（vv 无 invitee 本人分量）不误标 accepted"
    );

    // C 真走接受编排后回流（legacy join 形态：C 快照合入 bump 本机分量，
    // whole 携带 C 分量）→ 对账生效：pending → accepted + 投影同步 + 事件
    let mut meta_with_c = get_personal_meta(&a, &meta_key).unwrap().unwrap();
    meta_with_c.vv.insert("peer-c".to_string(), 1);
    meta_with_c.ts = NOW + 200;
    let org_meta_record2 = spark_core::sync::orgsync::OrgsyncRecord {
        key: meta_key.clone(),
        value: serde_json::from_str(&a.get(&meta_key).unwrap().unwrap()).unwrap(),
        meta: meta_with_c,
        dseq: None,
    };
    let body = build_orgsync_data_batch(
        ORG_ID,
        &format!("{STRUCT}@v{V1}"),
        &[org_meta_record2],
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
        body,
        "peer-a",
        "node-b",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let updated: spark_core::org::invite_record::OrgInviteRecord = serde_json::from_str(
        &b.get(&format!("org:inv:out:{ORG_ID}:{c_root}"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        updated.status,
        spark_core::org::OrgInviteStatus::Accepted,
        "本人分量到达 → 对账原地标 accepted"
    );
    let proj: serde_json::Value = serde_json::from_str(&b.get(&pk).unwrap().unwrap()).unwrap();
    assert_eq!(proj["status"], json!("accepted"), "投影同步转 accepted");
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, spark_core::p2p::P2pEvent::OrgInviteUpdated(_))),
        "发出 OrgInviteUpdated 事件"
    );
}
