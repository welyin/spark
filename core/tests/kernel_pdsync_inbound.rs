//! pdsync-data 入站编排集成测试（直调 `handle_inbound_dm`，手工签名自设备信封）：
//! - `msg:conv` 合并：convId 含冒号（`dm:{rootId}`）时正确提取；远端胜出后
//!   本地消息驱动字段（unread_count/updated_at）保留，同步字段（置顶/免打扰/
//!   草稿）合入；全新会话清零未读（§3.2 未读为设备本地语义）；
//! - key 白名单（§8.4 红线）：非注册 category 的键（`p2p:*`/`pmeta:*`）与
//!   声明 category 不一致的记录整批拒收；
//! - 合并事件：联系人 → ContactsSynced、设备 → DeviceUpdated、会话 →
//!   ConversationsSynced、消息 → ChatReceived（前端据此刷新）。

mod common;

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use spark_core::contact::{ContactService, FriendRecord, PeerRef};
use spark_core::epoch::{put_effective, put_local_key};
use spark_core::kernel::{direct_conversation_id, dm_envelope, handle_inbound_dm};
use spark_core::message::{
    AppMessageRecord, ConversationKind, ConversationRecord, MessageRecord, MessageService,
    MessageType, app_conversation_id, app_message_key, message_id_index_key, message_key,
};
use spark_core::p2p::P2pEvent;
use spark_core::pw::{self, build_ack, build_value, derive_kverify};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::pdsync::{PdsyncRecord, build_data_batch, build_hello, self_friend_key};
use spark_core::sync::{get_personal_meta, is_tombstone};

const PERSONAL: &str = "personal";
const NOW: i64 = 1_720_000_000_000;
/// pdsync 版本向量节点 id：对齐 src 内联测试惯例（无 p2p 节点时用 local-node）。
const NODE: &str = "local-node";

/// 自设备身份：rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致）。
fn self_identity(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

fn conv_record(id: &str, peer_root_id: &str) -> ConversationRecord {
    ConversationRecord {
        id: id.to_string(),
        kind: ConversationKind::Direct,
        title: "对方".to_string(),
        peer_root_id: peer_root_id.to_string(),
        peer: None,
        unread_count: 0,
        pinned_at: 0,
        muted: false,
        draft: String::new(),
        updated_at: 0,
        meta_updated_at: 0,
    }
}

fn remote_meta(node: &str, counter: i64, ts: i64) -> DocMeta {
    DocMeta {
        vv: [(node.to_string(), counter)].into_iter().collect(),
        ts,
        node_id: Some(node.to_string()),
        tombstone: None,
    }
}

/// 构造并投递一批 pdsync-data（from==to==自己，验签通过）。
fn deliver_pdsync_data(
    storage: &mut MemoryStorage,
    key: &SigningKey,
    my_root: &str,
    category: &str,
    records: &[PdsyncRecord],
) -> spark_core::kernel::InboundDmResult {
    deliver_pdsync_data_kv(storage, key, my_root, category, records, None)
}

/// 带解锁会话 kverify 注入的投递：M3 ack 批尾钩子（§13.4 钩子①）要求
/// `ctx.kverify` 在场才逐条 `verify_and_anchor_ack`（锁定态 kverify=None）。
fn deliver_pdsync_data_kv(
    storage: &mut MemoryStorage,
    key: &SigningKey,
    my_root: &str,
    category: &str,
    records: &[PdsyncRecord],
    kverify: Option<&[u8; 32]>,
) -> spark_core::kernel::InboundDmResult {
    let body = build_data_batch(category, records, 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        my_root,
        my_root,
        NOW,
        body,
        key,
    );
    handle_inbound_dm(
        storage,
        my_root,
        "我",
        envelope,
        "peer-self-b",
        &HashSet::new(),
        NOW,
        NODE,
        kverify,
    )
    .unwrap()
}

#[test]
fn pdsync_conv_merge_preserves_local_unread() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    // direct 会话 id 含冒号（`dm:{rootId}`）—— convId 提取必须取完整余段
    let conv_id = direct_conversation_id(&peer_root);
    assert!(conv_id.contains(':'));

    // 本地已有会话：未读 5、updated_at=NOW（消息驱动字段）
    let mut local = conv_record(&conv_id, &peer_root);
    local.unread_count = 5;
    local.updated_at = NOW;
    MessageService::upsert_conversation(&mut s, PERSONAL, &local).unwrap();

    // 远端元数据快照：同步字段不同（置顶/免打扰/草稿/标题），消息驱动字段
    // 携带对端值（不应覆盖本地）
    let mut remote = conv_record(&conv_id, &peer_root);
    remote.title = "远端标题".to_string();
    remote.pinned_at = 123;
    remote.muted = true;
    remote.draft = "草稿".to_string();
    remote.meta_updated_at = 999;
    remote.unread_count = 0;
    remote.updated_at = 1;
    let record = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: serde_json::to_value(&remote).unwrap(),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };

    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:conv", &[record]);
    assert_eq!(result.response, json!({ "ok": true }));

    let stored = MessageService::get_conversation(&s, PERSONAL, &conv_id)
        .unwrap()
        .expect("会话存在");
    // 本地消息驱动字段保留（merge_conv_meta 语义）
    assert_eq!(stored.unread_count, 5, "本地未读必须保留（每设备独立）");
    assert_eq!(stored.updated_at, NOW, "本地 updated_at 必须保留");
    // 远端同步字段合入
    assert_eq!(stored.pinned_at, 123);
    assert!(stored.muted);
    assert_eq!(stored.draft, "草稿");
    assert_eq!(stored.meta_updated_at, 999);
    assert_eq!(stored.title, "远端标题");
    // 会话合并通知前端刷新列表
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, P2pEvent::ConversationsSynced(_))),
        "应发出 ConversationsSynced 事件"
    );
}

#[test]
fn pdsync_new_conv_zeroes_message_driven_fields() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);

    // 本地无该会话：远端快照带来对端的未读/updated_at，首次落库须清零
    let mut remote = conv_record(&conv_id, &peer_root);
    remote.unread_count = 7;
    remote.updated_at = NOW;
    remote.pinned_at = 42;
    let record = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: serde_json::to_value(&remote).unwrap(),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };

    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:conv", &[record]);
    assert_eq!(result.response, json!({ "ok": true }));

    let stored = MessageService::get_conversation(&s, PERSONAL, &conv_id)
        .unwrap()
        .expect("新会话已落库");
    assert_eq!(stored.unread_count, 0, "新会话不继承对端未读");
    assert_eq!(stored.updated_at, 0, "新会话不继承对端 updated_at");
    assert_eq!(stored.pinned_at, 42, "同步字段正常落地");
}

#[test]
fn pdsync_data_rejects_keys_outside_category_registry() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);

    // §8.4 红线：`p2p:*` 记录（含节点私钥）不得经 pdsync 覆写
    let evil = PdsyncRecord {
        key: "p2p:identity:privateKey".to_string(),
        value: json!("forged"),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "p2p", &[evil]);
    assert_eq!(result.response["ok"], false, "非注册 category 的键整批拒收");
    assert!(s.get("p2p:identity:privateKey").unwrap().is_none());

    // `pmeta:*`（同步元数据）同样不在注册表内
    let evil = PdsyncRecord {
        key: "pmeta:ct:friend:a".to_string(),
        value: json!({}),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[evil]);
    assert_eq!(result.response["ok"], false);
    assert!(s.get("pmeta:ct:friend:a").unwrap().is_none());

    // 声明 category 与记录键不一致：整批拒收，合法键也不落库
    let mixed = PdsyncRecord {
        key: "ct:friend:a".to_string(),
        value: json!({"rootId": "a"}),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "device", &[mixed]);
    assert_eq!(result.response["ok"], false, "category 不一致拒收");
    assert!(s.get("ct:friend:a").unwrap().is_none());
}

#[test]
fn pdsync_data_emits_events_for_contacts_devices_messages() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);
    MessageService::upsert_conversation(&mut s, PERSONAL, &conv_record(&conv_id, &peer_root))
        .unwrap();

    // 联系人记录 → ContactsSynced
    let contact = PdsyncRecord {
        key: format!("ct:friend:{peer_root}"),
        value: json!({"rootId": peer_root, "nickname": "朋友"}),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[contact]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, P2pEvent::ContactsSynced(_))),
        "联系人合并应发 ContactsSynced"
    );

    // 设备记录 → DeviceUpdated（data 即 DeviceRecord JSON；须为合法形状）。
    let device = PdsyncRecord {
        key: "device:peer-x".to_string(),
        value: json!({
            "peerId": "peer-x",
            "deviceName": "另一台设备",
            "os": "Android",
            "arch": "aarch64",
            "macs": [],
            "updatedAt": NOW,
            "lastSeenAt": NOW
        }),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "device", &[device]);
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, P2pEvent::DeviceUpdated(_))),
        "设备合并应发 DeviceUpdated"
    );

    // 消息（窗口同步）→ 落库 + byid 索引 + 该会话一条 ChatReceived
    let msg = MessageRecord {
        id: "m1".to_string(),
        sender_id: my_root.clone(),
        sender_name: "我".to_string(),
        msg_type: MessageType::Text,
        content: "自设备同步来的消息".to_string(),
        created_at: NOW,
        ..Default::default()
    };
    let msg_key = format!("msg:item:personal:{conv_id}:{NOW:013}:m1");
    let message = PdsyncRecord {
        key: msg_key.clone(),
        value: serde_json::to_value(&msg).unwrap(),
        meta: DocMeta::default(),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:item", &[message]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(s.get(&msg_key).unwrap().is_some(), true, "消息本体落库");
    let chat_events: Vec<_> = result
        .events
        .iter()
        .filter(|e| matches!(e, P2pEvent::ChatReceived(_)))
        .collect();
    assert_eq!(chat_events.len(), 1, "该会话应发一条 ChatReceived");
    let P2pEvent::ChatReceived(data) = chat_events[0] else {
        unreachable!()
    };
    assert_eq!(data["conversation"]["id"], json!(conv_id));
    assert_eq!(data["message"]["id"], "m1");
    // 窗口合入不动未读：事件会话快照未读保持本地值（0）
    assert_eq!(data["conversation"]["unreadCount"], 0);
}

// ── 第三轮修复的链路级回归 ─────────────────────────────────────────

/// 真实当前时间（ms）：消息窗口采集内部按系统时钟过滤窗口下界，测试消息
/// 必须落在窗口内（不能用固定的历史时间戳）。
fn real_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// N1 回归：hello → 窗口采集 → data 分批 → 白名单 全链路。
/// 发送方账号同时有普通会话（msg:item）与应用会话（msg:app）消息：窗口记录
/// 必须按 key 前缀分流打批（msg:app 批声明 "msg:app"），否则接收侧白名单
/// category-mismatch 整批拒收、同批合法 msg:item 连坐。
#[test]
fn pdsync_hello_window_exchange_delivers_item_and_app() {
    let now = real_now_ms();
    let mut sender = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);

    // 普通会话 + 消息
    MessageService::upsert_conversation(&mut sender, PERSONAL, &conv_record(&conv_id, &peer_root))
        .unwrap();
    let item_msg = MessageRecord {
        id: "m1".to_string(),
        sender_id: my_root.clone(),
        sender_name: "我".to_string(),
        msg_type: MessageType::Text,
        content: "窗口里的普通消息".to_string(),
        created_at: now,
        ..Default::default()
    };
    let item_key = message_key(PERSONAL, &conv_id, now, "m1");
    sender
        .put(&item_key, &serde_json::to_string(&item_msg).unwrap())
        .unwrap();

    // 应用会话 + 应用消息
    let app_conv_id = app_conversation_id("notes");
    MessageService::upsert_conversation(
        &mut sender,
        PERSONAL,
        &conv_record(&app_conv_id, &peer_root),
    )
    .unwrap();
    let app_msg = AppMessageRecord {
        id: "a1".to_string(),
        plugin_id: "notes".to_string(),
        summary: "应用消息摘要".to_string(),
        payload: json!({ "summary": "应用消息摘要" }),
        card: None,
        created_at: now,
        status: "local".to_string(),
        read: false,
    };
    let app_key = app_message_key(PERSONAL, "notes", now, "a1");
    sender
        .put(&app_key, &serde_json::to_string(&app_msg).unwrap())
        .unwrap();

    // 对端 hello 到达发送方 → 触发窗口推送（category 折叠均为空，无 diff 输出，
    // pdsync_out 只含消息窗口批次）
    let hello_body = json!({
        "categories": {},
        "msgWindow": { "maxAgeMs": 86_400_000i64, "maxPerConv": 500 },
        "attachmentPolicy": "eager",
    });
    let hello_env = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_HELLO,
        &my_root,
        &my_root,
        now,
        hello_body,
        &key,
    );
    let hello_result = handle_inbound_dm(
        &mut sender,
        &my_root,
        "我",
        hello_env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();
    assert!(
        !hello_result.pdsync_out.is_empty(),
        "hello 应触发消息窗口 data 推送"
    );

    // 发送方回投的 data 逐个送达接收方（同账号另一台设备）
    let mut receiver = MemoryStorage::new();
    let mut saw_item_batch = false;
    let mut saw_app_batch = false;
    for out in &hello_result.pdsync_out {
        let body = out.body().clone();
        match body.get("category").and_then(serde_json::Value::as_str) {
            Some("msg:item") => saw_item_batch = true,
            Some("msg:app") => saw_app_batch = true,
            other => panic!("窗口批次 category 异常: {other:?}"),
        }
        let env = dm_envelope::build_envelope(
            dm_envelope::KIND_PDSYNC_DATA,
            &my_root,
            &my_root,
            now,
            body,
            &key,
        );
        let r = handle_inbound_dm(
            &mut receiver,
            &my_root,
            "我",
            env,
            "peer-self-b",
            &HashSet::new(),
            now,
            NODE,
            None,
        )
        .unwrap();
        assert_eq!(
            r.response,
            json!({ "ok": true }),
            "窗口批次不应被白名单整批拒收"
        );
    }
    assert!(
        saw_item_batch && saw_app_batch,
        "msg:item 与 msg:app 应分流为各自 category 的批次"
    );
    assert!(
        receiver.get(&item_key).unwrap().is_some(),
        "msg:item 消息落库"
    );
    assert!(
        receiver.get(&app_key).unwrap().is_some(),
        "msg:app 消息落库"
    );
    // msg:item 的 byid 索引随落盘重建；msg:app 无 byid（与本地 append 口径一致）
    let idx = message_id_index_key(PERSONAL, &conv_id, "m1");
    assert_eq!(
        receiver.get(&idx).unwrap().as_deref(),
        Some(item_key.as_str())
    );
}

/// M-inbound 回归：入站 chat 落库走 append_message_pdsync——「只收不发」的
/// 会话壳也 bump pmeta，对自设备 pdsync 的折叠/增量采集可见。
#[test]
fn pdsync_inbound_chat_bumps_conv_pmeta() {
    let now = real_now_ms();
    let mut s = MemoryStorage::new();
    let (_my_key, my_root) = self_identity(1);
    let (peer_key, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);

    let msg = MessageRecord {
        id: "c1".to_string(),
        sender_id: peer_root.clone(),
        sender_name: "朋友".to_string(),
        msg_type: MessageType::Text,
        content: "你好".to_string(),
        created_at: now,
        ..Default::default()
    };
    let body = json!({ "spaceKey": PERSONAL, "message": serde_json::to_value(&msg).unwrap() });
    let env = dm_envelope::build_envelope(
        dm_envelope::KIND_CHAT,
        &peer_root,
        &my_root,
        now,
        body,
        &peer_key,
    );
    let r = handle_inbound_dm(
        &mut s,
        &my_root,
        "我",
        env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();
    assert_eq!(r.response, json!({ "ok": true }));

    let conv_key = format!("msg:conv:personal:{conv_id}");
    let pmeta = get_personal_meta(&s, &conv_key)
        .unwrap()
        .expect("入站消息应 bump 会话壳 pmeta");
    // per-node 序号：入站 chat 的 msg:item 落库本身走受管路径（耗序号 1），
    // 会话壳 pmeta bump 是本节点第 2 次受管写 → vv={node:2}
    assert_eq!(pmeta.vv.get(NODE), Some(&2), "pmeta 计数器来自本机 node");
}

/// N3 回归：conv tombstone 经 pdsync-data 传播——单 batch 删本体 + 落墓碑
/// pmeta，并发 ConversationsSynced 事件。
#[test]
fn pdsync_conv_tombstone_deletes_record() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);
    MessageService::upsert_conversation(&mut s, PERSONAL, &conv_record(&conv_id, &peer_root))
        .unwrap();

    let tomb = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: json!(null),
        meta: DocMeta {
            vv: [("peer-node-b".to_string(), 2)].into_iter().collect(),
            ts: NOW,
            node_id: Some("peer-node-b".to_string()),
            tombstone: Some(true),
        },
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:conv", &[tomb]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert!(
        MessageService::get_conversation(&s, PERSONAL, &conv_id)
            .unwrap()
            .is_none(),
        "会话本体应被墓碑删除"
    );
    let pmeta = get_personal_meta(&s, &format!("msg:conv:personal:{conv_id}"))
        .unwrap()
        .expect("墓碑 pmeta 已落");
    assert!(is_tombstone(&pmeta), "pmeta 应为墓碑");
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, P2pEvent::ConversationsSynced(_))),
        "会话删除应发 ConversationsSynced"
    );
}

// ---------------------------------------------------------------------------
// M3 发送侧加密（team-lead 统一回归 c）：hello P4 分支推送消息窗口
// msg:item/msg:app 记录时，若对端 hello 宣告 epoch>0 且本机有效，value 必须为
// ikey 密文（`$enc:"ikey"`），不得明文外泄消息内容。
// ---------------------------------------------------------------------------

#[test]
fn pdsync_hello_window_push_encrypts_msg_records_when_remote_epoch_announced() {
    let now = real_now_ms();
    let mut sender = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);

    // 普通消息 + 应用消息（同基线用例）。
    MessageService::upsert_conversation(&mut sender, PERSONAL, &conv_record(&conv_id, &peer_root))
        .unwrap();
    let item_msg = MessageRecord {
        id: "enc1".to_string(),
        sender_id: my_root.clone(),
        sender_name: "我".to_string(),
        msg_type: MessageType::Text,
        content: "必须加密的窗口消息".to_string(),
        created_at: now,
        ..Default::default()
    };
    let item_key = message_key(PERSONAL, &conv_id, now, "enc1");
    sender
        .put(&item_key, &serde_json::to_string(&item_msg).unwrap())
        .unwrap();

    let app_conv_id = app_conversation_id("notes");
    MessageService::upsert_conversation(
        &mut sender,
        PERSONAL,
        &conv_record(&app_conv_id, &peer_root),
    )
    .unwrap();
    let app_msg = AppMessageRecord {
        id: "enca1".to_string(),
        plugin_id: "notes".to_string(),
        summary: "应用消息摘要须加密".to_string(),
        payload: json!({ "summary": "应用消息摘要须加密" }),
        card: None,
        created_at: now,
        status: "local".to_string(),
        read: false,
    };
    let app_key = app_message_key(PERSONAL, "notes", now, "enca1");
    sender
        .put(&app_key, &serde_json::to_string(&app_msg).unwrap())
        .unwrap();

    // 发送侧 epoch1 生效：本机密钥表 + effective 键。
    let epoch1_key: [u8; 32] = [0x47; 32];
    put_local_key(&mut sender, 1, &epoch1_key).unwrap();
    put_effective(&mut sender, 1).unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&sender).unwrap(),
        1,
        "前置 effective=1"
    );

    // 对端 hello 宣告 epoch=1（其已生效 epoch1）→ 发送侧按 min(1,1)=1 加密窗口记录。
    let hello_body = json!({
        "categories": {},
        "msgWindow": { "maxAgeMs": 86_400_000i64, "maxPerConv": 500 },
        "attachmentPolicy": "eager",
        "epoch": 1,
    });
    let hello_env = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_HELLO,
        &my_root,
        &my_root,
        now,
        hello_body,
        &key,
    );
    let hello_result = handle_inbound_dm(
        &mut sender,
        &my_root,
        "我",
        hello_env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();
    assert!(
        !hello_result.pdsync_out.is_empty(),
        "hello 应触发消息窗口 data 推送"
    );

    // 逐个 data 批次检查：msg:item / msg:app 记录 value 为 ikey 密文，且不含明文内容。
    let mut saw_item_batch = false;
    let mut saw_app_batch = false;
    for out in &hello_result.pdsync_out {
        let body = out.body().clone();
        let category = match body.get("category").and_then(serde_json::Value::as_str) {
            Some(c) => c,
            None => continue, // 非 data 输出（如 need）跳过
        };
        if category != "msg:item" && category != "msg:app" {
            continue;
        }
        let records = body
            .get("records")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(!records.is_empty(), "批次 {category} 应有记录");
        for rec in &records {
            let value = rec.get("value").unwrap_or(&json!(null));
            assert!(
                spark_core::epoch::is_ikey_ciphertext(value),
                "{category} 记录 value 应为 ikey 密文，实为 {value}"
            );
            assert_eq!(
                value.get("epoch").and_then(serde_json::Value::as_u64),
                Some(1),
                "{category} 密文 epoch 应为 1"
            );
            // 明文内容不得泄露进密文信封外层。
            let outer = serde_json::to_string(value).unwrap();
            assert!(
                !outer.contains("必须加密") && !outer.contains("应用消息摘要"),
                "{category} 密文外层不得含明文内容"
            );
        }
        if category == "msg:item" {
            saw_item_batch = true;
        }
        if category == "msg:app" {
            saw_app_batch = true;
        }
    }
    assert!(
        saw_item_batch && saw_app_batch,
        "msg:item 与 msg:app 均应以密文推送"
    );
}

// ── 自 FriendRecord 排除（`ct:friend:{rootId}`，设备相对 peer 不可互灌）──

/// 带 peer 寻址的朋友记录（自记录：rootId == 本机、peer 指向对端设备）。
fn friend_record_with_peer(root_id: &str, peer_id: &str) -> FriendRecord {
    FriendRecord {
        root_id: root_id.to_string(),
        nickname: "我".to_string(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: NOW,
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
        updated_at: NOW,
    }
}

/// 构造并投递一个 pdsync 信封（from==to==自己，验签通过），返回入站结果。
fn deliver_pdsync(
    storage: &mut MemoryStorage,
    key: &SigningKey,
    my_root: &str,
    kind: &str,
    body: serde_json::Value,
    node: &str,
) -> spark_core::kernel::InboundDmResult {
    let envelope = dm_envelope::build_envelope(kind, my_root, my_root, NOW, body, key);
    handle_inbound_dm(
        storage,
        my_root,
        "我",
        envelope,
        "peer-self-other",
        &HashSet::new(),
        NOW,
        node,
        None,
    )
    .unwrap()
}

/// 把一侧处理 hello 产生的 pdsync_out 逐条过链路：Data 由对侧落库；Need
/// 由对侧应答、应答 Data 回本侧落库（模拟双向一跳，与真实 dm 链路一致）。
fn route_pdsync_out(
    bodies: Vec<serde_json::Value>,
    origin: &mut MemoryStorage,
    origin_node: &str,
    peer: &mut MemoryStorage,
    peer_node: &str,
    key: &SigningKey,
    my_root: &str,
) {
    for body in bodies {
        let is_need = body.get("knownVv").is_some();
        let kind = if is_need {
            dm_envelope::KIND_PDSYNC_NEED
        } else {
            dm_envelope::KIND_PDSYNC_DATA
        };
        let r = deliver_pdsync(peer, key, my_root, kind, body, peer_node);
        assert_eq!(r.response, json!({ "ok": true }));
        if is_need {
            let answers: Vec<_> = r.pdsync_out.iter().map(|o| o.body().clone()).collect();
            for answer in answers {
                let r2 = deliver_pdsync(
                    origin,
                    key,
                    my_root,
                    dm_envelope::KIND_PDSYNC_DATA,
                    answer,
                    origin_node,
                );
                assert_eq!(r2.response, json!({ "ok": true }));
            }
        }
    }
}

/// 双设备端到端（回归：pdsync 互灌自 FriendRecord → 一端 peer 指向本机 →
/// 自聊投递拨自己 DialError::LocalPeerId）：A/B 同账号，各自自记录 peer
/// 指向对方设备。hello→need/data 双向交换后两端自记录保持各自原值，普通
/// 朋友记录照常同步，收敛后 ct:friend 无伪 diff。
#[test]
fn pdsync_self_friend_record_not_cross_fed() {
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    // 同账号两台设备共享同一身份（root 私钥随配对分发到各自设备）
    let (key, my_root) = self_identity(1);
    let (_, peer_root) = self_identity(7);
    let self_key = self_friend_key(&my_root);

    // A：自记录 peer→"peer-device-b"（B 的设备）+ 一条普通朋友记录
    // （put_personal 显式记账：本测试用裸存储模拟两端库，生产等价物是
    // 版本化中间件的自动记账）
    spark_core::sync::put_personal(
        &mut a,
        "node-a",
        &self_friend_key(&my_root),
        &serde_json::to_string(&friend_record_with_peer(&my_root, "peer-device-b")).unwrap(),
        NOW,
    )
    .unwrap();
    spark_core::sync::put_personal(
        &mut a,
        "node-a",
        &format!("ct:friend:{peer_root}"),
        &serde_json::to_string(&friend_record_with_peer(&peer_root, "peer-x")).unwrap(),
        NOW,
    )
    .unwrap();
    // B：自记录 peer→"peer-device-a"（A 的设备；ts 更晚——无排除时并发
    // LWW 必然毒化 A 的自记录）
    spark_core::sync::put_personal(
        &mut b,
        "node-b",
        &self_friend_key(&my_root),
        &serde_json::to_string(&friend_record_with_peer(&my_root, "peer-device-a")).unwrap(),
        NOW + 1000,
    )
    .unwrap();

    // A → B：hello（排除自记录键）→ B 的 need/data 过链路
    let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
    let r = deliver_pdsync(
        &mut b,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_HELLO,
        hello_a,
        "node-b",
    );
    let bodies: Vec<_> = r.pdsync_out.iter().map(|o| o.body().clone()).collect();
    route_pdsync_out(bodies, &mut b, "node-b", &mut a, "node-a", &key, &my_root);
    // B → A：反向
    let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
    let r = deliver_pdsync(
        &mut a,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_HELLO,
        hello_b,
        "node-a",
    );
    let bodies: Vec<_> = r.pdsync_out.iter().map(|o| o.body().clone()).collect();
    route_pdsync_out(bodies, &mut a, "node-a", &mut b, "node-b", &key, &my_root);

    // 两端自记录保持各自原值（peer 各指向对方设备，未被互灌）
    let fa = ContactService::get_friend(&a, &my_root)
        .unwrap()
        .expect("A 自记录仍在");
    assert_eq!(
        fa.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-device-b"),
        "A 自记录 peer 不得被 B 的版本覆盖"
    );
    let fb = ContactService::get_friend(&b, &my_root)
        .unwrap()
        .expect("B 自记录仍在");
    assert_eq!(
        fb.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-device-a"),
        "B 自记录 peer 不得被 A 的版本覆盖"
    );
    // 普通朋友记录照常同步到 B（排除仅针对自记录键，不误伤同 category）
    let fp = ContactService::get_friend(&b, &peer_root)
        .unwrap()
        .expect("普通朋友记录应同步到 B");
    assert_eq!(fp.peers.first().map(|p| p.peer_id.as_str()), Some("peer-x"));

    // 收敛：再互发 hello → ct:friend 无 need/data（folded vv 无伪 diff）
    let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
    let r = deliver_pdsync(
        &mut b,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_HELLO,
        hello_a2,
        "node-b",
    );
    assert!(
        r.pdsync_out.iter().all(
            |o| o.body().get("category").and_then(serde_json::Value::as_str) != Some("ct:friend")
        ),
        "收敛后 ct:friend 不应再有 diff 输出"
    );
    let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key), None).unwrap();
    let r = deliver_pdsync(
        &mut a,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_HELLO,
        hello_b2,
        "node-a",
    );
    assert!(
        r.pdsync_out.iter().all(
            |o| o.body().get("category").and_then(serde_json::Value::as_str) != Some("ct:friend")
        ),
        "收敛后 ct:friend 不应再有 diff 输出"
    );
}

/// apply 侧排除：旧版本对端仍可能推自记录/自记录墓碑——接收侧直接丢弃
/// （幂等 ok，非整批拒收），不覆盖本机自记录、不删本机本体、pmeta 不推进。
#[test]
fn pdsync_data_drops_self_friend_record_and_tombstone() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    // put_personal 显式记账（裸存储上的生产等价物 = 中间件自动记账）
    spark_core::sync::put_personal(
        &mut s,
        NODE,
        &self_friend_key(&my_root),
        &serde_json::to_string(&friend_record_with_peer(&my_root, "peer-device-b")).unwrap(),
        NOW,
    )
    .unwrap();

    // 对端视角的自记录：peer 指向「本机设备」（对它而言的对端），vv/ts 均
    // 更新——无排除时必然覆盖本机
    let poisoned = PdsyncRecord {
        key: self_friend_key(&my_root),
        value: serde_json::to_value(friend_record_with_peer(&my_root, "peer-device-a")).unwrap(),
        meta: remote_meta("peer-node-b", 9, NOW + 60_000),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[poisoned]);
    assert_eq!(result.response, json!({ "ok": true }), "排除是丢弃而非拒批");
    let f = ContactService::get_friend(&s, &my_root).unwrap().unwrap();
    assert_eq!(
        f.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-device-b"),
        "本机自记录不被对端版本覆盖"
    );
    let meta = get_personal_meta(&s, &self_friend_key(&my_root))
        .unwrap()
        .expect("pmeta 仍在");
    assert_eq!(meta.vv.get(NODE), Some(&1), "pmeta 不推进（保持本机版本）");

    // 对端删了它的自记录（墓碑）——不得删掉本机的自记录本体
    let tomb = PdsyncRecord {
        key: self_friend_key(&my_root),
        value: json!(null),
        meta: DocMeta {
            vv: [("peer-node-b".to_string(), 10)].into_iter().collect(),
            ts: NOW + 120_000,
            node_id: Some("peer-node-b".to_string()),
            tombstone: Some(true),
        },
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[tomb]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_some(),
        "自记录墓碑不得删本机本体"
    );
}

/// 方案 A 回归锚点：旧版本对端强制推送自 FriendRecord 键 `ct:friend:{rootId}`
/// （含普通记录 + 墓碑两种）→ 入站闸门逐条丢弃，本机自记录保持原值、pmeta
/// 不推进、墓碑不删本体。对端视角的自记录 peer 指向「本机设备」——恰是
/// pdsync 互灌污染的自指形态（peer == 本机节点 id），本机不得落库。
#[test]
fn pdsync_inbound_gate_drops_self_friend_record_and_tombstone() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    // put_personal 显式记账（裸存储上的生产等价物 = 中间件自动记账）
    spark_core::sync::put_personal(
        &mut s,
        NODE,
        &self_friend_key(&my_root),
        &serde_json::to_string(&friend_record_with_peer(&my_root, "peer-device-b")).unwrap(),
        NOW,
    )
    .unwrap();

    // 对端视角的自记录：peer 指向「本机设备」（peer == NODE，自指污染形态），
    // vv/ts 均更新——无闸门时必然覆盖本机、本机自记录自指
    let poisoned = PdsyncRecord {
        key: self_friend_key(&my_root),
        value: serde_json::to_value(friend_record_with_peer(&my_root, NODE)).unwrap(),
        meta: remote_meta("peer-node-b", 9, NOW + 60_000),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[poisoned]);
    assert_eq!(result.response, json!({ "ok": true }), "排除是丢弃而非拒批");
    let f = ContactService::get_friend(&s, &my_root).unwrap().unwrap();
    assert_eq!(
        f.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-device-b"),
        "本机自记录 peer 不得被自指污染值覆盖（保持原值）"
    );
    let meta = get_personal_meta(&s, &self_friend_key(&my_root))
        .unwrap()
        .expect("pmeta 仍在");
    assert_eq!(meta.vv.get(NODE), Some(&1), "pmeta 不推进（保持本机版本）");

    // 对端删了它的自记录（墓碑）——不得删掉本机的自记录本体
    let tomb = PdsyncRecord {
        key: self_friend_key(&my_root),
        value: json!(null),
        meta: DocMeta {
            vv: [("peer-node-b".to_string(), 10)].into_iter().collect(),
            ts: NOW + 120_000,
            node_id: Some("peer-node-b".to_string()),
            tombstone: Some(true),
        },
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[tomb]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_some(),
        "自记录墓碑不得删本机本体"
    );
}

/// 方案 C 集成回归：自设备 friend-request 携带自指 peer（peer == 本机 node）
/// 触发的写自 FriendRecord 防污染——记录 peer 不被污染（保持原值）、priority
/// 集合不含本机 peerId、auto_accept 目标非本机（自指→None→不回发）。与
/// `reject_self_pointing_peer` 单测（inbound_dm.rs）一道钉住写侧防护。
#[test]
fn self_friend_request_with_self_pointing_peer_is_rejected() {
    use spark_core::p2p::priority_peers::PriorityPeerStore;

    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    // 预置自记录：peer 指向对端设备（peer-device-b）
    ContactService::upsert_friend_pdsync(
        &mut s,
        &friend_record_with_peer(&my_root, "peer-device-b"),
        NOW,
        NODE,
    )
    .unwrap();

    // 自设备（from==我）的 friend-request：nodeInfo.peerId == 本机 node（NODE）
    // ——自指污染形态，防护必须拒绝写入且不回发自指目标
    let body = json!({
        "requestId": "req-self",
        "nickname": "我",
        "nodeInfo": { "peerId": NODE, "addresses": [] },
    });
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_FRIEND_REQUEST,
        &my_root,
        &my_root,
        NOW,
        body,
        &key,
    );
    let result = handle_inbound_dm(
        &mut s,
        &my_root,
        "我",
        envelope,
        "peer-self-other",
        &HashSet::new(),
        NOW,
        NODE,
        None,
    )
    .unwrap();
    assert_eq!(result.response["ok"], true, "自指请求仍正常应答 ok");

    // 1) 自记录 peer 不被污染（保持原值，而非 NODE）
    let f = ContactService::get_friend(&s, &my_root).unwrap().unwrap();
    assert_eq!(
        f.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-device-b"),
        "自指 peer 不得覆盖自记录（保持原值）"
    );

    // 2) priority 集合不含本机 peerId（否则 redial 永久对本机空转且无清理）
    let mut priority = PriorityPeerStore::new(&mut s);
    let list = priority.list().unwrap();
    assert!(
        !list.iter().any(|id| id == NODE),
        "priority 集合不得含本机 peerId，实际={list:?}"
    );

    // 3) auto_accept 目标非本机：自指→None→不回发
    assert!(
        result.auto_accept.is_none(),
        "自指 peer 不得触发 auto_accept 回发（避免对自身发起配对）"
    );
}

/// 自聊会话（peer_root == 本机 rootId）conv 合入：peer 是设备相对寻址
/// （各指向对方设备），远端胜出合并后保留本地 peer，其余同步字段照常合入。
#[test]
fn pdsync_self_conv_merge_preserves_local_peer() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    let conv_id = direct_conversation_id(&my_root); // dm:{my_root} 自聊会话

    let mut local = conv_record(&conv_id, &my_root);
    local.peer = Some(PeerRef {
        peer_id: "peer-device-b".to_string(),
        addresses: Vec::new(),
        ..Default::default()
    });
    MessageService::upsert_conversation(&mut s, PERSONAL, &local).unwrap();

    // 对端设备快照：同一会话、peer 指向「本机设备」（对它而言的对端）
    let mut remote = conv_record(&conv_id, &my_root);
    remote.peer = Some(PeerRef {
        peer_id: "peer-device-a".to_string(),
        addresses: Vec::new(),
        ..Default::default()
    });
    remote.pinned_at = 123;
    let record = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: serde_json::to_value(&remote).unwrap(),
        meta: remote_meta("peer-node-b", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:conv", &[record]);
    assert_eq!(result.response, json!({ "ok": true }));

    let stored = MessageService::get_conversation(&s, PERSONAL, &conv_id)
        .unwrap()
        .expect("会话存在");
    assert_eq!(
        stored.peer.map(|p| p.peer_id).as_deref(),
        Some("peer-device-b"),
        "自聊会话 peer 保留本地值（设备相对寻址不可互灌）"
    );
    assert_eq!(stored.pinned_at, 123, "其余同步字段正常合入");
}

// ---------------------------------------------------------------------------
// R1 need 路径端到端加密（team-lead 统一回归 a）。
//   Part 1：双设备 A/B（同 root，均 epoch1 持密钥）→ A 改数据 → B 发 need →
//           A need 响应 value 为 ikey 密文（非明文）→ B 用本机 epoch1 密钥解开合入。
//   Part 2：被撤销设备 C（无 epoch2 密钥）向不知情设备 D 发 need → D 响应值用
//           epoch2 加密 → C 解不开 → 记录不落库（R1 攻击路径锁死）。
// ---------------------------------------------------------------------------

#[test]
fn need_path_encryption_end_to_end_and_revoked_cannot_decrypt() {
    let (key, my_root) = self_identity(1);

    // ── Part 1：need 响应加密 + 接收方解密合入 ──────────────────────────
    let mut a = MemoryStorage::new(); // A：有数据
    let mut b = MemoryStorage::new(); // B：发起 need

    // A/B 均 epoch1 生效 + 本机 epoch1 密钥。
    let epoch1_key: [u8; 32] = [0x61; 32];
    put_effective(&mut a, 1).unwrap();
    put_local_key(&mut a, 1, &epoch1_key).unwrap();
    put_effective(&mut b, 1).unwrap();
    put_local_key(&mut b, 1, &epoch1_key).unwrap();

    // A 写一条朋友数据（pdsync 记账，collect_incremental 可采集）。
    let (_, friend_root) = self_identity(9);
    spark_core::sync::put_personal(
        &mut a,
        "node-a",
        &format!("ct:friend:{friend_root}"),
        &serde_json::to_string(&friend_record_with_peer(&friend_root, "peer-x")).unwrap(),
        NOW,
    )
    .unwrap();
    // A 已知 B 的生效 epoch=1（B 此前 hello 宣告；need body 不携带 epoch）。
    spark_core::sync::pdsync::set_remote_epoch(&mut a, "peer-self-other", 1).unwrap();

    // B 发 need：对 ct:friend 一无所知（knownVv 空）→ A 全量响应。
    let need_body = json!({ "category": "ct:friend", "knownVv": {}, "dlogAck": 0 });
    let r = deliver_pdsync(
        &mut a,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_NEED,
        need_body,
        "node-b",
    );
    assert_eq!(r.response, json!({ "ok": true }));

    // A 的响应批次：恰好一个 Data，记录 value 为 ikey 密文、外层不含明文。
    let mut cipher_records: Vec<(String, serde_json::Value)> = Vec::new();
    for out in &r.pdsync_out {
        let body = out.body().clone();
        assert_eq!(
            body.get("category").and_then(serde_json::Value::as_str),
            Some("ct:friend"),
            "need 响应应为 ct:friend data"
        );
        for rec in body
            .get("records")
            .and_then(serde_json::Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let key_ = rec
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();
            let value = rec.get("value").cloned().unwrap_or(json!(null));
            assert!(
                spark_core::epoch::is_ikey_ciphertext(&value),
                "need 响应 value 应为 ikey 密文，实为 {value}"
            );
            assert_eq!(
                value.get("epoch").and_then(serde_json::Value::as_u64),
                Some(1),
                "need 响应密文 epoch=1"
            );
            assert!(
                !serde_json::to_string(&value).unwrap().contains("nickname"),
                "密文外层不得含明文"
            );
            cipher_records.push((key_, value));
        }
    }
    assert_eq!(cipher_records.len(), 1, "A 应推回 1 条加密朋友记录");

    // A 的 data 响应送达 B：B 有 epoch1 密钥 → 解开 → 合入。
    let data_body = json!({
        "category": "ct:friend",
        "records": cipher_records
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": v, "meta": remote_meta("node-a", 1, NOW) }))
            .collect::<Vec<_>>(),
        "batchSeq": 0,
        "batchTotal": 1,
    });
    let r2 = deliver_pdsync(
        &mut b,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_DATA,
        data_body,
        "node-b",
    );
    assert_eq!(
        r2.response,
        json!({ "ok": true }),
        "B 应接受并合入 need 响应"
    );
    let stored = ContactService::get_friend(&b, &friend_root)
        .unwrap()
        .expect("B 解开后合入朋友");
    assert_eq!(
        stored.peers.first().map(|p| p.peer_id.as_str()),
        Some("peer-x"),
        "B 解开 epoch1 密文后合入的朋友 peer 正确"
    );

    // ── Part 2：被撤销 C 向不知情 D 发 need → D 用 epoch2 加密 → C 解不开 ──
    let mut c = MemoryStorage::new(); // C：被撤销，无 epoch2 密钥
    let mut d = MemoryStorage::new(); // D：不知情，epoch2 生效

    // D epoch2 生效 + 持 epoch2 密钥。
    let epoch2_key: [u8; 32] = [0x62; 32];
    put_effective(&mut d, 2).unwrap();
    put_local_key(&mut d, 2, &epoch2_key).unwrap();
    // C 无任何本机密钥、effective=0（被撤销后密钥被清）。
    assert_eq!(spark_core::epoch::get_effective(&c).unwrap(), 0);

    // D 写一条 epoch2 加密的新朋友数据。
    let (_, friend_y) = self_identity(10);
    spark_core::sync::put_personal(
        &mut d,
        "node-d",
        &format!("ct:friend:{friend_y}"),
        &serde_json::to_string(&friend_record_with_peer(&friend_y, "peer-y")).unwrap(),
        NOW,
    )
    .unwrap();
    // D 认为 C 的生效 epoch=2（C 撤销前宣告过 epoch2——R1 攻击：D 不知情）。
    spark_core::sync::pdsync::set_remote_epoch(&mut d, "peer-self-other", 2).unwrap();

    // C 向 D 发 need。
    let need_c = json!({ "category": "ct:friend", "knownVv": {}, "dlogAck": 0 });
    let rd = deliver_pdsync(
        &mut d,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_NEED,
        need_c,
        "node-d",
    );
    assert_eq!(rd.response, json!({ "ok": true }));

    // D 的响应：epoch2 密文。
    let mut d_ciphers: Vec<(String, serde_json::Value)> = Vec::new();
    for out in &rd.pdsync_out {
        for rec in out
            .body()
            .get("records")
            .and_then(serde_json::Value::as_array)
            .unwrap_or(&Vec::new())
        {
            let key_ = rec
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();
            let value = rec.get("value").cloned().unwrap_or(json!(null));
            assert!(
                spark_core::epoch::is_ikey_ciphertext(&value),
                "D 响应应为 epoch2 密文"
            );
            assert_eq!(
                value.get("epoch").and_then(serde_json::Value::as_u64),
                Some(2)
            );
            d_ciphers.push((key_, value));
        }
    }
    assert_eq!(d_ciphers.len(), 1, "D 应推回 1 条 epoch2 密文");

    // D 的 data 送达 C：C 无 epoch2 密钥 → 解不开 → 记录不落库（R1 锁死）。
    let data_c = json!({
        "category": "ct:friend",
        "records": d_ciphers
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": v, "meta": remote_meta("node-d", 1, NOW) }))
            .collect::<Vec<_>>(),
        "batchSeq": 0,
        "batchTotal": 1,
    });
    let rc = deliver_pdsync(
        &mut c,
        &key,
        &my_root,
        dm_envelope::KIND_PDSYNC_DATA,
        data_c,
        "node-c",
    );
    assert_eq!(
        rc.response,
        json!({ "ok": true }),
        "C 解不开也应 ok（不解不推进）"
    );
    assert!(
        ContactService::get_friend(&c, &friend_y).unwrap().is_none(),
        "C 无 epoch2 密钥不得合入新数据（R1 攻击路径锁死）"
    );
    assert!(
        c.get(&format!("ct:friend:{friend_y}")).unwrap().is_none(),
        "C 库中无此记录"
    );
}

// ---------------------------------------------------------------------------
// R4 回归（team-lead 统一回归 ②）：epoch 生效下 Normal pdoc 明文照推。
// 对端宣告 epoch>0、pdoc 声明为 Normal（无 sensitivity）→ 推送 value 为明文
// （非 `$enc`）且记录包含在批次中（不得被 Skip 丢弃，否则插件数据停止同步）。
// 该用例是 R4 修复（classify_pdoc Normal→Plain）的端到端验收。
// ---------------------------------------------------------------------------

#[test]
fn epoch_active_normal_pdoc_pushed_as_plaintext_end_to_end() {
    let now = real_now_ms();
    let mut sender = MemoryStorage::new();
    let (key, my_root) = self_identity(1);

    // 插件集合声明：Normal（缺省 sensitivity，非 sensitive）。
    sender
        .put(
            "pdecl:notes@v1",
            &json!({
                "name": "notes",
                "version": "v1",
                "collections": []
            })
            .to_string(),
        )
        .unwrap();
    // 一条 Normal 插件数据记录（pdsync 记账，可采集）。
    spark_core::sync::put_personal(
        &mut sender,
        NODE,
        "pdoc:notes@v1:doc-1",
        &json!({ "title": "普通笔记", "body": "明文内容" }).to_string(),
        now,
    )
    .unwrap();

    // 发送侧 epoch1 生效。
    let epoch1_key: [u8; 32] = [0x73; 32];
    put_local_key(&mut sender, 1, &epoch1_key).unwrap();
    put_effective(&mut sender, 1).unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&sender).unwrap(),
        1,
        "前置 effective=1"
    );

    // 对端 hello 宣告 epoch=1 → P4 消息窗口/数据推。
    let hello_body = json!({
        "categories": {},
        "msgWindow": { "maxAgeMs": 86_400_000i64, "maxPerConv": 500 },
        "attachmentPolicy": "eager",
        "epoch": 1,
    });
    let hello_env = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_HELLO,
        &my_root,
        &my_root,
        now,
        hello_body,
        &key,
    );
    let hello_result = handle_inbound_dm(
        &mut sender,
        &my_root,
        "我",
        hello_env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();
    assert!(
        !hello_result.pdsync_out.is_empty(),
        "hello 应触发 pdoc 推送"
    );

    // 收集 pdoc 批次的记录：Normal pdoc → 明文照推（value 非 $enc），且含在批次内。
    let mut saw_pdoc = false;
    for out in &hello_result.pdsync_out {
        let body = out.body().clone();
        if body.get("category").and_then(serde_json::Value::as_str) != Some("pdoc") {
            continue;
        }
        let records = body
            .get("records")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for rec in &records {
            let rec_key = rec
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if rec_key != "pdoc:notes@v1:doc-1" {
                continue;
            }
            saw_pdoc = true;
            let value = rec.get("value").cloned().unwrap_or(json!(null));
            assert!(
                !spark_core::epoch::is_ikey_ciphertext(&value),
                "Normal pdoc 应明文照推（非 $enc 密文），实为 {value}"
            );
            assert_eq!(
                value.get("title").and_then(serde_json::Value::as_str),
                Some("普通笔记"),
                "Normal pdoc 明文 value 完整可读"
            );
        }
    }
    assert!(
        saw_pdoc,
        "Normal pdoc 记录必须被推送（明文），不得被 Skip 丢弃——R4 回归"
    );
}

// ---------------------------------------------------------------------------
// R4 回归（team-lead 统一回归 ③ 边角一）：effective>0 但本机无该 epoch 密钥 →
// Encrypt 类记录不得明文泄漏（Skip），Normal pdoc（Plain）仍明文照推。
// 触发：有效 effective 但未 put_local_key，hello 宣告 epoch>0。
// ---------------------------------------------------------------------------

#[test]
fn encrypt_record_skipped_when_effective_key_missing_but_plain_pushed() {
    let now = real_now_ms();
    let mut sender = MemoryStorage::new();
    let (key, my_root) = self_identity(1);

    // Normal pdoc 声明 + 数据。
    sender
        .put(
            "pdecl:notes@v1",
            &json!({ "name": "notes", "version": "v1", "collections": [] }).to_string(),
        )
        .unwrap();
    spark_core::sync::put_personal(
        &mut sender,
        NODE,
        "pdoc:notes@v1:doc-1",
        &json!({ "title": "普通笔记" }).to_string(),
        now,
    )
    .unwrap();
    // Encrypt 类记录：msg:item。
    let (_, peer_root) = self_identity(7);
    let conv_id = direct_conversation_id(&peer_root);
    MessageService::upsert_conversation(&mut sender, PERSONAL, &conv_record(&conv_id, &peer_root))
        .unwrap();
    let item_msg = MessageRecord {
        id: "enc-missing-key".to_string(),
        sender_id: my_root.clone(),
        sender_name: "我".to_string(),
        msg_type: MessageType::Text,
        content: "密钥缺失不得明文外泄".to_string(),
        created_at: now,
        ..Default::default()
    };
    let item_key = message_key(PERSONAL, &conv_id, now, "enc-missing-key");
    sender
        .put(&item_key, &serde_json::to_string(&item_msg).unwrap())
        .unwrap();

    // effective=1 但未 put_local_key（密钥缺失）→ encrypt_records_for_push 的
    // get_local_key None 分支：宁可整批不推也不明文泄漏（fail-closed 整批丢弃）。
    put_effective(&mut sender, 1).unwrap();
    assert_eq!(spark_core::epoch::get_effective(&sender).unwrap(), 1);

    let hello_body = json!({
        "categories": {},
        "msgWindow": { "maxAgeMs": 86_400_000i64, "maxPerConv": 500 },
        "attachmentPolicy": "eager",
        "epoch": 1,
    });
    let hello_env = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_HELLO,
        &my_root,
        &my_root,
        now,
        hello_body,
        &key,
    );
    let result = handle_inbound_dm(
        &mut sender,
        &my_root,
        "我",
        hello_env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();

    // 密钥缺失 → Encrypt 类记录不得明文外泄：msg:item 不出现在任何 data 批次。
    for out in &result.pdsync_out {
        let body = out.body().clone();
        let records = body
            .get("records")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for rec in &records {
            let rec_key = rec
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            assert_ne!(
                rec_key, item_key,
                "Encrypt 类记录密钥缺失不得明文外泄（整批 fail-closed 丢弃），实为 {rec_key}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// R4 回归（team-lead 统一回归 ③ 边角二）：encrypt_records_for_push classify
// Err → Skip。触发：pdecl 存在但 JSON 损坏 → classify_pdoc 返回 Err → 该 pdoc
// 记录被跳过不推（宁可少推不泄露）。
// ---------------------------------------------------------------------------

#[test]
fn malformed_pdecl_classify_error_skips_pdoc_record() {
    let now = real_now_ms();
    let mut sender = MemoryStorage::new();
    let (key, my_root) = self_identity(1);

    // 损坏的 pdecl（非法 JSON）→ classify_pdoc 解析 Err。
    sender.put("pdecl:broken@v1", "{ not-valid-json }").unwrap();
    spark_core::sync::put_personal(
        &mut sender,
        NODE,
        "pdoc:broken@v1:doc-x",
        &json!({ "title": "不应外泄" }).to_string(),
        now,
    )
    .unwrap();

    put_effective(&mut sender, 1).unwrap();
    let epoch1_key: [u8; 32] = [0x74; 32];
    put_local_key(&mut sender, 1, &epoch1_key).unwrap();

    let hello_body = json!({
        "categories": {},
        "msgWindow": { "maxAgeMs": 86_400_000i64, "maxPerConv": 500 },
        "attachmentPolicy": "eager",
        "epoch": 1,
    });
    let hello_env = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_HELLO,
        &my_root,
        &my_root,
        now,
        hello_body,
        &key,
    );
    let result = handle_inbound_dm(
        &mut sender,
        &my_root,
        "我",
        hello_env,
        "peer-self-b",
        &HashSet::new(),
        now,
        NODE,
        None,
    )
    .unwrap();

    // 损坏声明的 pdoc 记录不得被明文推送（classify Err → Skip）。
    for out in &result.pdsync_out {
        let body = out.body().clone();
        if body.get("category").and_then(serde_json::Value::as_str) != Some("pdoc") {
            continue;
        }
        for rec in body
            .get("records")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let rec_key = rec
                .get("key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            assert_ne!(
                rec_key, "pdoc:broken@v1:doc-x",
                "损坏声明的 pdoc 记录不得明文推送（classify Err → Skip）"
            );
        }
    }
}

// ── M3 口令：pwv:self 入站分支 + pwack 批尾钩子（§13.5 乙侧 + §13.4 钩子①）──

#[test]
fn pwv_inbound_new_v_sets_stale_and_keeps_watermark() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(31);

    let salt = [7u8; 16];
    let nonce = [9u8; 12];
    let v = build_value("new-secret", &salt, &nonce, 2000, "me").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&v).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    let stored = pw::get_pwv(&s).unwrap().expect("新 V 已合入");
    assert_eq!(stored.changed_at, 2000);
    assert!(
        pw::get_stale(&s).unwrap(),
        "接受新 V 但未验证 → 置 stale（等待 unlock 自证）"
    );
    assert_eq!(
        pw::get_applied_vts(&s).unwrap(),
        0,
        "入站不得推进水位（留待 maybe_ack_on_unlock 验证后推进）"
    );
}

// ── 自动密码统一（方案 a，wiki/security/password-unify-auto-trigger）────────

#[test]
fn pwv_inbound_auto_unifies_when_session_password_matches_v() {
    // 对端 V 合入（同口令、不同 salt——recover_mnemonic 场景）→ 会话 Kverify
    // 可解新 V → 自愈：清 stale + 推进水位 + 自锚 ack + 幂等标记 + hello 旗标。
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(40);
    let password = "pw-e2e-2026";
    // 本机 V（applied 已推进，模拟 unlock 时已验证）。
    let local_v = build_value(password, &[1u8; 16], &[2u8; 12], 2000, NODE).unwrap();
    pw::put_pwv(&mut s, NODE, &local_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 2000).unwrap();
    // 对端 V：同口令、不同 salt、更新 changedAt（LWW 胜者为对端）。
    let remote_v = build_value(password, &[3u8; 16], &[4u8; 12], 3000, "peer-b").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&remote_v).unwrap(),
        meta: remote_meta("peer-b", 1, NOW),
        dseq: None,
    };
    // Kverify 按当前（合入后对端）salt 派生——等价生产下一条信封的口径。
    let salt2 =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &remote_v.salt).unwrap();
    let kverify = derive_kverify(password, &salt2.try_into().unwrap()).unwrap();
    deliver_pdsync_data_kv(&mut s, &key, &my_root, "pwv", &[record], Some(&kverify));

    assert!(!pw::get_stale(&s).unwrap(), "自愈后 stale 清除");
    assert_eq!(
        pw::get_applied_vts(&s).unwrap(),
        3000,
        "水位推进到胜者的 changedAt"
    );
    assert!(
        pw::get_pwack(&s, NODE).unwrap().is_some(),
        "自锚 ack 落库（D′ 门控闭合的前提）"
    );
    assert!(
        s.get("p2p:pw-autounified:3000").unwrap().is_some(),
        "按 changedAt 幂等标记落库"
    );
    assert!(
        s.get(spark_core::sync::pdsync::HELLO_REQUEST_KEY)
            .unwrap()
            .is_some(),
        "hello 补发旗标（把新 pwack 推给对端闭合门控链）"
    );
}

#[test]
fn pwv_inbound_no_auto_unify_when_password_mismatches_v() {
    // 真改密竞态：对端 V 由新口令生成，本机会话仍是旧口令 → Kverify 解不开
    // 新 V → 不自愈（stale 保留、无标记），等用户输入新口令后走 unlock 路径。
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(41);
    let local_v = build_value("old-pw", &[1u8; 16], &[2u8; 12], 2000, NODE).unwrap();
    pw::put_pwv(&mut s, NODE, &local_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 2000).unwrap();
    // 对端 V：新口令生成。
    let remote_v = build_value("new-pw", &[3u8; 16], &[4u8; 12], 3000, "peer-b").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&remote_v).unwrap(),
        meta: remote_meta("peer-b", 1, NOW),
        dseq: None,
    };
    let salt2 =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &remote_v.salt).unwrap();
    let kverify = derive_kverify("old-pw", &salt2.try_into().unwrap()).unwrap();
    deliver_pdsync_data_kv(&mut s, &key, &my_root, "pwv", &[record], Some(&kverify));

    assert!(pw::get_stale(&s).unwrap(), "口令不符 → stale 保留");
    assert_eq!(pw::get_applied_vts(&s).unwrap(), 2000, "水位不动");
    assert!(
        s.get("p2p:pw-autounified:3000").unwrap().is_none(),
        "无幂等标记 = 未发生自愈"
    );
}

#[test]
fn pwv_inbound_replay_below_watermark_ignored() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(32);
    pw::put_applied_vts(&mut s, 5000).unwrap();

    let salt = [7u8; 16];
    let nonce = [9u8; 12];
    let v = build_value("old-secret", &salt, &nonce, 2000, "me").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&v).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    assert!(
        pw::get_pwv(&s).unwrap().is_none(),
        "changedAt <= 已应用水位 → 回放忽略，不得覆写"
    );
    assert!(!pw::get_stale(&s).unwrap(), "回放不置 stale");
}

#[test]
fn pwv_inbound_replay_merges_remote_vv_into_pmeta() {
    // 回放忽略内容，但对端 vv 分量须「见讫记账」并入本键 pmeta——否则本键
    // 折叠 vv 永不收敛，每轮 hello 都判 Concurrent/LocalBehind 双向重复互推。
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(34);

    // 本机已有更新水位（changedAt=6000）的 pwv，pmeta = {NODE:1}。
    let local_v = build_value("local-secret", &[8u8; 16], &[10u8; 12], 6000, "me").unwrap();
    pw::put_pwv(&mut s, NODE, &local_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 5000).unwrap();

    // 对端推来旧 pwv（changedAt=2000 <= 水位 5000 → 回放丢弃），vv={peerB:4}。
    let old_v = build_value("old-secret", &[7u8; 16], &[9u8; 12], 2000, "me").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&old_v).unwrap(),
        meta: remote_meta("peerB", 4, NOW),
        dseq: None,
    };
    deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    let stored = pw::get_pwv(&s).unwrap().expect("本机 pwv 保留");
    assert_eq!(stored.changed_at, 6000, "回放不得覆写内容");
    assert_eq!(pw::get_applied_vts(&s).unwrap(), 5000, "水位不变");
    assert!(!pw::get_stale(&s).unwrap(), "回放不置 stale");
    let meta = get_personal_meta(&s, pw::PWV_KEY)
        .unwrap()
        .expect("pmeta 存在");
    assert_eq!(meta.vv.get(NODE).copied(), Some(1), "本机分量保留");
    assert_eq!(
        meta.vv.get("peerB").copied(),
        Some(4),
        "对端分量须见讫记账并入 pmeta（折叠 vv 收敛前提）"
    );
}

#[test]
fn pwv_inbound_future_ts_rejected() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(33);

    // changedAt 超过 now + 时间窗：伪造 V 推死水位的 DoS 防护。
    let future = (NOW as u64) + dm_envelope::ENVELOPE_TS_WINDOW_MS as u64 + 60_000;
    let salt = [7u8; 16];
    let nonce = [9u8; 12];
    let v = build_value("future-secret", &salt, &nonce, future, "me").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&v).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    assert!(
        pw::get_pwv(&s).unwrap().is_none(),
        "未来时间戳的 V 拒收（不落库不推水位）"
    );
    assert_eq!(pw::get_applied_vts(&s).unwrap(), 0);
}

#[test]
fn pwv_inbound_preserves_last_good() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(34);

    // 本地已有已验证 V（changedAt=1000）+ 水位 1000。
    let salt = [7u8; 16];
    let nonce = [9u8; 12];
    let old_v = build_value("old-secret", &salt, &nonce, 1000, "me").unwrap();
    pw::put_pwv(&mut s, NODE, &old_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 1000).unwrap();

    // 入站新 V（changedAt=2000），remote meta vv 大于本地 → Applied。
    let new_v = build_value("new-secret", &salt, &nonce, 2000, "me").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&new_v).unwrap(),
        meta: remote_meta(NODE, 2, NOW),
        dseq: None,
    };
    deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    let last_good = pw::get_last_good_v(&s).unwrap().expect("last-good 已保留");
    assert_eq!(last_good.changed_at, 1000, "接受新 V 前保留已验证 V 副本");
    let stored = pw::get_pwv(&s).unwrap().expect("新 V 已合入");
    assert_eq!(stored.changed_at, 2000);
    assert!(pw::get_stale(&s).unwrap(), "新 V 未验证 → stale");
}

#[test]
fn pwack_inbound_unlocked_anchors_verified_vts() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(35);

    let salt = [7u8; 16];
    let kverify = derive_kverify("correct-horse", &salt).unwrap();
    let peer = "peer-b";
    let ack = build_ack(&kverify, peer, 2000);
    let record = PdsyncRecord {
        key: format!("{}{}", pw::PWACK_PREFIX, peer),
        value: serde_json::to_value(&ack).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    deliver_pdsync_data_kv(&mut s, &key, &my_root, "pwack", &[record], Some(&kverify));

    assert_eq!(
        pw::get_last_verified_vts(&s, peer).unwrap(),
        2000,
        "解锁态：批尾钩子① 逐条 verify_and_anchor_ack"
    );
}

#[test]
fn pwack_inbound_locked_persists_without_anchor() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(36);

    let salt = [7u8; 16];
    let kverify = derive_kverify("correct-horse", &salt).unwrap();
    let peer = "peer-b";
    let ack = build_ack(&kverify, peer, 2000);
    let record = PdsyncRecord {
        key: format!("{}{}", pw::PWACK_PREFIX, peer),
        value: serde_json::to_value(&ack).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    // 锁定态：kverify=None → ack 落库但批尾不锚定（解锁后由钩子②兜底）。
    deliver_pdsync_data(&mut s, &key, &my_root, "pwack", &[record]);

    assert!(
        pw::get_pwack(&s, peer).unwrap().is_some(),
        "锁定期间到达的 ack 落库持久"
    );
    assert_eq!(
        pw::get_last_verified_vts(&s, peer).unwrap(),
        0,
        "未解锁不锚定"
    );
}

#[test]
fn pwack_inbound_bad_mac_not_anchored() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(37);

    let salt = [7u8; 16];
    let kverify = derive_kverify("correct-horse", &salt).unwrap();
    let peer = "peer-b";
    let mut ack = build_ack(&kverify, peer, 2000);
    // 篡改 MAC：翻转 base64 首字符。
    let flipped = (ack.mac.as_bytes()[0] ^ 0x01) as char;
    ack.mac.replace_range(0..1, &flipped.to_string());
    let record = PdsyncRecord {
        key: format!("{}{}", pw::PWACK_PREFIX, peer),
        value: serde_json::to_value(&ack).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    deliver_pdsync_data_kv(&mut s, &key, &my_root, "pwack", &[record], Some(&kverify));

    assert!(
        pw::get_pwack(&s, peer).unwrap().is_some(),
        "MAC 篡改的 ack 仍落库（正确性由锚定校验把关）"
    );
    assert_eq!(
        pw::get_last_verified_vts(&s, peer).unwrap(),
        0,
        "MAC 校验失败 → 零状态写入，不推进锚"
    );
}

// ── E5 实时事件（A45）：pwv 入站置 stale → PasswordChangeObserved ──────────

/// 新 V 合入且 stale → 广播 PasswordChangeObserved（在线接收设备实时引导）；
/// 无同 ts epoch:state 时 reason 兜底 password_change。
#[test]
fn pwv_inbound_stale_emits_password_change_observed() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(50);
    let v = build_value("new-secret", &[7u8; 16], &[9u8; 12], 2000, "peer-a").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&v).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    let observed = result
        .events
        .iter()
        .find_map(|e| match e {
            P2pEvent::PasswordChangeObserved {
                rotated_at,
                rotated_by,
                rotated_by_device,
                reason,
            } => Some((*rotated_at, rotated_by.clone(), rotated_by_device.clone(), reason.clone())),
            _ => None,
        })
        .expect("stale 时应发 PasswordChangeObserved");
    assert_eq!(observed.0, 2000);
    assert_eq!(observed.1, "peer-a");
    assert_eq!(observed.2, "peer-a", "无设备记录时回退 changedBy");
    assert_eq!(observed.3, "password_change", "无同 ts epoch:state → 兜底");
}

/// reason 以 epoch:state 为权威（时戳同源：rotatedAt == changedAt 时采信）。
#[test]
fn pwv_inbound_event_reason_from_matching_epoch_state() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(51);
    // epoch:state：password_reset 轮换于 NOW（与 incoming V 的 changedAt 同 ts）。
    let self_x25519 = spark_core::epoch::ed_sk_to_x25519(&[1; 32]);
    spark_core::epoch::EpochService::rotate(
        &mut s,
        &"root-x".to_string(),
        NODE,
        NODE,
        NOW,
        spark_core::epoch::RotationReason::PasswordReset,
        &self_x25519,
        &[],
        None,
    )
    .unwrap();
    let v = build_value("new-secret", &[7u8; 16], &[9u8; 12], NOW as u64, "peer-a").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&v).unwrap(),
        meta: remote_meta(NODE, 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);
    let reason = result
        .events
        .iter()
        .find_map(|e| match e {
            P2pEvent::PasswordChangeObserved { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("应发事件");
    assert_eq!(reason, "password_reset", "同 ts epoch:state 的 reason 为权威");
}

/// 自愈成功（会话口令可解新 V）→ stale 清除，不发事件（无需用户动作）。
#[test]
fn pwv_inbound_auto_unify_suppresses_event() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(52);
    let password = "pw-e2e-2026";
    let local_v = build_value(password, &[1u8; 16], &[2u8; 12], 2000, NODE).unwrap();
    pw::put_pwv(&mut s, NODE, &local_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 2000).unwrap();
    let remote_v = build_value(password, &[3u8; 16], &[4u8; 12], 3000, "peer-b").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&remote_v).unwrap(),
        meta: remote_meta("peer-b", 1, NOW),
        dseq: None,
    };
    let salt2 =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &remote_v.salt).unwrap();
    let kverify = derive_kverify(password, &salt2.try_into().unwrap()).unwrap();
    let result = deliver_pdsync_data_kv(&mut s, &key, &my_root, "pwv", &[record], Some(&kverify));

    assert!(!pw::get_stale(&s).unwrap(), "自愈成功");
    assert!(
        result
            .events
            .iter()
            .all(|e| !matches!(e, P2pEvent::PasswordChangeObserved { .. })),
        "自愈无用户动作 → 不发事件"
    );
}

/// 回放忽略（changedAt <= applied）→ 不置 stale 不发事件（改密端本机回环同构：
/// 本机发布的 V 已推进 applied，回环到达走此分支，天然不重复自报）。
#[test]
fn pwv_inbound_replay_emits_nothing() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(53);
    let local_v = build_value("pw", &[1u8; 16], &[2u8; 12], 5000, NODE).unwrap();
    pw::put_pwv(&mut s, NODE, &local_v, NOW).unwrap();
    pw::put_applied_vts(&mut s, 5000).unwrap();
    let replay_v = build_value("pw", &[3u8; 16], &[4u8; 12], 2000, "peer-a").unwrap();
    let record = PdsyncRecord {
        key: pw::PWV_KEY.to_string(),
        value: serde_json::to_value(&replay_v).unwrap(),
        meta: remote_meta("peer-a", 1, NOW),
        dseq: None,
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "pwv", &[record]);

    assert!(!pw::get_stale(&s).unwrap(), "回放不置 stale");
    assert!(
        result
            .events
            .iter()
            .all(|e| !matches!(e, P2pEvent::PasswordChangeObserved { .. })),
        "回放不发事件"
    );
}
