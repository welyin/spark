//! orgsync 入站：资格拒绝（非成员/复制组外）+ 键白名单整批拒收。

use super::*;

// ── 2. 资格拒绝：非成员 / 复制组外普通成员 ──────────────────────────────

#[test]
fn orgsync_rejects_non_member_and_outside_replication_group() {
    // 集合 data-accounts：复制组 = 数据账号（此处显式指定 = [admin]）。
    let (_a_key, a_root) = self_identity(1); // admin / 数据账号
    let (m_key, m_root) = self_identity(3); // 普通成员（非复制组）
    let (x_key, x_root) = self_identity(9); // 非成员
    let (_self_key, self_root) = self_identity(2); // 本机 B（成员、数据账号）
    let mut s = MemoryStorage::new();

    save_org(
        &mut s,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[a_root.as_str(), self_root.as_str()],
    );
    declare_org_collection(
        &mut s,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::DataAccounts,
        &self_root,
        NOW,
    );

    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));

    // (a) 非成员 from 发 hello → rejected
    let hello_x = build_hello_for(&s, ORG_ID, &x_root, "peer-x");
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &x_key,
        &x_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_x,
        "peer-x",
        "node-b",
    );
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("rejected"), "非成员 hello 拒绝");

    // (b) 复制组外普通成员发 hello → 公共前置通过但集合复制组拒绝 → 该集合
    // 被静默跳过（ok:true，无任何 diff/合并输出）——不复写数据即安全。
    let hello_m = build_hello_for(&s, ORG_ID, &m_root, "peer-m");
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &m_key,
        &m_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        hello_m,
        "peer-m",
        "node-b",
    );
    assert_eq!(
        r.response["ok"], true,
        "复制组外成员 hello 静默跳过（非 rejected）"
    );
    assert!(
        r.orgsync_out.is_empty(),
        "复制组外成员 hello 无 diff 输出（零合入）"
    );

    // (b2) 复制组外普通成员发 need → rejected（need/data 走显式拒绝）
    let need_m = build_orgsync_need(
        ORG_ID,
        &format!("{NAME}@v{VERSION}"),
        &Default::default(),
        0,
    );
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &m_key,
        &m_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        need_m,
        "peer-m",
        "node-b",
    );
    assert_eq!(r.response["ok"], false);
    assert_eq!(
        r.response["reason"],
        json!("rejected"),
        "复制组外成员 need 拒绝"
    );

    // (c) 复制组外普通成员发 data（携带合法 orgd 键）→ rejected 且零合入
    let records = vec![spark_core::sync::orgsync::OrgsyncRecord {
        key: data_key.clone(),
        value: json!("poisoned"),
        meta: DocMeta {
            vv: [("node-m".to_string(), 1)].into_iter().collect(),
            ts: NOW,
            node_id: Some("node-m".to_string()),
            ..Default::default()
        },
        dseq: None,
    }];
    let data_body = build_orgsync_data_batch(ORG_ID, &format!("{NAME}@v{VERSION}"), &records, 0, 1);
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &m_key,
        &m_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-m",
        "node-b",
    );
    assert_eq!(r.response["ok"], false);
    assert_eq!(
        r.response["reason"],
        json!("rejected"),
        "复制组外成员 data 拒绝"
    );
    assert!(s.get(&data_key).unwrap().is_none(), "被拒 data 零合入");

    // (d) 非成员发 need → rejected
    let need_body = build_orgsync_need(
        ORG_ID,
        &format!("{NAME}@v{VERSION}"),
        &Default::default(),
        0,
    );
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &x_key,
        &x_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_NEED,
        need_body,
        "peer-x",
        "node-b",
    );
    assert_eq!(r.response["reason"], json!("rejected"), "非成员 need 拒绝");
    assert!(r.orgsync_out.is_empty(), "被拒 need 无 diff 输出");
}

// ── 3. 键白名单：越界键整批拒收 ─────────────────────────────────────────

#[test]
fn orgsync_data_rejects_keys_outside_collection_prefix() {
    let (a_key, a_root) = self_identity(1); // 复制组成员（all-members）
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();

    save_org(
        &mut s,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[],
    );
    declare_org_collection(
        &mut s,
        "node-b",
        ORG_ID,
        NAME,
        VERSION,
        Accounts::AllMembers,
        &self_root,
        NOW,
    );

    // 越界键：org:meta:（组织记录键，用不存在的组织 id 防止撞上真实 org 记录）
    // 与 pdoc:（个人域同步键）
    for bad_key in [
        "org:meta:org_0000000000forged".to_string(),
        "pdoc:ai-chat@v1:k1".to_string(),
    ] {
        let records = vec![spark_core::sync::orgsync::OrgsyncRecord {
            key: bad_key.clone(),
            value: json!("forged"),
            meta: DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: NOW,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            },
            dseq: None,
        }];
        let data_body =
            build_orgsync_data_batch(ORG_ID, &format!("{NAME}@v{VERSION}"), &records, 0, 1);
        let r = deliver_orgsync(
            &mut s,
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
        assert_eq!(r.response["ok"], false);
        assert_eq!(r.response["reason"], json!("key-out-of-collection"));
        assert!(s.get(&bad_key).unwrap().is_none(), "越界键不得落库");
    }

    // 同批既有合法键也有越界键 → 整批拒收，合法键连坐不落库
    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    let records = vec![
        spark_core::sync::orgsync::OrgsyncRecord {
            key: data_key.clone(),
            value: json!("ok"),
            meta: DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: NOW,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            },
            dseq: None,
        },
        spark_core::sync::orgsync::OrgsyncRecord {
            key: format!("pdoc:ai-chat@v1:k9"),
            value: json!("forged"),
            meta: DocMeta::default(),
            dseq: None,
        },
    ];
    let data_body = build_orgsync_data_batch(ORG_ID, &format!("{NAME}@v{VERSION}"), &records, 0, 1);
    let r = deliver_orgsync(
        &mut s,
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
    assert_eq!(r.response["reason"], json!("key-out-of-collection"));
    assert!(s.get(&data_key).unwrap().is_none(), "连坐：合法键不落库");
}
