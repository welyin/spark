//! M1/M2 设备通知与设备撤销内核集成测试。
//!
//! 覆盖方案文档 §6：
//! - M2 §6.5  revoke_device 正常流（revokedAt 落库、self FriendRecord peer
//!    清除/heal、PriorityPeerStore 移除、DeviceUpdated 事件带 revokedAt）。
//! - M2 §6.6  本机保护（peerId/deviceUid 命中均拒；Device not found；锁定态）。
//! - M2 §6.7  撤销粘性：revoked 后 upsert_self 保留 revokedAt。
//! - M2 §6.8  LWW：revoked 记录覆盖正常；更旧正常记录不覆盖标记。
//! - M2 §6.12 安全日志：两条、字段完整、键递增；security_log_list 可读/倒序。
//! - M2 §6.13 serde 兼容：无 revokedAt 旧 JSON → None；带字段往返无损。
//! - M1 §6.2  handle_device_notice：from==me → 恰好一次 DeviceNoticeReceived；
//!    from!=me / 缺 kind / 缺 deviceId → 静默无事件；且不产生任何
//!    DeviceRecord/FriendRecord 存储副作用（P0 关注点）。
//!
//! 纯内核层（`handle_inbound_dm` / `DeviceService` / `ContactService` /
//! `Kernel::revoke_device` + `__test_storage`），不 mock。

mod common;

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::contact::{ContactService, FriendRecord, FriendRequestRecord, FriendRequestStatus};
use spark_core::device::{DeviceRecord, DeviceService};
use spark_core::kernel::{Kernel, dm_envelope, handle_inbound_dm};
use spark_core::message::PeerRef;
use spark_core::p2p::P2pEvent;
use spark_core::p2p::priority_peers::PriorityPeerStore;
use spark_core::p2p::node::system_now_ms;
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::pdsync::{PdsyncRecord, build_data_batch};

use common::*;

const NOW: i64 = 1_720_000_000_000;
/// pdsync 版本向量节点 id（无 p2p 节点时用 local-node）。
const NODE: &str = "local-node";

fn peer_root(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

/// 构造任意设备记录（供 seeding，字段齐全）。
fn device_record(peer_id: &str, uid: &str, updated_at: i64, revoked: Option<i64>) -> DeviceRecord {
    DeviceRecord {
        peer_id: peer_id.to_string(),
        device_uid: Some(uid.to_string()),
        device_name: "对端设备".to_string(),
        os: "Android".to_string(),
        os_version: "14".to_string(),
        arch: "aarch64".to_string(),
        macs: Vec::new(),
        app_version: String::new(),
        updated_at,
        last_seen_at: updated_at,
        revoked_at: revoked,
        device_pub_key: None,
    }
}

/// 构造 self FriendRecord（peer 指向给定 peerId）。
fn self_friend(root_id: &str, peer_id: &str, updated_at: i64) -> FriendRecord {
    FriendRecord {
        root_id: root_id.to_string(),
        nickname: "我".to_string(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: updated_at,
        peers: vec![PeerRef {
            peer_id: peer_id.to_string(),
            addresses: Vec::new(),
            ..Default::default()
        }],
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked: false,
        updated_at,
    }
}

// ---------------------------------------------------------------------------
// M2 §6.13 serde 兼容：无 revokedAt 旧 JSON → None；带字段往返无损。
// ---------------------------------------------------------------------------

#[test]
fn device_record_serde_revoked_at_compat() {
    // 旧版本 JSON：无 revokedAt 字段 → 反序列化为 None（向后兼容）。
    let old_json = json!({
        "peerId": "peer-old",
        "deviceUid": "uid-old",
        "deviceName": "旧设备",
        "os": "Android",
        "osVersion": "14",
        "arch": "aarch64",
        "macs": [],
        "appVersion": "1.0.0",
        "updatedAt": 1000,
        "lastSeenAt": 1000,
    });
    let parsed: DeviceRecord = serde_json::from_str(&old_json.to_string()).expect("旧 JSON 可解析");
    assert!(parsed.revoked_at.is_none(), "缺 revokedAt 字段应反序列化为 None");
    assert_eq!(parsed.peer_id, "peer-old");

    // 带 revokedAt 字段往返无损。
    let rec = device_record("peer-b", "uid-b", 2000, Some(123456789));
    let round: DeviceRecord =
        serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
    assert_eq!(round.revoked_at, Some(123456789));
    assert_eq!(round.peer_id, "peer-b");
    assert_eq!(round.device_uid.as_deref(), Some("uid-b"));

    // 明确 null → None。
    let null_json = json!({
        "peerId": "peer-n",
        "deviceUid": "uid-n",
        "deviceName": "N",
        "os": "os",
        "osVersion": "v",
        "arch": "a",
        "macs": [],
        "appVersion": "v",
        "updatedAt": 1,
        "lastSeenAt": 1,
        "revokedAt": null
    });
    let parsed: DeviceRecord = serde_json::from_str(&null_json.to_string()).unwrap();
    assert!(parsed.revoked_at.is_none(), "revokedAt:null 应反序列化为 None");
}

// ---------------------------------------------------------------------------
// M1 §6.2 handle_device_notice 入站校验 + 无存储副作用。
// ---------------------------------------------------------------------------

/// 构造合法的自设备 system/device-notice（kind=device_joined）信封。
fn notice_envelope(key: &SigningKey, my_root: &str, body: Value) -> Value {
    dm_envelope::build_envelope("system/device-notice", my_root, my_root, NOW, body, key)
}

/// 断言 handle_device_notice 的分支行为与「无 DeviceRecord/FriendRecord 副作用」。
#[test]
fn handle_device_notice_validation_branches_and_no_side_effects() {
    let (key, my_root) = peer_root(1);
    let mut s = MemoryStorage::new();
    let empty = HashSet::new();

    // (a) from==me 且合法 → 恰好一次 DeviceNoticeReceived，字段齐。
    let body = json!({
        "kind": "device_joined",
        "deviceId": "peer-new-device",
        "deviceName": "我的新手机",
        "ts": NOW,
    });
    let envelope = notice_envelope(&key, &my_root, body);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &empty, NOW, NODE, None)
        .expect("合法自设备通知应 Ok");
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(result.events.len(), 1, "恰好一次事件");
    let P2pEvent::DeviceNoticeReceived(data) = &result.events[0] else {
        panic!("应发出 DeviceNoticeReceived 事件，实为 {:?}", result.events[0]);
    };
    assert_eq!(data["kind"], "device_joined");
    assert_eq!(data["deviceId"], "peer-new-device");
    assert_eq!(data["deviceName"], "我的新手机");
    assert_eq!(data["ts"], NOW);

    // 无 DeviceRecord 副作用：清单为空（本函数不落设备库）。
    let devices = DeviceService::list(&s).unwrap();
    assert!(
        devices.is_empty(),
        "handle_device_notice 不得产生任何 DeviceRecord 存储副作用"
    );
    // 无 FriendRecord 副作用：self 朋友记录不存在。
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_none(),
        "handle_device_notice 不得产生任何 FriendRecord 存储副作用"
    );

    // (b) from != me → 静默无事件（response ok:false，但不 emit）。
    let (other_key, other_root) = peer_root(2);
    // 用 other_key 签名、from=other_root、to=my_root：验签通过但 from!=me。
    let envelope = dm_envelope::build_envelope(
        "system/device-notice",
        &other_root,
        &my_root,
        NOW,
        json!({
            "kind": "device_joined", "deviceId": "peer-x", "deviceName": "x", "ts": NOW,
        }),
        &other_key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-b", &empty, NOW, NODE, None)
        .expect("非自身份应静默 Ok");
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-self" }));
    assert!(result.events.is_empty(), "from!=me 不得 emit 任何事件");

    // (c) from==me 但缺 kind → 静默无事件。
    let envelope = notice_envelope(&key, &my_root, json!({
        "deviceId": "peer-x", "deviceName": "x", "ts": NOW,
    }));
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-b", &empty, NOW, NODE, None)
        .expect("缺 kind 应静默 Ok");
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-kind" }));
    assert!(result.events.is_empty(), "缺 kind 不得 emit 事件");

    // (d) from==me 但 kind 非 device_joined → 静默。
    let envelope = notice_envelope(&key, &my_root, json!({
        "kind": "device_revoked", "deviceId": "peer-x", "ts": NOW,
    }));
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-b", &empty, NOW, NODE, None)
        .expect("非 device_joined 应静默 Ok");
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-kind" }));
    assert!(result.events.is_empty());

    // (e) from==me 但缺 deviceId / 空 deviceId → 静默。
    let envelope = notice_envelope(&key, &my_root, json!({
        "kind": "device_joined", "deviceName": "x", "ts": NOW,
    }));
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-b", &empty, NOW, NODE, None)
        .expect("缺 deviceId 应静默 Ok");
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-deviceId" }));
    assert!(result.events.is_empty());

    let envelope = notice_envelope(&key, &my_root, json!({
        "kind": "device_joined", "deviceId": "   ", "deviceName": "x", "ts": NOW,
    }));
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-b", &empty, NOW, NODE, None)
        .expect("空 deviceId 应静默 Ok");
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-deviceId" }));
    assert!(result.events.is_empty());

    // 全部校验分支后仍无任何存储副作用。
    assert!(
        DeviceService::list(&s).unwrap().is_empty(),
        "全部校验分支后不得有 DeviceRecord 副作用"
    );
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_none(),
        "全部校验分支后不得有 FriendRecord 副作用"
    );
}

// ---------------------------------------------------------------------------
// M2 §6.7 撤销粘性 + §6.8 LWW。
// ---------------------------------------------------------------------------

#[test]
fn upsert_self_preserves_revoked_at_stickiness() {
    let mut s = MemoryStorage::new();
    // 先写入本机记录并标记撤销（模拟被撤销设备）。
    DeviceService::upsert_self(&mut s, "peer-self", 100, NODE, "1.0.0", None).unwrap();
    let uid = DeviceService::get(&s, "peer-self").unwrap().unwrap().device_uid;
    DeviceService::mark_revoked(&mut s, uid.as_deref().unwrap(), 200, 200, NODE).unwrap();

    // 被撤销设备重启 → upsert_self 重写自己的记录（updated_at 刷新），
    // revokedAt 必须保留（粘性），否则自我洗白黑名单。
    let rec = DeviceService::upsert_self(&mut s, "peer-self", 300, NODE, "1.0.0", None).unwrap();
    assert_eq!(rec.revoked_at, Some(200), "upsert_self 必须保留 revokedAt");
    assert_eq!(rec.updated_at, 300, "updated_at 刷新为重启时间");
    // 其外发记录（upsert_pdsync 序列化）仍带标记。
    let stored = DeviceService::get(&s, "peer-self").unwrap().unwrap();
    assert_eq!(stored.revoked_at, Some(200));
}

#[test]
fn device_lww_revoked_record_prevails() {
    let mut s = MemoryStorage::new();
    // 对端正常记录（updated_at=100）。
    let normal = device_record("peer-b", "uid-b", 100, None);
    DeviceService::upsert_pdsync(&mut s, &normal, 100, NODE).unwrap();
    assert!(DeviceService::get(&s, "peer-b").unwrap().unwrap().revoked_at.is_none());

    // revoked 记录（updated_at=200，新）→ 覆盖正常记录。
    let revoked = device_record("peer-b", "uid-b", 200, Some(200));
    let (applied, changed) = DeviceService::apply_remote(&mut s, revoked, 200, "peer-b", NODE).unwrap();
    assert!(changed, "新 revoked 记录应判定为内容变更");
    assert_eq!(applied.revoked_at, Some(200));
    assert_eq!(DeviceService::get(&s, "peer-b").unwrap().unwrap().revoked_at, Some(200));

    // 更旧的正常记录（updated_at=150 < 200）到达 → 不覆盖标记。
    let older_normal = device_record("peer-b", "uid-b", 150, None);
    let (applied, changed) = DeviceService::apply_remote(&mut s, older_normal, 201, "peer-b", NODE).unwrap();
    assert!(!changed, "更旧正常记录不得判定为内容变更");
    // apply_remote 不覆盖已撤销标记（本地 revoked 粘性：远端正常记录无法洗白）。
    assert_eq!(
        DeviceService::get(&s, "peer-b").unwrap().unwrap().revoked_at,
        Some(200),
        "更旧正常记录不得覆盖撤销标记"
    );
    // changed=false 时返回值保留本地最新（含 revokedAt）。
    assert_eq!(applied.revoked_at, Some(200));
}

// ---------------------------------------------------------------------------
// M2 §6.5 / §6.6 / §6.12 revoke_device 完整流（真 kernel + 真存储）。
// ---------------------------------------------------------------------------

fn kernel_with_p2p(dir: &tempfile::TempDir) -> Kernel {
    let mut k = Kernel::init(config(dir.path())).expect("kernel init");
    let (_root, _mnemonic) = init_identity(&mut k);
    k.start_p2p().expect("start_p2p");
    k
}

/// 仅初始化身份、不开 p2p 的内核（离线场景：storage 有私钥可推导 peerId）。
fn kernel_identity_only(dir: &tempfile::TempDir) -> Kernel {
    let mut k = Kernel::init(config(dir.path())).expect("kernel init");
    let (_root, _mnemonic) = init_identity(&mut k);
    k
}

fn local_peer_id(k: &Kernel) -> String {
    k.p2p_status()
        .unwrap()
        .expect("p2p started")
        .peer_id
        .expect("peer_id present")
}

/// 在真 kernel 中 seed 一台目标设备（friend 寻址 + 设备记录 + 优先集合），
/// 返回 (device_uid, target_peer_id)。
fn seed_target_device(k: &mut Kernel, root_id: &str, target_peer: &str) -> String {
    let storage = k.__test_storage().unwrap();
    // 目标设备记录（带 deviceUid）。
    let uid = format!("uid-{target_peer}");
    DeviceService::upsert_pdsync(
        &mut storage.clone(),
        &device_record(target_peer, &uid, system_now_ms(), None),
        system_now_ms(),
        NODE,
    )
    .unwrap();
    // self FriendRecord 指向该设备（撤销后应被清除/heal）。
    ContactService::upsert_friend_pdsync(
        &mut storage.clone(),
        &self_friend(root_id, target_peer, system_now_ms()),
        system_now_ms(),
        NODE,
    )
    .unwrap();
    // 优先恢复集合。
    PriorityPeerStore::new(&mut storage.clone())
        .add(target_peer)
        .unwrap();
    uid
}

#[test]
fn revoke_device_normal_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = kernel_with_p2p(&dir);
    let root_id = k.current_root_id().unwrap().expect("root id");
    let local = local_peer_id(&k);
    let target_peer = "peer-to-revoke";
    let uid = seed_target_device(&mut k, &root_id, target_peer);

    // 撤销前基线：优先集合含目标。
    let before: Vec<String> = {
        let mut s = k.__test_storage().unwrap();
        PriorityPeerStore::new(&mut s).list().unwrap()
    };
    assert!(before.contains(&target_peer.to_string()), "撤销前应在优先集合");

    // 订阅事件后再撤销，捕获 DeviceUpdated（带 revokedAt）。
    let mut rx = k.subscribe_p2p_events();
    k.revoke_device(target_peer).expect("revoke ok");
    // revoke 内部同步 send DeviceUpdated 到 event_tx，应已入缓冲。
    let updated_event = rx.try_recv();
    match updated_event {
        Ok(P2pEvent::DeviceUpdated(data)) => {
            assert_eq!(data["peerId"], target_peer);
            assert!(data["revokedAt"].is_number(), "DeviceUpdated 事件带 revokedAt");
        }
        other => panic!("应收到 DeviceUpdated 事件，实为 {other:?}"),
    }

    // (1) revokedAt 落库。
    let rec = DeviceService::get(&k.__test_storage().unwrap(), target_peer)
        .unwrap()
        .expect("目标记录仍保留");
    assert!(rec.revoked_at.is_some(), "revokedAt 已落库");
    assert_eq!(rec.device_uid.as_deref(), Some(uid.as_str()));

    // (2) self FriendRecord peer 已清除/heal（不再指向被撤销设备）。
    let self_friend = ContactService::get_friend(&k.__test_storage().unwrap(), &root_id)
        .unwrap()
        .expect("self friend 存在");
    let peer_ids: Vec<String> = self_friend
        .peers
        .iter()
        .map(|p| p.peer_id.clone())
        .collect();
    assert!(
        !peer_ids.contains(&target_peer.to_string()),
        "self FriendRecord 不得再指向被撤销设备"
    );

    // (3) PriorityPeerStore 已移除该设备。
    let after: Vec<String> = {
        let mut s = k.__test_storage().unwrap();
        PriorityPeerStore::new(&mut s).list().unwrap()
    };
    assert!(
        !after.contains(&target_peer.to_string()),
        "撤销后不得在优先集合"
    );

    // (4) 寻址过滤：devices_list 视图带 revokedAt（撤销后清单可见但标记）。
    let views = k.devices_list().unwrap();
    let target_view = views.iter().find(|v| v.peer_id == target_peer).expect("清单含目标");
    assert!(target_view.revoked_at.is_some(), "devices_list 视图带 revokedAt");
    assert_eq!(target_view.is_self, false, "被撤销目标非本机");
    let _ = local;
}

#[test]
fn revoke_device_self_protection_and_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = kernel_with_p2p(&dir);
    let root_id = k.current_root_id().unwrap().expect("root id");
    let local = local_peer_id(&k);

    // (a) peerId 命中本机 → Cannot revoke current device。
    let err = k.revoke_device(&local).unwrap_err().to_string();
    assert!(
        err.contains("Cannot revoke current device"),
        "本机 peerId 拒绝，实为 {err}"
    );

    // (b) deviceUid 命中本机 → Cannot revoke current device。
    // 本机记录经 devices_list 兜底采集落库。
    let _ = k.devices_list().unwrap();
    let self_uid = DeviceService::get(&k.__test_storage().unwrap(), &local)
        .unwrap()
        .unwrap()
        .device_uid
        .unwrap();
    let err = k.revoke_device(&self_uid).unwrap_err().to_string();
    assert!(
        err.contains("Cannot revoke current device"),
        "本机 deviceUid 拒绝，实为 {err}"
    );

    // (c) 空 ID → 错误。
    let err = k.revoke_device("   ").unwrap_err().to_string();
    assert!(err.contains("deviceId is empty"), "空 ID 报错，实为 {err}");

    // (d) 不存在 → Device not found。
    let err = k.revoke_device("peer-does-not-exist").unwrap_err().to_string();
    assert!(err.contains("Device not found"), "未找到报错，实为 {err}");

    let _ = root_id;
}

#[test]
fn security_log_two_entries_and_sorted_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = kernel_with_p2p(&dir);
    let root_id = k.current_root_id().unwrap().expect("root id");
    let target_peer = "peer-log";
    seed_target_device(&mut k, &root_id, target_peer);

    k.revoke_device(target_peer).expect("revoke ok");

    // 安全日志经 security_log_list（壳层 inner 直调语义）断言：可读、倒序、limit 生效。
    let logs = k.security_log_list(None).unwrap();
    // 至少含 initiated + effective 两条针对该 deviceUid。
    let uid = DeviceService::get(&k.__test_storage().unwrap(), target_peer)
        .unwrap()
        .unwrap()
        .device_uid
        .unwrap();
    let kinds: Vec<String> = logs
        .iter()
        .map(|(key, val)| {
            let v: Value = serde_json::from_str(val).unwrap();
            let _ = key;
            v["kind"].as_str().unwrap_or("").to_string()
        })
        .collect();
    assert!(
        kinds.iter().any(|k| k == "device_revoke_initiated"),
        "应含 device_revoke_initiated，实为 {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "device_revoke_effective"),
        "应含 device_revoke_effective，实为 {kinds:?}"
    );
    // M3：撤销触发 epoch 轮换，安全日志追加 epoch 相关条目。
    assert!(
        kinds.iter().any(|k| k == "epoch_rotated"),
        "应含 epoch_rotated，实为 {kinds:?}"
    );
    // epoch_init/revoke/password_change 等冗余 kind 已删除，信息由 epoch_rotated.reason 承载。

    // 每条字段完整：kind/ts；device 类日志额外含 deviceId；initiated 额外含 deviceName/actor。
    for (_key, val) in &logs {
        let v: Value = serde_json::from_str(val).unwrap();
        assert!(v.get("kind").is_some(), "字段 kind 缺失");
        assert!(v.get("ts").is_some(), "字段 ts 缺失");
        let kind = v["kind"].as_str().unwrap_or("");
        if kind.starts_with("device_") {
            assert!(v.get("deviceId").is_some(), "device 类日志字段 deviceId 缺失");
        }
        if kind == "device_revoke_initiated" {
            assert!(v.get("deviceName").is_some(), "initiated 字段 deviceName 缺失");
            assert_eq!(v["actor"].as_str(), Some("local"), "initiated actor 应为 local");
        }
    }

    // 键倒序（键 = `security:log:{ts}:{kind}:{deviceId}`，ts 递增则扫描倒序）。
    let keys: Vec<&String> = logs.iter().map(|(k, _)| k).collect();
    let _uid = uid;
    // 倒序断言：ts 部分（第一个冒号后到第二个冒号）应单调不增。
    let ts_vals: Vec<i64> = keys
        .iter()
        .map(|key| {
            // `security:log:` 前缀剥离
            let rest = key.trim_start_matches("security:log:");
            rest.split(':').next().unwrap_or("0").parse::<i64>().unwrap_or(0)
        })
        .collect();
    for pair in ts_vals.windows(2) {
        assert!(pair[0] >= pair[1], "security_log_list 键应倒序（ts 单调不增）");
    }

    // limit 生效（方案 §6.12：先倒序再 truncate(limit)）。
    let limited = k.security_log_list(Some(1)).unwrap();
    assert_eq!(limited.len(), 1, "limit=1 只回最新一条");
    assert_eq!(limited[0].0, logs[0].0, "limit=1 应是最新（键序第一）那条");

    // limit=0 → 空。
    assert!(k.security_log_list(Some(0)).unwrap().is_empty(), "limit=0 回空");
}

// ---------------------------------------------------------------------------
// M1 golden vector：dm_envelope_vectors.rs 已覆盖 system/device-notice 往返。
// 这里补充：构造 + 本地验签一致。
// ---------------------------------------------------------------------------

#[test]
fn device_notice_envelope_roundtrip_signed() {
    let (key, my_root) = peer_root(9);
    let body = json!({
        "kind": "device_joined",
        "deviceId": "peer-golden",
        "deviceName": "金标手机",
        "ts": NOW,
    });
    let envelope = notice_envelope(&key, &my_root, body);
    assert_eq!(envelope["kind"], "system/device-notice");
    assert_eq!(envelope["from"], my_root);
    assert_eq!(envelope["to"], my_root);
    // 验签通过（verify_envelope 内部校验签名与 freshness）。
    let verified = dm_envelope::verify_envelope(&envelope, &my_root, NOW).expect("自建信封应验签通过");
    assert_eq!(verified.kind, "system/device-notice");
    // sender 侧 body 形状（§3.1/§3.2）：kind/deviceId/deviceName/ts 字段齐。
    assert_eq!(verified.body["kind"], "device_joined");
    assert_eq!(verified.body["deviceId"], "peer-golden");
    assert_eq!(verified.body["deviceName"], "金标手机");
    assert!(verified.body["ts"].is_number(), "body.ts 字段存在");
}

// ---------------------------------------------------------------------------
// M1 sender 侧触发（测试点 1）：friend-accept 自身份配对 → device_notice_broadcast
// 置位 + 打开 24h 补发窗。广播 body 形状/目标排除由 golden vector + addressing
// 单测覆盖；完整 async 广播（写 noticeSent 标记）经代码核对。
// ---------------------------------------------------------------------------

#[test]
fn friend_accept_self_pairs_sets_notice_broadcast_and_opens_window() {
    let (key, my_root) = peer_root(3);
    let mut s = MemoryStorage::new();
    let empty = HashSet::new();

    // seed 一条自身份外发请求（root_id==my_root、Pending），friend-accept 才有效。
    let req = FriendRequestRecord {
        id: "req-self-1".to_string(),
        root_id: my_root.clone(),
        nickname: "我".to_string(),
        avatar: None,
        message: String::new(),
        source: String::new(),
        status: FriendRequestStatus::Pending,
        created_at: NOW,
        updated_at: NOW,
        peer: None,
        thread: Vec::new(),
        invite_code: None,
    };
    ContactService::put_outgoing_request(&mut s, &req).unwrap();

    // friend-accept：from==my_root（自设备配对确认）。
    let body = json!({
        "requestId": "req-self-1",
        "nickname": "我",
        "nodeInfo": { "peerId": "peer-self-1", "addresses": [] },
    });
    let envelope = dm_envelope::build_envelope("friend-accept", &my_root, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-self-1", &empty, NOW, NODE, None)
        .expect("自身份 friend-accept 应 Ok");

    // sender 触发：device_notice_broadcast 置位（host 侧据此 spawn 广播）。
    assert!(result.device_notice_broadcast, "自身份配对应触发 device_notice_broadcast");
    // 补发窗打开：P2P_DEVICE_NOTICE_SELF_UNTIL 写入（NOW + 24h）。
    let until = s
        .get(spark_core::p2p::constants::P2P_DEVICE_NOTICE_SELF_UNTIL)
        .unwrap()
        .expect("补发窗键已写入");
    assert_eq!(until, (NOW + 24 * 60 * 60 * 1000).to_string(), "补发窗=NOW+24h");
}

// ---------------------------------------------------------------------------
// M2 兜底（返工项）：p2p 未启动时 revoke_device 不再报 "p2p not started"。
// 本机保护经 p2p:identity:privateKey 推导 peerId 离线生效。
// ---------------------------------------------------------------------------

#[test]
fn revoke_device_offline_without_p2p() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = kernel_identity_only(&dir);
    let root_id = k.current_root_id().unwrap().expect("root id");

    // seed libp2p 私钥 → 离线可推导本机 peerId（等价于 p2p 曾启动的持久化态）。
    {
        let mut s = k.__test_storage().unwrap();
        let keypair = spark_core::p2p::identity_store::get_or_create_libp2p_keypair(&mut s)
            .expect("seed keypair");
        drop(s);
        let local = spark_core::p2p::identity_store::load_peer_id(&k.__test_storage().unwrap())
            .expect("load peer id offline");
        assert!(!local.is_empty());
        // 本机保护（离线）：撤销自身 peerId 仍拒。
        let err = k.revoke_device(&local).unwrap_err().to_string();
        assert!(
            err.contains("Cannot revoke current device"),
            "离线本机保护应生效，实为 {err}"
        );
        let _ = keypair;
    }

    // 离线撤销一台目标设备：不报 "p2p not started"，成功 + 安全日志写入。
    let target_peer = "peer-offline-target";
    seed_target_device(&mut k, &root_id, target_peer);
    k.revoke_device(target_peer).expect("离线撤销应成功");

    // 撤销落库 + 安全日志两条写入。
    let rec = DeviceService::get(&k.__test_storage().unwrap(), target_peer)
        .unwrap()
        .expect("目标记录保留");
    assert!(rec.revoked_at.is_some(), "离线撤销 revokedAt 落库");
    let logs = k.security_log_list(None).unwrap();
    let kinds: Vec<String> = logs
        .iter()
        .map(|(_k, val)| {
            serde_json::from_str::<Value>(val).unwrap()["kind"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(kinds.contains(&"device_revoke_initiated".to_string()));
    assert!(kinds.contains(&"device_revoke_effective".to_string()));
}

// ---------------------------------------------------------------------------
// 方案 §6-7b pdsync 撤销粘性合并：device: 键经 handle_pdsync_data，
// 本地已撤销 + 远端快照无 revokedAt → 合入后本地 revokedAt 保留（防洗白）。
// ---------------------------------------------------------------------------

#[test]
fn pdsync_device_sticky_merge_preserves_local_revoked_at() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = peer_root(4);
    let peer_id = "peer-sticky";

    // 本地已撤销该设备（mark_revoked(uid, revoked_at, now_ms, node)）。
    DeviceService::upsert_self(&mut s, peer_id, 100, NODE, "1.0.0", None).unwrap();
    let uid = DeviceService::get(&s, peer_id).unwrap().unwrap().device_uid;
    DeviceService::mark_revoked(&mut s, uid.as_deref().unwrap(), 200, 200, NODE).unwrap();
    assert_eq!(
        DeviceService::get(&s, peer_id).unwrap().unwrap().revoked_at,
        Some(200)
    );

    // 远端快照：无 revokedAt 的干净设备记录（对端未同步到撤销）。
    let remote = device_record(peer_id, "uid-sticky", 300, None);
    let meta = DocMeta {
        vv: [(NODE.to_string(), 5)].into_iter().collect(),
        ts: 300,
        node_id: Some(NODE.to_string()),
        tombstone: None,
    };
    let record = PdsyncRecord {
        key: format!("device:{peer_id}"),
        value: serde_json::to_value(&remote).unwrap(),
        meta,
        dseq: None,
    };
    let body = build_data_batch("device", &[record], 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        &my_root,
        &my_root,
        NOW,
        body,
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-self-b", &HashSet::new(), NOW, NODE, None)
        .expect("pdsync-data 应 Ok");
    assert_eq!(result.response, json!({ "ok": true }));

    // 粘性：本地 revokedAt 保留，未被远端无标记快照洗白。
    let after = DeviceService::get(&s, peer_id).unwrap().unwrap();
    assert_eq!(
        after.revoked_at,
        Some(200),
        "本地已撤销 + 远端无 revokedAt → 本地 revokedAt 必须保留"
    );
}

// ---------------------------------------------------------------------------
// 方案 §6-7c 复现链（单存储内验证撤销粘性穿透 pdsync 回流）：
// A 撤销 B 后，B 的干净记录经 pdsync 回流，A 的 revokedAt 仍在。
// ---------------------------------------------------------------------------

#[test]
fn revoke_then_clean_pdsync_reflow_keeps_revoked_at() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = peer_root(5);
    let peer_id = "peer-reflow";

    // 先落一条干净目标记录，再「A 撤销 B」。
    DeviceService::upsert_self(&mut s, peer_id, 100, NODE, "1.0.0", None).unwrap();
    let uid = DeviceService::get(&s, peer_id).unwrap().unwrap().device_uid;
    DeviceService::mark_revoked(&mut s, uid.as_deref().unwrap(), 500, 500, NODE).unwrap();
    assert_eq!(DeviceService::get(&s, peer_id).unwrap().unwrap().revoked_at, Some(500));

    // B 的干净新记录（无 revokedAt，updated_at 更晚）经 pdsync 回流。
    let clean = device_record(peer_id, "uid-reflow", 900, None);
    let meta = DocMeta {
        vv: [(NODE.to_string(), 9)].into_iter().collect(),
        ts: 900,
        node_id: Some(NODE.to_string()),
        tombstone: None,
    };
    let record = PdsyncRecord {
        key: format!("device:{peer_id}"),
        value: serde_json::to_value(&clean).unwrap(),
        meta,
        dseq: None,
    };
    let body = build_data_batch("device", &[record], 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        &my_root,
        &my_root,
        NOW,
        body,
        &key,
    );
    handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-self-b", &HashSet::new(), NOW, NODE, None)
        .expect("pdsync 回流应 Ok");

    // A 的 revokedAt 仍在（is_revoked_peer 恒 true 的底层保证）。
    assert_eq!(
        DeviceService::get(&s, peer_id).unwrap().unwrap().revoked_at,
        Some(500),
        "A 撤销后 B 干净记录回流不得洗白 revokedAt"
    );
}

// ---------------------------------------------------------------------------
// 方案 §6-7b 补充：不可解析的 device: 值整键跳过、不推进 pmeta。
// ---------------------------------------------------------------------------

#[test]
fn pdsync_device_unparseable_remote_does_not_advance_pmeta_or_overwrite_local() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = peer_root(6);
    let peer_id = "peer-unparseable";
    let device_key = format!("device:{peer_id}");

    // 本地 upsert_self + mark_revoked，建立基线记录与其 pmeta。
    DeviceService::upsert_self(&mut s, peer_id, 100, NODE, "1.0.0", None).unwrap();
    let uid = DeviceService::get(&s, peer_id).unwrap().unwrap().device_uid;
    DeviceService::mark_revoked(&mut s, uid.as_deref().unwrap(), 200, 200, NODE).unwrap();

    // 记录基线 pmeta。
    let baseline = spark_core::sync::personal::get_personal_meta(&s, &device_key)
        .unwrap()
        .expect("本地 device 记录应有 pmeta");

    // 远端推来一个不可解析的 device: 值（ts 更新、vv (NODE,9)）。
    let meta = DocMeta {
        vv: [(NODE.to_string(), 9)].into_iter().collect(),
        ts: 900,
        node_id: Some(NODE.to_string()),
        tombstone: None,
    };
    let record = PdsyncRecord {
        key: device_key.clone(),
        value: json!("not-a-device-record"),
        meta,
        dseq: None,
    };
    let body = build_data_batch("device", &[record], 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        &my_root,
        &my_root,
        NOW,
        body,
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-self-c", &HashSet::new(), NOW, NODE, None)
        .expect("pdsync-data 应 Ok");
    assert_eq!(result.response, json!({ "ok": true }));

    // pmeta 未推进，仍为基线版本。
    let after = spark_core::sync::personal::get_personal_meta(&s, &device_key)
        .unwrap()
        .expect("pmeta 应保持存在");
    assert_eq!(
        after.vv.get(NODE),
        baseline.vv.get(NODE),
        "不可解析 device: 值不得推进 pmeta"
    );

    // 本地记录未被覆盖：revokedAt 必须保留。
    let local = DeviceService::get(&s, peer_id).unwrap().unwrap();
    assert_eq!(local.revoked_at, Some(200), "本地 revokedAt 不得被覆盖");
}
