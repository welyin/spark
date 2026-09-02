//! F7（org-invite-scope-fix）回归：org:invites 退出 orgsync（键碰撞 + 传播面
//! 错位）+ 入站 invite/invite-reply 显式 put_personal 记账 + 存量迁移清理。

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use spark_core::kernel::{dm_envelope, handle_inbound_dm};
use spark_core::org::invite_record::OrgInviteRecord;
use spark_core::org::{OrgInviteDirection, OrgInviteStatus, OrganizationService};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::{get_personal_meta, put_personal};

const ORG_ID: &str = "org_aaaabbbbccccdddd";
const NOW: i64 = 1_720_000_000_000;

/// 自身份：rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致）。
fn self_identity(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

/// 投递 org-invite / org-invite-reply 信封（from → to）。
fn deliver_invite(
    storage: &mut MemoryStorage,
    to_root: &str,
    from_key: &SigningKey,
    from_root: &str,
    kind: &str,
    body: serde_json::Value,
    node_id: &str,
) -> spark_core::kernel::InboundDmResult {
    let envelope = dm_envelope::build_envelope(kind, from_root, to_root, NOW, body, from_key);
    handle_inbound_dm(
        storage,
        to_root,
        "me",
        envelope,
        "peer-from",
        &HashSet::new(),
        NOW,
        node_id,
        None,
    )
    .unwrap()
}

fn invite_body(invite_id: &str, org_id: &str) -> serde_json::Value {
    json!({
        "inviteId": invite_id,
        "inviteCode": "code-xyz",
        "orgId": org_id,
        "orgName": "星火组织",
        "inviterNickname": "管理员",
    })
}

/// F7 靶心回归（单测级）：同一邀请人 A 邀 B、C——C 的入站记录显式记账
/// （pmeta per-node 序号）；泄漏的远端版本（B 的已接受记录，同键）经合入
/// **不覆盖** C 的 pending；B/C 侧记录不进任何 orgsync 面（折叠/增量/键域）。
#[test]
fn f7_inbound_invite_versioned_and_not_clobbered_by_leaked_remote() {
    let (a_key, a_root) = self_identity(1);
    let (_c_key, c_root) = self_identity(2);
    let mut c = MemoryStorage::new();

    // A 邀 C：入站落库 + 显式记账
    let r = deliver_invite(
        &mut c, &c_root, &a_key, &a_root,
        dm_envelope::KIND_ORG_INVITE, invite_body("inv-c", ORG_ID), "node-c",
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let key = format!("org:inv:in:{ORG_ID}:{a_root}");
    let record: OrgInviteRecord =
        serde_json::from_str(&c.get(&key).unwrap().expect("入站记录落库")).unwrap();
    assert_eq!(record.id, "inv-c");
    assert_eq!(record.status, OrgInviteStatus::Pending);
    let meta = get_personal_meta(&c, &key).unwrap().expect("显式记账：pmeta 存在");
    assert!(
        meta.vv.get("node-c").copied().unwrap_or(0) >= 1,
        "per-node 序号记账（本机 nodeId）"
    );
    assert_eq!(meta.node_id.as_deref(), Some("node-c"));

    // 泄漏的远端版本（修复前的实际覆盖形态：B 的 accepted 终态记录同键到达，
    // 且 C 本地无 pmeta → 无条件采纳）。现 C 有 pmeta：远端 vv {node-b:1} 与
    // 本地 {node-c:1} 并发、ts 更旧 → LWW 本地胜，C 的 pending 不被覆盖。
    let leaked = json!({
        "id": "inv-b", "orgId": ORG_ID, "orgName": "星火组织", "peerRootId": a_root,
        "peerNickname": "管理员", "direction": "incoming", "status": "accepted",
        "inviteCode": "code-b", "createdAt": NOW - 5000, "updatedAt": NOW - 5000,
    });
    let leaked_meta = DocMeta {
        vv: [("node-b".to_string(), 1)].into_iter().collect(),
        ts: NOW - 5000,
        node_id: Some("node-b".to_string()),
        ..Default::default()
    };
    let applied = spark_core::sync::apply_personal_remote_no_dlog(
        &mut c, &key, &leaked.to_string(), &leaked_meta,
    )
    .unwrap();
    assert!(!applied.did_apply(), "泄漏的远端旧版本不得覆盖本地 pending");
    let record: OrgInviteRecord = serde_json::from_str(&c.get(&key).unwrap().unwrap()).unwrap();
    assert_eq!(record.id, "inv-c", "C 的记录 id 不被换成 B 的");
    assert_eq!(record.status, OrgInviteStatus::Pending);

    // C 应答对账：按邀请 id 仍查得到（修复前被覆盖后报「组织邀请不存在」）
    let found = OrganizationService::find_incoming_invite_by_id(&c, "inv-c").unwrap();
    assert!(found.is_some(), "C 应答时入站记录可查");

    // 记录不进任何 orgsync 面：org:inv:* 不属任何内建集合键域
    assert!(spark_core::sync::orgsync::legacy_org_key_scope(&key).is_none());
    assert!(
        spark_core::sync::orgsync::collect_org_collection_vv(&c, ORG_ID, "org:invites", "1")
            .unwrap()
            .is_empty(),
        "org:invites 集合已不存在（折叠为空）"
    );
    assert!(
        spark_core::sync::orgsync::collect_org_incremental(
            &c, ORG_ID, "org:invites", "1", &Default::default(), 0,
        )
        .unwrap()
        .is_empty(),
        "inv: 记录不进 orgsync 增量"
    );

    // 应答回执（C → A）：A 侧出站记录状态流转同样显式记账（同一潜伏缺陷的
    // 另一落点）
    let mut a = MemoryStorage::new();
    let out_record = OrgInviteRecord {
        id: "inv-c".to_string(),
        org_id: ORG_ID.to_string(),
        org_name: "星火组织".to_string(),
        org_avatar: None,
        peer_root_id: c_root.clone(),
        peer_nickname: "待加入成员".to_string(),
        direction: OrgInviteDirection::Outgoing,
        status: OrgInviteStatus::Pending,
        invite_code: None,
        created_at: NOW,
        updated_at: NOW,
    };
    OrganizationService::put_invite_record(&mut a, &out_record).unwrap();
    let r = deliver_invite(
        &mut a, &a_root, &{
            // C 的签名钥
            let (c_key, _) = self_identity(2);
            c_key
        }, &c_root,
        dm_envelope::KIND_ORG_INVITE_REPLY,
        json!({ "orgId": ORG_ID, "accept": true, "nickname": "小C" }),
        "node-a",
    );
    assert_eq!(r.response, json!({ "ok": true }), "应答受理");
    let out_key = format!("org:inv:out:{ORG_ID}:{c_root}");
    let updated: OrgInviteRecord =
        serde_json::from_str(&a.get(&out_key).unwrap().unwrap()).unwrap();
    assert_eq!(updated.status, OrgInviteStatus::Accepted);
    assert_eq!(updated.peer_nickname, "小C");
    let out_meta = get_personal_meta(&a, &out_key).unwrap().expect("reply 流转显式记账");
    assert!(
        out_meta.vv.get("node-a").copied().unwrap_or(0) >= 1,
        "per-node 序号记账"
    );
}

/// 同构 out: 用例：两管理员邀同一人——双方出站记录各自留存本地（键在各自
/// 账号内唯一），且 `org:inv` pdsync category 保留（自设备同步面不动）。
#[test]
fn f7_outgoing_records_two_admins_stay_local_and_pdsync_kept() {
    let org = "org_1111222233334444";
    let target = "cd".repeat(32);
    let out_key = format!("org:inv:out:{org}:{target}");
    // 两个管理员各自账号存储各写各的出站记录
    for (node, tag) in [("node-m1", "m1"), ("node-m2", "m2")] {
        let mut s = MemoryStorage::new();
        let record = OrgInviteRecord {
            id: format!("inv-{tag}"),
            org_id: org.to_string(),
            org_name: "t".to_string(),
            org_avatar: None,
            peer_root_id: target.clone(),
            peer_nickname: "待加入成员".to_string(),
            direction: OrgInviteDirection::Outgoing,
            status: OrgInviteStatus::Pending,
            invite_code: None,
            created_at: NOW,
            updated_at: NOW,
        };
        OrganizationService::put_invite_record(&mut s, &record).unwrap();
        put_personal(&mut s, node, &out_key, &serde_json::to_string(&record).unwrap(), NOW)
            .unwrap();
        let stored: OrgInviteRecord =
            serde_json::from_str(&s.get(&out_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored.id, format!("inv-{tag}"), "各自出站记录留存（无互盖）");
        assert!(get_personal_meta(&s, &out_key).unwrap().is_some());
    }
    // pdsync 自设备同步面保留（F7 不动 personal 域）
    assert_eq!(
        spark_core::sync::pdsync::category_for_key(&format!("org:inv:in:{org}:x"))
            .map(|c| c.name),
        Some("org:inv")
    );
    assert_eq!(
        spark_core::sync::pdsync::category_for_key(&out_key).map(|c| c.name),
        Some("org:inv")
    );
}

/// F7 存量迁移（batch1 §3 修订：墓碑化复活窗口闭合）：预制 in: 记录 + pmeta
/// + org:invites 声明 → 迁移后 in: 本体清净 + 墓碑 pmeta（vv 不 bump）、声明
/// 墓碑化；out: 记录与其余声明保留；未迁移自设备推回旧记录（同 vv）→
/// Equal 拒收不复活；邀请人重发（bump 支配墓碑）→ 正常落库；二次执行幂等。
#[test]
fn f7_migration_cleans_leaked_incoming_and_tombstones_decl() {
    let mut s = MemoryStorage::new();
    // 预制：两条 in: 记录（一条自有 pending、一条泄漏 accepted——不可区分）
    // + 各自 pmeta
    for (inviter, tag) in [("aa".repeat(32), "self"), ("bb".repeat(32), "leaked")] {
        let key = format!("org:inv:in:{ORG_ID}:{inviter}");
        s.put(&key, &json!({"id": format!("inv-{tag}")}).to_string()).unwrap();
        put_personal(&mut s, "node-x", &key, &json!({"id": format!("inv-{tag}")}).to_string(), NOW)
            .unwrap();
    }
    // out: 记录（应保留）+ pmeta
    let out_key = format!("org:inv:out:{ORG_ID}:{}", "cd".repeat(32));
    put_personal(&mut s, "node-x", &out_key, &json!({"id": "inv-out"}).to_string(), NOW).unwrap();
    // org:invites 声明（应墓碑化）+ pmeta；org:contacts 声明（对照，不动）
    let invites_decl = format!("org:coll:{ORG_ID}:org:invites@v1");
    let contacts_decl = format!("org:coll:{ORG_ID}:org:contacts@v1");
    put_personal(&mut s, "node-x", &invites_decl, &json!({"name":"org:invites"}).to_string(), NOW).unwrap();
    put_personal(&mut s, "node-x", &contacts_decl, &json!({"name":"org:contacts"}).to_string(), NOW).unwrap();

    let (removed, tombstoned) =
        spark_core::org::service::migrate_org_invites_out_of_orgsync(&mut s, NOW + 1000).unwrap();
    assert_eq!(removed, 2, "两条 in: 记录删除");
    assert_eq!(tombstoned, 1, "org:invites 声明墓碑化");
    // in: 本体清净；pmeta 为墓碑（batch1 §3：复活窗口关闭的关键）
    assert!(
        s.scan(&spark_core::storage::ScanOptions::prefix("org:inv:in:"))
            .unwrap()
            .is_empty()
    );
    let tomb_key = format!("org:inv:in:{ORG_ID}:{}", "aa".repeat(32));
    let tomb_meta = get_personal_meta(&s, &tomb_key).unwrap().expect("墓碑 pmeta 存在");
    assert_eq!(tomb_meta.tombstone, Some(true));
    assert_eq!(tomb_meta.vv.get("node-x"), Some(&1), "墓碑保留既有 vv 分量不 bump");
    // 个人域 dlog 无条目（不登 dlog——Equal 拒收已闭合复活窗口）
    assert!(
        spark_core::sync::dlog::entries_after(&s, 0).unwrap().is_empty(),
        "迁移墓碑不登 dlog"
    );
    // out: 记录保留（出站是 inviter 自己的记账，不在泄漏面）
    assert!(s.get(&out_key).unwrap().is_some(), "out: 记录保留");
    // 声明墓碑化：记录删除 + pmeta 墓碑
    assert!(s.get(&invites_decl).unwrap().is_none());
    let decl_meta = get_personal_meta(&s, &invites_decl).unwrap().expect("声明墓碑 pmeta");
    assert_eq!(decl_meta.tombstone, Some(true));
    assert_eq!(decl_meta.vv.get("node-x"), Some(&4), "墓碑保留既有 vv 分量不 bump");
    // 对照：其余声明不动
    assert!(s.get(&contacts_decl).unwrap().is_some());
    let contacts_meta = get_personal_meta(&s, &contacts_decl).unwrap().unwrap();
    assert_ne!(contacts_meta.tombstone, Some(true));

    // 幂等：二次执行无操作（in: 前缀已空——墓碑 pmeta 不在 org:inv:in: 前缀
    // 扫描面）。注意须先于重发断言：重发重建的 in: 记录会被迁移再清（迁移
    // 是升级一次性动作，先于新邀请到达运行）。
    let (r2, t2) =
        spark_core::org::service::migrate_org_invites_out_of_orgsync(&mut s, NOW + 2000).unwrap();
    assert_eq!((r2, t2), (0, 0), "二次执行幂等");

    // 未迁移自设备推回旧记录（同 vv）→ Equal 拒收、本地不复活
    let leaked_meta = DocMeta {
        vv: [("node-x".to_string(), 1)].into_iter().collect(),
        ts: NOW,
        node_id: Some("node-x".to_string()),
        ..Default::default()
    };
    let applied = spark_core::sync::apply_personal_remote_no_dlog(
        &mut s, &tomb_key, &json!({"id":"inv-self"}).to_string(), &leaked_meta,
    )
    .unwrap();
    assert_eq!(applied, spark_core::sync::ApplyResult::Equal, "同 vv 旧记录 Equal 拒收");
    assert!(s.get(&tomb_key).unwrap().is_none(), "本地不复活");

    // 邀请人重发（入站写 bump 支配墓碑）→ 正常落库
    let resent = put_personal(&mut s, "node-x", &tomb_key, &json!({"id":"inv-new"}).to_string(), NOW + 3000).unwrap();
    assert_eq!(resent.tombstone, None, "重发记录非墓碑");
    assert!(s.get(&tomb_key).unwrap().is_some(), "重发正常落库");
}
