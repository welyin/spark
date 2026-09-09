//! orgsync 入站：资格拒绝（非成员/复制组外）+ 键白名单整批拒收。

use super::*;

// ── 2. 资格拒绝：非成员 / 复制组外普通成员 ──────────────────────────────

/// A14 全员数据节点：data-accounts 集合复制组 = 全体成员——普通成员
/// hello/need/data 正常服务合入；非成员仍 rejected。
#[test]
fn orgsync_member_in_replication_group_non_member_rejected() {
    let (_a_key, a_root) = self_identity(1); // admin
    let (m_key, m_root) = self_identity(3); // 普通成员（A14：也在复制组）
    let (x_key, x_root) = self_identity(9); // 非成员
    let (_self_key, self_root) = self_identity(2); // 本机 B（成员）
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

    // (a) 非成员 from 发 hello → rejected（不变）
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

    // (b) 普通成员发 hello → A14 起在复制组：正常服务（不再静默跳过）
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
        "成员（数据节点）hello 正常受理"
    );

    // (b2) 普通成员发 need → A14 起在复制组：受理（不再 rejected）
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
    assert_ne!(
        r.response["reason"],
        json!("rejected"),
        "成员（数据节点）need 不再拒绝"
    );

    // (c) 普通成员发 data（携带合法 orgd 键）→ A14 起合入（键落库）
    let records = vec![spark_core::sync::orgsync::OrgsyncRecord {
        key: data_key.clone(),
        value: json!("member-write"),
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
    assert_ne!(
        r.response["reason"],
        json!("rejected"),
        "成员（数据节点）data 不再拒绝"
    );
    assert!(
        s.get(&data_key).unwrap().is_some(),
        "成员数据合入落库（全员数据节点）"
    );

    // (d) 非成员发 need → rejected（不变）
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
