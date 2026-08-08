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
use spark_core::kernel::{direct_conversation_id, dm_envelope, handle_inbound_dm};
use spark_core::message::{
    AppMessageRecord, ConversationKind, ConversationRecord, MessageRecord, MessageService,
    MessageType, app_conversation_id, app_message_key, message_id_index_key, message_key,
};
use spark_core::p2p::P2pEvent;
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::pdsync::{PdsyncRecord, build_data_batch, build_hello, self_friend_key};
use spark_core::sync::{get_personal_meta, is_tombstone};

use common::*;

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
    let body = build_data_batch(category, records, 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        my_root,
        my_root,
        NOW,
        body,
        key,
    );
    handle_inbound_dm(storage, my_root, "我", envelope, "peer-self-b", &HashSet::new(), NOW, NODE)
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
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "p2p", &[evil]);
    assert_eq!(result.response["ok"], false, "非注册 category 的键整批拒收");
    assert!(s.get("p2p:identity:privateKey").unwrap().is_none());

    // `pmeta:*`（同步元数据）同样不在注册表内
    let evil = PdsyncRecord {
        key: "pmeta:ct:friend:a".to_string(),
        value: json!({}),
        meta: remote_meta("peer-node-b", 1, NOW),
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[evil]);
    assert_eq!(result.response["ok"], false);
    assert!(s.get("pmeta:ct:friend:a").unwrap().is_none());

    // 声明 category 与记录键不一致：整批拒收，合法键也不落库
    let mixed = PdsyncRecord {
        key: "ct:friend:a".to_string(),
        value: json!({"rootId": "a"}),
        meta: remote_meta("peer-node-b", 1, NOW),
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

    // 设备记录 → DeviceUpdated（data 即 DeviceRecord JSON）
    let device = PdsyncRecord {
        key: "device:peer-x".to_string(),
        value: json!({"peerId": "peer-x", "nickname": "另一台设备"}),
        meta: remote_meta("peer-node-b", 1, NOW),
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
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "msg:item", &[message]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(
        s.get(&msg_key).unwrap().is_some(),
        true,
        "消息本体落库"
    );
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
    assert_eq!(receiver.get(&idx).unwrap().as_deref(), Some(item_key.as_str()));
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
    )
    .unwrap();
    assert_eq!(r.response, json!({ "ok": true }));

    let conv_key = format!("msg:conv:personal:{conv_id}");
    let pmeta = get_personal_meta(&s, &conv_key)
        .unwrap()
        .expect("入站消息应 bump 会话壳 pmeta");
    assert_eq!(pmeta.vv.get(NODE), Some(&1), "pmeta 计数器来自本机 node");
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
        peer: Some(PeerRef {
            peer_id: peer_id.to_string(),
            addresses: Vec::new(),
        }),
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
    handle_inbound_dm(storage, my_root, "我", envelope, "peer-self-other", &HashSet::new(), NOW, node)
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
    ContactService::upsert_friend_pdsync(
        &mut a,
        &friend_record_with_peer(&my_root, "peer-device-b"),
        NOW,
        "node-a",
    )
    .unwrap();
    ContactService::upsert_friend_pdsync(
        &mut a,
        &friend_record_with_peer(&peer_root, "peer-x"),
        NOW,
        "node-a",
    )
    .unwrap();
    // B：自记录 peer→"peer-device-a"（A 的设备；ts 更晚——无排除时并发
    // LWW 必然毒化 A 的自记录）
    ContactService::upsert_friend_pdsync(
        &mut b,
        &friend_record_with_peer(&my_root, "peer-device-a"),
        NOW + 1000,
        "node-b",
    )
    .unwrap();

    // A → B：hello（排除自记录键）→ B 的 need/data 过链路
    let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
    let r = deliver_pdsync(&mut b, &key, &my_root, dm_envelope::KIND_PDSYNC_HELLO, hello_a, "node-b");
    let bodies: Vec<_> = r.pdsync_out.iter().map(|o| o.body().clone()).collect();
    route_pdsync_out(bodies, &mut b, "node-b", &mut a, "node-a", &key, &my_root);
    // B → A：反向
    let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
    let r = deliver_pdsync(&mut a, &key, &my_root, dm_envelope::KIND_PDSYNC_HELLO, hello_b, "node-a");
    let bodies: Vec<_> = r.pdsync_out.iter().map(|o| o.body().clone()).collect();
    route_pdsync_out(bodies, &mut a, "node-a", &mut b, "node-b", &key, &my_root);

    // 两端自记录保持各自原值（peer 各指向对方设备，未被互灌）
    let fa = ContactService::get_friend(&a, &my_root)
        .unwrap()
        .expect("A 自记录仍在");
    assert_eq!(
        fa.peer.map(|p| p.peer_id).as_deref(),
        Some("peer-device-b"),
        "A 自记录 peer 不得被 B 的版本覆盖"
    );
    let fb = ContactService::get_friend(&b, &my_root)
        .unwrap()
        .expect("B 自记录仍在");
    assert_eq!(
        fb.peer.map(|p| p.peer_id).as_deref(),
        Some("peer-device-a"),
        "B 自记录 peer 不得被 A 的版本覆盖"
    );
    // 普通朋友记录照常同步到 B（排除仅针对自记录键，不误伤同 category）
    let fp = ContactService::get_friend(&b, &peer_root)
        .unwrap()
        .expect("普通朋友记录应同步到 B");
    assert_eq!(fp.peer.map(|p| p.peer_id).as_deref(), Some("peer-x"));

    // 收敛：再互发 hello → ct:friend 无 need/data（folded vv 无伪 diff）
    let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
    let r = deliver_pdsync(&mut b, &key, &my_root, dm_envelope::KIND_PDSYNC_HELLO, hello_a2, "node-b");
    assert!(
        r.pdsync_out
            .iter()
            .all(|o| o.body().get("category").and_then(serde_json::Value::as_str)
                != Some("ct:friend")),
        "收敛后 ct:friend 不应再有 diff 输出"
    );
    let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
    let r = deliver_pdsync(&mut a, &key, &my_root, dm_envelope::KIND_PDSYNC_HELLO, hello_b2, "node-a");
    assert!(
        r.pdsync_out
            .iter()
            .all(|o| o.body().get("category").and_then(serde_json::Value::as_str)
                != Some("ct:friend")),
        "收敛后 ct:friend 不应再有 diff 输出"
    );
}

/// apply 侧排除：旧版本对端仍可能推自记录/自记录墓碑——接收侧直接丢弃
/// （幂等 ok，非整批拒收），不覆盖本机自记录、不删本机本体、pmeta 不推进。
#[test]
fn pdsync_data_drops_self_friend_record_and_tombstone() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = self_identity(1);
    ContactService::upsert_friend_pdsync(
        &mut s,
        &friend_record_with_peer(&my_root, "peer-device-b"),
        NOW,
        NODE,
    )
    .unwrap();

    // 对端视角的自记录：peer 指向「本机设备」（对它而言的对端），vv/ts 均
    // 更新——无排除时必然覆盖本机
    let poisoned = PdsyncRecord {
        key: self_friend_key(&my_root),
        value: serde_json::to_value(friend_record_with_peer(&my_root, "peer-device-a")).unwrap(),
        meta: remote_meta("peer-node-b", 9, NOW + 60_000),
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[poisoned]);
    assert_eq!(result.response, json!({ "ok": true }), "排除是丢弃而非拒批");
    let f = ContactService::get_friend(&s, &my_root).unwrap().unwrap();
    assert_eq!(
        f.peer.map(|p| p.peer_id).as_deref(),
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
    };
    let result = deliver_pdsync_data(&mut s, &key, &my_root, "ct:friend", &[tomb]);
    assert_eq!(result.response, json!({ "ok": true }));
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_some(),
        "自记录墓碑不得删本机本体"
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
    });
    MessageService::upsert_conversation(&mut s, PERSONAL, &local).unwrap();

    // 对端设备快照：同一会话、peer 指向「本机设备」（对它而言的对端）
    let mut remote = conv_record(&conv_id, &my_root);
    remote.peer = Some(PeerRef {
        peer_id: "peer-device-a".to_string(),
        addresses: Vec::new(),
    });
    remote.pinned_at = 123;
    let record = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: serde_json::to_value(&remote).unwrap(),
        meta: remote_meta("peer-node-b", 1, NOW),
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
