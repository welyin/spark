//! 阶段四A P1（org-member-split）集成测试：per-member 记录入站合入
//! （成员级结构化合并 + accessKey 写一次守卫下沉）、混跑期 whole 合入
//! 就地投影、成员移除 = 成员记录墓碑传播（org-dlog-only）。

use super::*;

use spark_core::org::types::org_member_key;

const STRUCT: &str = "org:structure";
const V1: &str = "1";
const COL_FULL: &str = "org:structure@v1";

fn ak(tag: &str) -> spark_core::org::types::OrganizationAccessKey {
    spark_core::org::types::OrganizationAccessKey {
        public_key: format!("pk-{tag}"),
        bind_sig: "bind".to_string(),
        root_pubkey: None,
    }
}

fn org_record(
    key: &str,
    value: serde_json::Value,
    meta: DocMeta,
) -> spark_core::sync::orgsync::OrgsyncRecord {
    spark_core::sync::orgsync::OrgsyncRecord {
        key: key.to_string(),
        value,
        meta,
        dseq: None,
    }
}

/// 远端 meta 手工构造（确定性控制 vv/ts，免搭对端存储）。
fn remote_meta(node: &str, seq: i64, ts: i64) -> DocMeta {
    DocMeta {
        vv: [(node.to_string(), seq)].into_iter().collect(),
        ts,
        node_id: Some(node.to_string()),
        tombstone: None,
    }
}

/// P1 靶心：per-member 记录并发合入走条目级结构化合并——B 本地条目（自己
/// 发布的 accessKey，秩低）与 A 推来的同成员条目（管理员改 role，秩高）
/// Concurrent → accessKey 写一次守卫保本地、管理员字段组取秩高侧；合并
/// vv 支配双输入、不 bump 本机分量（合入语义无回声）。
#[test]
fn member_record_concurrent_merge_keeps_local_access_key() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut b = MemoryStorage::new();
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();

    // B 本地条目：自己发布的 accessKey（本机分量 node-b:2，ts 较早）
    let entry_key = org_member_key(ORG_ID, &b_root);
    let mut local_entry = member(&b_root, OrganizationRole::Member);
    local_entry.access_key = Some(ak("b"));
    put_personal(
        &mut b,
        "node-b",
        &entry_key,
        &serde_json::to_string(&local_entry).unwrap(),
        NOW,
    )
    .unwrap();
    // 本地分量推进到 2（模拟既有本地写历史）
    put_personal(
        &mut b,
        "node-b",
        &entry_key,
        &serde_json::to_string(&local_entry).unwrap(),
        NOW,
    )
    .unwrap();

    // 本地分量基线（declare_builtin_org_collections 的声明注册写已推进
    // per-node 序号，两笔条目写在此基础上继续递增——捕获交付前基线，
    // 断言合入不再推进它，而非钉死具体值）
    let local_before = get_personal_meta(&b, &entry_key).unwrap().unwrap();

    // A 推来同成员条目：管理员改 role=Admin、无 accessKey（秩高 ts）
    let remote_entry = member(&b_root, OrganizationRole::Admin);
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &entry_key,
            serde_json::to_value(&remote_entry).unwrap(),
            remote_meta("node-a", 3, NOW + 100),
        )],
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
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "org:member 键域过 B3 白名单"
    );

    let stored: OrganizationMember =
        serde_json::from_str(&b.get(&entry_key).unwrap().unwrap()).unwrap();
    assert_eq!(
        stored.access_key,
        Some(ak("b")),
        "accessKey 写一次守卫：本地保留"
    );
    assert_eq!(stored.role, OrganizationRole::Admin, "管理员字段组取秩高侧");
    let meta = get_personal_meta(&b, &entry_key).unwrap().unwrap();
    assert_eq!(meta.vv.get("node-a"), Some(&3), "合并 vv 并入远端分量");
    assert_eq!(
        meta.vv.get("node-b"),
        local_before.vv.get("node-b"),
        "合并是合入语义，本机分量不 bump"
    );
    assert_eq!(meta.ts, NOW + 100, "ts 取大");
    // 装配视图：get_record 读到条目覆盖后的成员
    let view = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    let m = view.find_member(&b_root).unwrap();
    assert_eq!(m.access_key, Some(ak("b")));
    assert_eq!(m.role, OrganizationRole::Admin);
    // 幂等：重放同一条目 → 判 Local（合并 vv 支配远端）不再写
    let before = b.get(&entry_key).unwrap().unwrap();
    let data_body2 = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &entry_key,
            serde_json::to_value(&remote_entry).unwrap(),
            remote_meta("node-a", 3, NOW + 100),
        )],
        0,
        1,
    );
    let r2 = deliver_orgsync(
        &mut b,
        &b_root,
        "B",
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body2,
        "peer-a",
        "node-b",
    );
    assert_eq!(r2.response, json!({ "ok": true }));
    assert_eq!(b.get(&entry_key).unwrap().unwrap(), before, "重放幂等");
}

/// P1 混跑兼容（设计 §6）：旧端只写 whole org:meta → 新端合入 whole 后
/// 就地投影出成员条目（远端语义：pmeta 复制 whole 的远端 meta，无本机
/// 分量）；装配视图与 whole 一致。
#[test]
fn legacy_whole_apply_projects_member_entries() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let mut b = MemoryStorage::new();
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();

    // 旧端 whole：b_root 改了 nickname（B 本地 whole 无 pmeta → Remote 覆盖）
    let mut whole = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    whole
        .members
        .iter_mut()
        .find(|m| m.root_id == b_root)
        .unwrap()
        .nickname = Some("旧端昵称".to_string());
    whole.updated_at = NOW + 50;
    let whole_meta = remote_meta("node-a", 7, NOW + 50);
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &format!("org:meta:{ORG_ID}"),
            serde_json::to_value(&whole).unwrap(),
            whole_meta.clone(),
        )],
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
    assert_eq!(r.response, json!({ "ok": true }));

    // 投影：两名成员各有条目，内容与 whole 同名成员逐字段等价
    for m in &whole.members {
        let raw = b
            .get(&org_member_key(ORG_ID, &m.root_id))
            .unwrap()
            .expect("whole 合入后就地投影出成员条目");
        assert_eq!(
            raw,
            serde_json::to_string(m).unwrap(),
            "投影内容与 whole 等价"
        );
        let meta = get_personal_meta(&b, &org_member_key(ORG_ID, &m.root_id))
            .unwrap()
            .unwrap();
        assert_eq!(meta.vv, whole_meta.vv, "投影 pmeta 复制 whole 远端 vv");
        assert_eq!(
            meta.vv.get("node-b"),
            None,
            "远端语义：无本机分量（防回声）"
        );
    }
    // 装配视图 == whole（投影一致）
    let view = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    assert_eq!(
        view.find_member(&b_root).unwrap().nickname.as_deref(),
        Some("旧端昵称")
    );
}

/// P1 踢出语义：成员移除 = 成员记录墓碑——whole（无 C）+ C 条目墓碑同批
/// 到达 → B 条目值删、pmeta 墓碑、装配视图排除 C（即使 whole 残留也排除）、
/// org 域 dlog 接力补登（个人域 dlog 不污染：org:member 是 orgsync 原生键）。
#[test]
fn member_removal_tombstone_propagates_and_excludes() {
    let (a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let (_c_key, c_root) = self_identity(3);
    let mut b = MemoryStorage::new();
    save_org(
        &mut b,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
            (c_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut b, ORG_ID, &b_root, NOW, "node-b")
        .unwrap();
    // B 本地已有 C 条目（无 pmeta → 远端墓碑判 Remote 干净落地）
    let c_entry_key = org_member_key(ORG_ID, &c_root);
    b.put(
        &c_entry_key,
        &serde_json::to_string(&member(&c_root, OrganizationRole::Member)).unwrap(),
    )
    .unwrap();

    // A 侧：whole 移除 C + C 条目墓碑（同批）
    let mut whole = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    whole.members.retain(|m| m.root_id != c_root);
    whole.updated_at = NOW + 60;
    let tomb_meta = DocMeta {
        tombstone: Some(true),
        ..remote_meta("node-a", 5, NOW + 60)
    };
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[
            org_record(
                &format!("org:meta:{ORG_ID}"),
                serde_json::to_value(&whole).unwrap(),
                remote_meta("node-a", 5, NOW + 60),
            ),
            spark_core::sync::orgsync::OrgsyncRecord {
                key: c_entry_key.clone(),
                value: serde_json::Value::Null,
                meta: tomb_meta,
                dseq: Some(1),
            },
        ],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut b,
        &b_root,
        &b_root,
        &a_key,
        &a_root,
        &b_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-a",
        "node-b",
    );
    assert_eq!(r.response, json!({ "ok": true }));

    // 条目：值删 + pmeta 墓碑
    assert!(b.get(&c_entry_key).unwrap().is_none(), "被踢成员条目值删除");
    let meta = get_personal_meta(&b, &c_entry_key).unwrap().unwrap();
    assert!(is_tombstone(&meta), "成员移除 = 成员记录墓碑");
    // 装配视图排除 C
    let view = OrganizationService::get_record(&b, ORG_ID)
        .unwrap()
        .unwrap();
    assert!(view.find_member(&c_root).is_none(), "装配视图排除被踢成员");
    assert_eq!(view.members.len(), 2);
    // org 域 dlog 接力补登（A→B→C 传播）
    let entries = org_dlog_entries_after(&b, ORG_ID, STRUCT, V1, 0).unwrap();
    assert!(
        entries.iter().any(|(_, k)| k == &c_entry_key),
        "远端墓碑落地补登 org 域 dlog（接力）"
    );
    // 个人域 dlog 不污染（org:member 非存量键，org-dlog-only）
    let personal: Vec<_> = b.scan(&ScanOptions::prefix("dlog:entry:")).unwrap();
    assert!(personal.is_empty(), "org:member 墓碑只登 org 域 dlog");
}

/// F4 第二层 P2 挂点（评审补齐）+ 裁决 §10.2 精确触发：P2 join 主链路只产
/// org:member 条目流量（成员自写条目）——invitee 自写条目入站到达（合入
/// 记录 vv 含 invitee 本人设备分量 ⟹ 真走了接受编排）→ 邀请人侧 outbound
/// pending 对账标 accepted + OrgInviteUpdated 事件。（生产口径：vv 分量键
/// = 写入设备 peerId，与端点 peerId 同命名空间——本用例对齐为同值。）
#[test]
fn outbound_invite_reconciled_on_member_entry_arrival() {
    let (_a_key, a_root) = self_identity(1);
    let (c_key, c_root) = self_identity(3);
    let mut a = MemoryStorage::new();
    save_org(
        &mut a,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (c_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    // A 侧 outbound pending 邀请（A 邀 C）
    let out_record = spark_core::org::invite_record::OrgInviteRecord {
        id: "inv-a-c".to_string(),
        org_id: ORG_ID.to_string(),
        org_name: "t".to_string(),
        org_avatar: None,
        peer_root_id: c_root.clone(),
        peer_nickname: "C".to_string(),
        direction: spark_core::org::OrgInviteDirection::Outgoing,
        status: spark_core::org::OrgInviteStatus::Pending,
        invite_code: None,
        created_at: NOW,
        updated_at: NOW,
    };
    OrganizationService::put_invite_record(&mut a, &out_record).unwrap();

    // C 的自写条目（含端点，accept 编排第 4 步产物）经 orgsync 到达
    let entry_key = org_member_key(ORG_ID, &c_root);
    let mut c_entry = member(&c_root, OrganizationRole::Member);
    c_entry.node_info = Some(spark_core::org::types::OrganizationDeviceSet::from_single(
        spark_core::org::types::OrganizationNodeInfo {
            device_uid: Some("uid-c".to_string()),
            peer_id: Some("peer-c".to_string()),
            addresses: Vec::new(),
        },
    ));
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &entry_key,
            serde_json::to_value(&c_entry).unwrap(),
            // vv 分量 = 写入设备 peerId（生产 node_id 即 peerId）——裁决
            // §10.2 条件 3 命中 C 的已知端点
            remote_meta("peer-c", 1, NOW + 100),
        )],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &c_key,
        &c_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-c",
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }));

    // 对账触发：outbound pending → accepted + 事件
    let updated = OrganizationService::get_outgoing_invite(&a, ORG_ID, &c_root)
        .unwrap()
        .expect("outbound 记录存在");
    assert_eq!(
        updated.status,
        spark_core::org::OrgInviteStatus::Accepted,
        "条目入站到达触发对账：outbound 标 accepted"
    );
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, spark_core::p2p::P2pEvent::OrgInviteUpdated(_))),
        "OrgInviteUpdated 事件已发"
    );
}

/// 裁决 §10.2 负面对照：预录/中继类合入**不误标** accepted（vv 只含他人
/// 分量），后续 declined 回执正常落账（终态不重置不再被误标挡门）。
/// 覆盖两个挂点：org:meta 分支（whole 合入）与 org:member 分支（中继条目）。
#[test]
fn outbound_invite_not_marked_on_prerecord_or_relay_then_declined_lands() {
    let (_a_key, a_root) = self_identity(1);
    let (_b_key, b_root) = self_identity(2);
    let (c_key, c_root) = self_identity(3);
    let mut a = MemoryStorage::new();
    save_org(
        &mut a,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (b_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    // A 侧 outbound pending 邀请（A 邀 C；C 已由 addMember 预录进成员表——
    // 预录即携端点 peer-c，e2e join_org 形态）
    let mut whole = OrganizationService::get_record(&a, ORG_ID)
        .unwrap()
        .unwrap();
    whole.members.push({
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
    put_personal(
        &mut a,
        "node-a",
        &format!("org:meta:{ORG_ID}"),
        &serde_json::to_string(&whole).unwrap(),
        NOW,
    )
    .unwrap();
    let out_record = spark_core::org::invite_record::OrgInviteRecord {
        id: "inv-a-c".to_string(),
        org_id: ORG_ID.to_string(),
        org_name: "t".to_string(),
        org_avatar: None,
        peer_root_id: c_root.clone(),
        peer_nickname: "C".to_string(),
        direction: spark_core::org::OrgInviteDirection::Outgoing,
        status: spark_core::org::OrgInviteStatus::Pending,
        invite_code: None,
        created_at: NOW,
        updated_at: NOW,
    };
    OrganizationService::put_invite_record(&mut a, &out_record).unwrap();
    let invite_status = |s: &MemoryStorage| {
        OrganizationService::get_outgoing_invite(s, ORG_ID, &c_root)
            .unwrap()
            .map(|r| r.status)
    };

    // 1) org:meta 挂点：他人 whole 到达（中继，vv 只含 B 的分量）→ applied
    //    但 C 无本人分量 → 不误标
    let mut incoming_whole = whole.clone();
    incoming_whole.updated_at = NOW + 10;
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &format!("org:meta:{ORG_ID}"),
            serde_json::to_value(&incoming_whole).unwrap(),
            remote_meta("peer-b", 4, NOW + 10),
        )],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &_b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-b",
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    assert_eq!(
        invite_status(&a),
        Some(spark_core::org::OrgInviteStatus::Pending),
        "whole 中继合入（无 C 本人分量）不误标 accepted"
    );

    // 2) org:member 挂点：C 的预录条目（含端点 peer-c）经 B 中继到达——
    //    vv 只含 B 的分量（写入设备是 B 的中继来源/管理员双写产物）→ 不误标
    let entry_key = org_member_key(ORG_ID, &c_root);
    let prerecorded_entry = whole
        .members
        .iter()
        .find(|m| m.root_id == c_root)
        .unwrap()
        .clone();
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &entry_key,
            serde_json::to_value(&prerecorded_entry).unwrap(),
            remote_meta("peer-b", 5, NOW + 20),
        )],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &_b_key,
        &b_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-b",
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    assert_eq!(
        invite_status(&a),
        Some(spark_core::org::OrgInviteStatus::Pending),
        "预录条目中继合入（vv 无 C 分量）不误标 accepted"
    );

    // 3) declined 回执正常落账（误标曾凭「终态不重置」把 declined 挡在门外）
    let reply = dm_envelope::build_envelope(
        dm_envelope::KIND_ORG_INVITE_REPLY,
        &c_root,
        &a_root,
        NOW + 30,
        json!({ "orgId": ORG_ID, "accept": false, "nickname": "C" }),
        &c_key,
    );
    let r = spark_core::kernel::handle_inbound_dm(
        &mut a,
        &a_root,
        "A",
        reply,
        "peer-c",
        &std::collections::HashSet::new(),
        NOW + 30,
        "node-a",
        None,
    )
    .unwrap();
    assert_eq!(r.response, json!({ "ok": true }));
    assert_eq!(
        invite_status(&a),
        Some(spark_core::org::OrgInviteStatus::Declined),
        "未被误标 → declined 回执正常落账"
    );
}

/// 裁决 §10.2 org:meta 挂点正向：whole 记录携带 invitee 本人分量（legacy
/// join 形态——invitee 经快照合入 bump 本机分量后 whole 回流）→ 对账标
/// accepted。
#[test]
fn outbound_invite_reconciled_on_whole_with_invitee_component() {
    let (_a_key, a_root) = self_identity(1);
    let (_d_key, d_root) = self_identity(4);
    let mut a = MemoryStorage::new();
    save_org(
        &mut a,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (d_root.as_str(), OrganizationRole::Member),
        ],
        &[],
    );
    spark_core::plugindata::declare_builtin_org_collections(&mut a, ORG_ID, &a_root, NOW, "node-a")
        .unwrap();
    // D 预录携端点 peer-d
    let mut whole = OrganizationService::get_record(&a, ORG_ID)
        .unwrap()
        .unwrap();
    whole
        .members
        .iter_mut()
        .find(|m| m.root_id == d_root)
        .unwrap()
        .node_info = Some(spark_core::org::types::OrganizationDeviceSet::from_single(
        spark_core::org::types::OrganizationNodeInfo {
            device_uid: Some("uid-d".to_string()),
            peer_id: Some("peer-d".to_string()),
            addresses: Vec::new(),
        },
    ));
    OrganizationService::save_record(&mut a, &whole).unwrap();
    let out_record = spark_core::org::invite_record::OrgInviteRecord {
        id: "inv-a-d".to_string(),
        org_id: ORG_ID.to_string(),
        org_name: "t".to_string(),
        org_avatar: None,
        peer_root_id: d_root.clone(),
        peer_nickname: "D".to_string(),
        direction: spark_core::org::OrgInviteDirection::Outgoing,
        status: spark_core::org::OrgInviteStatus::Pending,
        invite_code: None,
        created_at: NOW,
        updated_at: NOW,
    };
    OrganizationService::put_invite_record(&mut a, &out_record).unwrap();

    // legacy join 回流：whole 携带 D 的本机分量（vv 键 = D 的设备 peerId）
    let mut incoming = whole.clone();
    incoming.updated_at = NOW + 50;
    let data_body = build_orgsync_data_batch(
        ORG_ID,
        COL_FULL,
        &[org_record(
            &format!("org:meta:{ORG_ID}"),
            serde_json::to_value(&incoming).unwrap(),
            remote_meta("peer-d", 1, NOW + 50),
        )],
        0,
        1,
    );
    let r = deliver_orgsync(
        &mut a,
        &a_root,
        "A",
        &_d_key,
        &d_root,
        &a_root,
        dm_envelope::KIND_ORGSYNC_DATA,
        data_body,
        "peer-d",
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let updated = OrganizationService::get_outgoing_invite(&a, ORG_ID, &d_root)
        .unwrap()
        .expect("outbound 记录存在");
    assert_eq!(
        updated.status,
        spark_core::org::OrgInviteStatus::Accepted,
        "whole 携 invitee 本人分量 → 对账标 accepted"
    );
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, spark_core::p2p::P2pEvent::OrgInviteUpdated(_))),
        "OrgInviteUpdated 事件已发"
    );
}
