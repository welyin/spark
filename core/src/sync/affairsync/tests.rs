//! affairsync 应用链路集成单测：本地写入 → 采集 → 对端应用的端到端纯逻辑
//! 链路，覆盖乱序暂存/drain、白名单红线、关注门槛与主持人门槛。

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};

use super::apply::{apply_affairsync_records, ingest_local_entries};
use super::collect::{collect_affair_records, collect_affair_vv};
use super::dir::follower_hints;
use super::envelope::{
    AffairsyncRecord, build_affairsync_hello, parse_affairsync_data, parse_affairsync_need,
};
use super::follow::{follow_affair, is_following, unfollow_affair};
use super::inbound::{handle_affairsync_hello, handle_affairsync_need};
use crate::affair::{affair_head_key, affair_op_key, affair_record_key};
use crate::storage::{MemoryStorage, StorageBackend};
use crate::sync::meta::DocMeta;

const NOW: i64 = 1_720_000_000_000;

struct FixedKey {
    signing_key: SigningKey,
    public_key: String,
    identity: String,
}

fn key_from_seed(seed_byte: u8) -> FixedKey {
    let signing_key = SigningKey::from_bytes(&[seed_byte; 32]);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    let public_key =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public_key_bytes);
    let identity = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(public_key_bytes));
    FixedKey {
        signing_key,
        public_key,
        identity,
    }
}

fn actor_json(key: &FixedKey) -> Value {
    json!({ "kind": "person", "identity": key.identity, "publicKey": key.public_key })
}

fn sign_payload(key: &FixedKey, payload: &str) -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        key.signing_key.sign(payload.as_bytes()).to_bytes(),
    )
}

fn make_genesis(initiator: &FixedKey) -> (Value, String) {
    let mut genesis = json!({
        "affairV": 1, "type": "forum", "title": "测试事务", "summary": "s", "tags": [],
        "initiator": actor_json(initiator),
        "rules": {
            "engine": "b1",
            "closeConditions": [{ "type": "op-count", "opType": "content", "count": 100 }],
            "pubPeriod": { "delayMs": 86400000 },
            "participation": { "combine": "all" },
            "ruleChange": { "kind": "delayed-veto", "delayMs": 259200000, "vetoThreshold": { "count": 3 } },
            "exec": null
        },
        "initialVoters": [initiator.identity], "refs": [], "createdAt": NOW,
    });
    let payload = crate::affair::genesis_sign_payload(&genesis).expect("genesis payload");
    genesis["sig"] = json!(sign_payload(initiator, &payload));
    let affair_id = crate::affair::compute_affair_id(&genesis).expect("affair id");
    (genesis, affair_id)
}

fn make_op(
    affair_id: &str,
    prev_op_hash: &str,
    op_type: &str,
    payload: Value,
    key: &FixedKey,
) -> (Value, String) {
    make_op_at(affair_id, prev_op_hash, op_type, payload, key, NOW)
}

fn make_op_at(
    affair_id: &str,
    prev_op_hash: &str,
    op_type: &str,
    payload: Value,
    key: &FixedKey,
    declared_at: i64,
) -> (Value, String) {
    let mut op = json!({
        "opV": 1, "affairId": affair_id, "prevOpHash": prev_op_hash,
        "opType": op_type, "payload": payload, "actor": actor_json(key),
        "declaredAt": declared_at,
    });
    let sign_input = crate::affair::op_sign_payload(&op).expect("op payload");
    op["sig"] = json!(sign_payload(key, &sign_input));
    let op_hash = crate::affair::compute_op_hash(&op).expect("op hash");
    (op, op_hash)
}

/// 从发送方存储采集增量组装 data 记录（走 collect 真实链路）。
fn collect_for_peer(
    sender: &MemoryStorage,
    affair_id: &str,
    peer_vv: &crate::sync::meta::VersionVector,
) -> Vec<AffairsyncRecord> {
    collect_affair_records(sender, affair_id, peer_vv).expect("collect")
}

/// A→B 一轮反熵：B 向 A 的摘要回 need，A 应答 data，B 应用。
fn replicate_round(
    a: &mut MemoryStorage,
    b: &mut MemoryStorage,
    affair_id: &str,
    root_a: &str,
    root_b: &str,
) -> super::apply::AffairApplySummary {
    let vv_a = collect_affair_vv(a, affair_id).unwrap();
    let heads_a: Vec<String> =
        serde_json::from_str::<Value>(&a.get(&affair_head_key(affair_id)).unwrap().unwrap())
            .unwrap()
            .get("heads")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
    let hello = build_affairsync_hello(affair_id, &vv_a, &heads_a, "pc");
    let out = handle_affairsync_hello(b, root_a, Some("peer-a"), &hello, NOW)
        .unwrap()
        .expect("B follows: hello handled");
    let need = out.need.expect("B behind: need returned");
    let data_bodies = handle_affairsync_need(a, root_b, Some("peer-b"), &need, NOW)
        .unwrap()
        .expect("A follows: need handled");
    assert!(!data_bodies.is_empty(), "A ahead: data returned");
    let mut total = super::apply::AffairApplySummary {
        affair_id: affair_id.to_string(),
        ..super::apply::AffairApplySummary::default()
    };
    for body in data_bodies {
        let (_, records) = parse_affairsync_data(&body).expect("parse data");
        let summary = apply_affairsync_records(b, affair_id, &records, NOW).unwrap();
        total.accepted += summary.accepted;
        total.duplicates += summary.duplicates;
        total.pending += summary.pending;
        total.rejected += summary.rejected;
        total.drained += summary.drained;
    }
    total
}

#[test]
fn local_ingest_collect_replicate_roundtrip() {
    let initiator = key_from_seed(0x11);
    let contributor = key_from_seed(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let (op1, op1_hash) = make_op(
        &affair_id,
        &affair_id,
        "content",
        json!({ "kind": "post", "text": "第一条" }),
        &contributor,
    );
    let (op2, op2_hash) = make_op(
        &affair_id,
        &op1_hash,
        "content",
        json!({ "kind": "post", "text": "第二条" }),
        &contributor,
    );

    // A（发起方）：关注 + 本地写入创世与两条操作
    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, NOW).unwrap();
    let summary =
        ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, op1, op2], NOW).unwrap();
    assert_eq!(summary.accepted, 3);
    assert_eq!(summary.rejected, 0);

    // B（关注者）：空副本经一轮反熵拉全量
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let applied = replicate_round(
        &mut a,
        &mut b,
        &affair_id,
        &initiator.identity,
        &contributor.identity,
    );
    assert_eq!(applied.accepted, 3, "genesis + 2 ops replicated");
    assert_eq!(applied.rejected, 0);
    assert!(b.get(&affair_record_key(&affair_id)).unwrap().is_some());
    assert!(
        b.get(&affair_op_key(&affair_id, &op1_hash))
            .unwrap()
            .is_some()
    );
    assert!(
        b.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_some()
    );
    // pmeta 落盘（反熵 vv 折叠的来源）
    let meta_key = crate::sync::personal_meta_key(&affair_op_key(&affair_id, &op1_hash));
    assert!(b.get(&meta_key).unwrap().is_some());
    // DAG 头：op2 是唯一头
    let heads_raw = b.get(&affair_head_key(&affair_id)).unwrap().unwrap();
    let heads = serde_json::from_str::<Value>(&heads_raw).unwrap();
    assert_eq!(
        heads["heads"].as_array().unwrap(),
        &vec![json!(op2_hash.clone())]
    );
    // 覆盖网线索：B 从入站 hello 学到 A（initiator）是关注者；
    // need 是 B 发起的，A 从入站 need 学到 B（contributor）——方向各归各的目录
    let hints_b = follower_hints(&b, &affair_id).unwrap();
    assert!(hints_b.iter().any(|h| h.root_id == initiator.identity));
    let hints_a = follower_hints(&a, &affair_id).unwrap();
    assert!(hints_a.iter().any(|h| h.root_id == contributor.identity));

    // A 追加 op3 → 第二轮增量反熵只带新操作
    let (op3, op3_hash) = make_op(
        &affair_id,
        &op2_hash,
        "content",
        json!({ "kind": "post", "text": "第三条" }),
        &contributor,
    );
    ingest_local_entries(&mut a, "node-a", &affair_id, &[op3], NOW).unwrap();
    let applied2 = replicate_round(
        &mut a,
        &mut b,
        &affair_id,
        &initiator.identity,
        &contributor.identity,
    );
    assert_eq!(applied2.accepted, 1);
    assert!(
        b.get(&affair_op_key(&affair_id, &op3_hash))
            .unwrap()
            .is_some()
    );
    let heads_raw = b.get(&affair_head_key(&affair_id)).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&heads_raw).unwrap()["heads"],
        json!([op3_hash])
    );
}

#[test]
fn out_of_order_op_stages_then_drains() {
    let initiator = key_from_seed(0x11);
    let contributor = key_from_seed(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let (op1, op1_hash) = make_op(&affair_id, &affair_id, "content", json!({}), &contributor);
    let (op2, op2_hash) = make_op(&affair_id, &op1_hash, "content", json!({}), &contributor);

    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, NOW).unwrap();
    ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, op1, op2], NOW).unwrap();

    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    // 先给 B 创世，再单独发 op2（乱序：prev 未知 → 暂存）
    let rec_record = AffairsyncRecord {
        key: affair_record_key(&affair_id),
        value: serde_json::from_str(&a.get(&affair_record_key(&affair_id)).unwrap().unwrap())
            .unwrap(),
        meta: serde_json::from_str(
            &a.get(&crate::sync::personal_meta_key(&affair_record_key(
                &affair_id,
            )))
            .unwrap()
            .unwrap(),
        )
        .unwrap(),
    };
    let s = apply_affairsync_records(&mut b, &affair_id, &[rec_record], NOW).unwrap();
    assert_eq!(s.accepted, 1);
    let op2_value: Value = serde_json::from_str(
        &a.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let op2_meta: DocMeta = serde_json::from_str(
        &a.get(&crate::sync::personal_meta_key(&affair_op_key(
            &affair_id, &op2_hash,
        )))
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let s = apply_affairsync_records(
        &mut b,
        &affair_id,
        &[AffairsyncRecord {
            key: affair_op_key(&affair_id, &op2_hash),
            value: op2_value,
            meta: op2_meta,
        }],
        NOW,
    )
    .unwrap();
    assert_eq!(s.pending, 1, "prev unknown: staged, not rejected");
    assert!(
        b.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_none()
    );

    // op1 到达 → drain 补入 op2
    let op1_value: Value = serde_json::from_str(
        &a.get(&affair_op_key(&affair_id, &op1_hash))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let op1_meta: DocMeta = serde_json::from_str(
        &a.get(&crate::sync::personal_meta_key(&affair_op_key(
            &affair_id, &op1_hash,
        )))
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let s = apply_affairsync_records(
        &mut b,
        &affair_id,
        &[AffairsyncRecord {
            key: affair_op_key(&affair_id, &op1_hash),
            value: op1_value,
            meta: op1_meta,
        }],
        NOW,
    )
    .unwrap();
    assert_eq!(s.accepted, 1);
    assert_eq!(s.drained, 1, "staged op2 drained after prev arrived");
    assert!(
        b.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_some()
    );
    // 暂存区已清空
    let pend = b
        .scan(&crate::storage::ScanOptions::prefix(
            &super::keys::affair_pend_prefix(&affair_id),
        ))
        .unwrap();
    assert!(pend.is_empty());
}

#[test]
fn local_staged_op_drains_after_prev_arrives() {
    let initiator = key_from_seed(0x11);
    let contributor = key_from_seed(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let (op1, op1_hash) = make_op(&affair_id, &affair_id, "content", json!({}), &contributor);
    let (op2, op2_hash) = make_op(&affair_id, &op1_hash, "content", json!({}), &contributor);

    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, NOW).unwrap();
    // 本地乱序写入：op2 的 prev（op1）未知 → 暂存（本地起源暂存线形无 meta）
    let s = ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, op2], NOW).unwrap();
    assert_eq!(s.accepted, 1, "genesis accepted");
    assert_eq!(s.pending, 1, "prev unknown: staged locally");
    assert!(
        a.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_none()
    );

    // prev 到达（本地补写 op1）→ 同批 drain 判 Accept：本地兜底生成 meta 入集，
    // 不得被静默删除（评审 C4 发现 1 的兜底分支钉住）
    let s = ingest_local_entries(&mut a, "node-a", &affair_id, &[op1], NOW).unwrap();
    assert_eq!(s.accepted, 1);
    assert_eq!(
        s.drained, 1,
        "locally staged op2 drained after prev arrived"
    );
    assert!(
        a.get(&affair_op_key(&affair_id, &op1_hash))
            .unwrap()
            .is_some()
    );
    assert!(
        a.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_some()
    );
    // 兜底 meta 落盘（本机 per-node 序号）
    let meta_key = crate::sync::personal_meta_key(&affair_op_key(&affair_id, &op2_hash));
    assert!(a.get(&meta_key).unwrap().is_some());
    // 暂存区清空
    let pend = a
        .scan(&crate::storage::ScanOptions::prefix(
            &super::keys::affair_pend_prefix(&affair_id),
        ))
        .unwrap();
    assert!(pend.is_empty());
}

#[test]
fn foreign_key_whole_batch_rejected() {
    let initiator = key_from_seed(0x11);
    let (genesis, affair_id) = make_genesis(&initiator);
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let poisoned = AffairsyncRecord {
        key: "org:meta:some-org".to_string(),
        value: genesis,
        meta: DocMeta::default(),
    };
    let s = apply_affairsync_records(&mut b, &affair_id, &[poisoned], NOW).unwrap();
    assert_eq!(s.rejected, 1, "B3 红线：org 键整批拒收");
    assert!(b.get(&affair_record_key(&affair_id)).unwrap().is_none());
}

#[test]
fn unfollowed_affair_data_rejected() {
    let initiator = key_from_seed(0x11);
    let (genesis, affair_id) = make_genesis(&initiator);
    let mut b = MemoryStorage::default();
    let record = AffairsyncRecord {
        key: affair_record_key(&affair_id),
        value: genesis,
        meta: DocMeta::default(),
    };
    let s = apply_affairsync_records(&mut b, &affair_id, &[record], NOW).unwrap();
    assert_eq!(s.rejected, 1, "未关注：整批拒收");
}

#[test]
fn tampered_genesis_rejected() {
    let initiator = key_from_seed(0x11);
    let (mut genesis, affair_id) = make_genesis(&initiator);
    genesis["title"] = json!("篡改标题");
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let record = AffairsyncRecord {
        key: affair_record_key(&affair_id),
        value: genesis,
        meta: DocMeta::default(),
    };
    let s = apply_affairsync_records(&mut b, &affair_id, &[record], NOW).unwrap();
    assert_eq!(s.rejected, 1, "verify_genesis 全链：篡改后验签/复算必败");
    assert!(b.get(&affair_record_key(&affair_id)).unwrap().is_none());
}

#[test]
fn non_moderator_moderate_rejected() {
    let initiator = key_from_seed(0x11);
    let stranger = key_from_seed(0x99);
    let (genesis, affair_id) = make_genesis(&initiator);
    let (content, content_hash) = make_op(&affair_id, &affair_id, "content", json!({}), &initiator);
    let (fold_op, fold_hash) = make_op(
        &affair_id,
        &content_hash,
        "moderate",
        json!({ "action": "fold", "target": content_hash }),
        &stranger,
    );
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, NOW).unwrap();
    ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, content], NOW).unwrap();
    let mut records = collect_for_peer(&a, &affair_id, &Default::default());
    records.push(AffairsyncRecord {
        key: affair_op_key(&affair_id, &fold_hash),
        value: fold_op,
        meta: DocMeta::default(),
    });
    let s = apply_affairsync_records(&mut b, &affair_id, &records, NOW).unwrap();
    assert_eq!(s.accepted, 2, "genesis + content accepted");
    assert_eq!(s.rejected, 1, "§11: stranger moderate rejected");
    assert!(
        b.get(&affair_op_key(&affair_id, &fold_hash))
            .unwrap()
            .is_none()
    );
}

#[test]
fn unfollow_keeps_replicated_data() {
    let initiator = key_from_seed(0x11);
    let (genesis, affair_id) = make_genesis(&initiator);
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let record = AffairsyncRecord {
        key: affair_record_key(&affair_id),
        value: genesis,
        meta: DocMeta::default(),
    };
    apply_affairsync_records(&mut b, &affair_id, &[record], NOW).unwrap();
    unfollow_affair(&mut b, &affair_id).unwrap();
    assert!(!is_following(&b, &affair_id).unwrap());
    assert!(
        b.get(&affair_record_key(&affair_id)).unwrap().is_some(),
        "取关不删除已复制的数据（affair 数据 append-only）"
    );
}

#[test]
fn hello_ignored_for_unfollowed_affair() {
    let mut b = MemoryStorage::default();
    let affair_id = "ab".repeat(32);
    let hello = build_affairsync_hello(&affair_id, &Default::default(), &[], "pc");
    let out = handle_affairsync_hello(&mut b, &"cd".repeat(32), None, &hello, NOW).unwrap();
    assert!(out.is_none(), "未关注：hello 静默丢弃（§7）");
    // need 同理（parse 后判关注）
    let need = super::envelope::build_affairsync_need(&affair_id, &Default::default());
    let _ = parse_affairsync_need(&need);
    let out = handle_affairsync_need(&mut b, &"cd".repeat(32), None, &need, NOW).unwrap();
    assert!(out.is_none());
}

#[test]
fn replication_exempts_declared_at_freshness() {
    // affair.md §3.1：复制入站豁免 declaredAt 新鲜度——历史操作补齐不被
    // ±10min 窗口锁死；保真由签名 + opHash 链承担。
    let initiator = key_from_seed(0x11);
    let contributor = key_from_seed(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let old = NOW - 30 * 86_400_000; // 30 天前，远超 ±10min 新鲜度窗口
    let (op1, op1_hash) = make_op_at(
        &affair_id,
        &affair_id,
        "content",
        json!({ "kind": "post", "text": "历史操作" }),
        &contributor,
        old,
    );

    // A：操作签发当时（old 时刻）本地写入，新鲜度内正常接受
    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, old).unwrap();
    let s = ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, op1], old).unwrap();
    assert_eq!(s.accepted, 2);

    // B：现在（NOW）才关注并复制，op 的 declaredAt 已超窗 30 天——豁免后仍接受
    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let records = collect_for_peer(&a, &affair_id, &Default::default());
    let s = apply_affairsync_records(&mut b, &affair_id, &records, NOW).unwrap();
    assert_eq!(s.accepted, 2, "genesis + 超窗历史操作经复制入集");
    assert_eq!(s.rejected, 0);
    assert!(
        b.get(&affair_op_key(&affair_id, &op1_hash))
            .unwrap()
            .is_some()
    );
}

#[test]
fn local_submit_enforces_declared_at_freshness() {
    // 新鲜度门槛只压实时提交：本地写入超窗 declaredAt 仍被拒（防时间造假）。
    let initiator = key_from_seed(0x11);
    let (genesis, affair_id) = make_genesis(&initiator);
    let stale = NOW - 30 * 86_400_000;
    let (op1, _) = make_op_at(
        &affair_id,
        &affair_id,
        "content",
        json!({ "kind": "post", "text": "补签的旧操作" }),
        &initiator,
        stale,
    );

    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, NOW).unwrap();
    ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis], NOW).unwrap();
    let err = ingest_local_entries(&mut a, "node-a", &affair_id, &[op1], NOW)
        .expect_err("实时提交超窗 declaredAt 必须拒绝");
    assert!(
        err.to_string().contains("verify-op-failed"),
        "拒收原因应来自 verify_op 新鲜度门槛: {err}"
    );
}

#[test]
fn stale_staged_op_drains_after_prev_arrives() {
    // drain 复检同样豁免新鲜度：超窗历史操作乱序到达（prev 未知→暂存），
    // prev 补齐后 drain 接受，不因暂存驻留期间超窗被丢弃。
    let initiator = key_from_seed(0x11);
    let contributor = key_from_seed(0x21);
    let (genesis, affair_id) = make_genesis(&initiator);
    let old = NOW - 30 * 86_400_000;
    let (op1, op1_hash) = make_op_at(
        &affair_id,
        &affair_id,
        "content",
        json!({}),
        &contributor,
        old,
    );
    let (op2, op2_hash) = make_op_at(
        &affair_id,
        &op1_hash,
        "content",
        json!({}),
        &contributor,
        old,
    );

    let mut a = MemoryStorage::default();
    follow_affair(&mut a, &affair_id, old).unwrap();
    ingest_local_entries(&mut a, "node-a", &affair_id, &[genesis, op1, op2], old).unwrap();

    let mut b = MemoryStorage::default();
    follow_affair(&mut b, &affair_id, NOW).unwrap();
    let record_of = |a: &MemoryStorage, key: &str| AffairsyncRecord {
        key: key.to_string(),
        value: serde_json::from_str(&a.get(key).unwrap().unwrap()).unwrap(),
        meta: serde_json::from_str(
            &a.get(&crate::sync::personal_meta_key(key))
                .unwrap()
                .unwrap(),
        )
        .unwrap(),
    };
    // 创世 + 乱序的 op2（prev 未知 → 暂存；此刻 declaredAt 已超窗）
    let s = apply_affairsync_records(
        &mut b,
        &affair_id,
        &[
            record_of(&a, &affair_record_key(&affair_id)),
            record_of(&a, &affair_op_key(&affair_id, &op2_hash)),
        ],
        NOW,
    )
    .unwrap();
    assert_eq!(s.accepted, 1, "genesis accepted");
    assert_eq!(s.pending, 1, "op2 prev unknown: staged");
    // op1 到达 → drain 复检豁免新鲜度，op2 入集
    let s = apply_affairsync_records(
        &mut b,
        &affair_id,
        &[record_of(&a, &affair_op_key(&affair_id, &op1_hash))],
        NOW,
    )
    .unwrap();
    assert_eq!(s.accepted, 1);
    assert_eq!(s.drained, 1, "超窗暂存 op2 在 prev 到达后 drain 入集");
    assert!(
        b.get(&affair_op_key(&affair_id, &op2_hash))
            .unwrap()
            .is_some()
    );
}
