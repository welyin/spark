//! kernel 消息门面与 dm 入站编排集成测试：
//! - 门面：ensure_direct 幂等与 `dm:` id 约定、无 p2p 发送落库 failed、
//!   resend 状态门槛、recall 窗口、mark_read/clear/delete、视图 'me' 映射；
//! - 入站：手工签名 chat/read/recall/friend-request/friend-accept 信封直调
//!   `handle_inbound_dm`，断言落库、事件、拉黑拒收、验签失败、组织非成员拒绝。

mod common;

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::contact::{ContactService, FriendRecord};
use spark_core::kernel::{direct_conversation_id, dm_envelope, handle_inbound_dm};
use spark_core::message::{
    ConversationKind, ConversationRecord, MessageRecord, MessageService, MessageType,
};
use spark_core::org::OrganizationService;
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::P2pEvent;
use spark_core::p2p::node::system_now_ms;
use spark_core::storage::MemoryStorage;

use common::*;

const PERSONAL: &str = "personal";
const NOW: i64 = 1_720_000_000_000;

fn peer_root(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

fn chat_body(space: &str, from: &str, msg_id: &str, text: &str) -> Value {
    let record = MessageRecord {
        id: msg_id.to_string(),
        sender_id: from.to_string(),
        sender_name: "对方昵称".to_string(),
        msg_type: MessageType::Text,
        content: text.to_string(),
        created_at: NOW,
        ..Default::default()
    };
    json!({ "spaceKey": space, "message": serde_json::to_value(&record).unwrap() })
}

fn make_conversation(id: &str, peer_root_id: &str) -> ConversationRecord {
    ConversationRecord {
        id: id.to_string(),
        kind: ConversationKind::Direct,
        title: "对方".to_string(),
        peer_root_id: peer_root_id.to_string(),
        updated_at: NOW,
        ..Default::default()
    }
}

fn make_message(id: &str, sender: &str, status: Option<&str>) -> MessageRecord {
    MessageRecord {
        id: id.to_string(),
        sender_id: sender.to_string(),
        sender_name: "对方".to_string(),
        msg_type: MessageType::Text,
        content: "hi".to_string(),
        created_at: NOW,
        status: status.map(str::to_string),
        ..Default::default()
    }
}

fn friend_record(root_id: &str, blocked: bool) -> FriendRecord {
    FriendRecord {
        root_id: root_id.to_string(),
        nickname: "朋友昵称".to_string(),
        signature: String::new(),
        gender: None,
        added_at: NOW,
        peer: None,
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked,
    }
}

// ---------------------------------------------------------------------------
// 门面：会话与发送
// ---------------------------------------------------------------------------

#[test]
fn ensure_direct_idempotent_with_dm_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);

    let first = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();
    assert_eq!(first.id, direct_conversation_id(&peer));
    assert!(first.id.starts_with("dm:"), "direct 会话 id 为 dm: 前缀");
    assert!(!first.online, "p2p 停止时 online 恒 false");

    let second = kernel.message_ensure_direct(PERSONAL, &peer, "另一个标题").unwrap();
    assert_eq!(second.id, first.id, "ensure 幂等：不重复建会话");
    assert_eq!(second.title, "对方", "已有会话标题不被覆盖");

    let list = kernel.message_list_conversations(PERSONAL).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].peer_id, peer);
}

#[test]
fn send_text_without_p2p_fails_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();

    let view = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-1", "你好", None)
        .unwrap();
    assert_eq!(view.sender_id, "me", "自己发的消息 senderId 映射为 me");
    assert_eq!(view.sender_name, "我", "自己发的消息 senderName 映射为 我");
    assert_eq!(view.status.as_deref(), Some("failed"), "无 p2p 投递失败");

    let messages = kernel.message_list_messages(PERSONAL, &conv.id).unwrap();
    assert_eq!(messages.len(), 1, "失败消息同样落库");
    assert_eq!(messages[0].id, "msg-1");
    assert_eq!(messages[0].content, "你好");
    assert_eq!(messages[0].status.as_deref(), Some("failed"));

    let storage = kernel.__test_storage().unwrap();
    let stored = MessageService::get_messages(&storage, PERSONAL, &conv.id).unwrap();
    assert_eq!(stored[0].status.as_deref(), Some("failed"));
}

#[test]
fn resend_requires_failed_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();

    // 注入一条 delivered 消息：非 failed 不可重发
    let mut storage = kernel.__test_storage().unwrap();
    let record = make_message("msg-delivered", &kernel.current_root_id().unwrap().unwrap(), Some("delivered"));
    MessageService::append_message(&mut storage, PERSONAL, &conv.id, &record).unwrap();
    let err = kernel
        .message_resend(PERSONAL, &conv.id, "msg-delivered")
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "仅失败或发送中的消息可以重发",
        "delivered 不可重发，文案与实现口径一致（failed/sending 可重发）"
    );

    // failed 消息可重发（无 p2p 仍 failed，但流程走通）
    kernel
        .message_send_text(PERSONAL, &conv.id, "msg-failed", "重发我", None)
        .unwrap();
    let view = kernel
        .message_resend(PERSONAL, &conv.id, "msg-failed")
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("failed"));
}

#[test]
fn recall_window_and_local_ops() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();
    kernel
        .message_send_text(PERSONAL, &conv.id, "msg-1", "撤回我", None)
        .unwrap();

    // 窗口内：撤回成功；重复撤回失败
    assert!(kernel.message_recall(PERSONAL, &conv.id, "msg-1").unwrap());
    assert!(!kernel.message_recall(PERSONAL, &conv.id, "msg-1").unwrap());
    let messages = kernel.message_list_messages(PERSONAL, &conv.id).unwrap();
    assert!(messages[0].recalled);

    // 超窗（3 分钟前）：撤回失败（自己发的消息，归属校验通过、窗口拒绝）
    let mut storage = kernel.__test_storage().unwrap();
    let mut old = make_message("msg-old", &kernel.current_root_id().unwrap().unwrap(), None);
    old.created_at = system_now_ms() - 3 * 60_000;
    MessageService::append_message(&mut storage, PERSONAL, &conv.id, &old).unwrap();
    assert!(!kernel.message_recall(PERSONAL, &conv.id, "msg-old").unwrap());

    // mark_read / draft / pin / mute
    MessageService::increment_unread(&mut storage, PERSONAL, &conv.id).unwrap();
    kernel.message_mark_read(PERSONAL, &conv.id).unwrap();
    kernel.message_set_draft(PERSONAL, &conv.id, "草稿").unwrap();
    kernel.message_toggle_pin(PERSONAL, &conv.id).unwrap();
    kernel.message_toggle_mute(PERSONAL, &conv.id).unwrap();
    let view = &kernel.message_list_conversations(PERSONAL).unwrap()[0];
    assert_eq!(view.unread_count, 0);
    assert_eq!(view.draft, "草稿");
    assert!(view.pinned_at > 0);
    assert!(view.muted);

    // clear：消息清空、会话保留
    kernel.message_clear(PERSONAL, &conv.id).unwrap();
    assert!(kernel.message_list_messages(PERSONAL, &conv.id).unwrap().is_empty());
    assert_eq!(kernel.message_list_conversations(PERSONAL).unwrap().len(), 1);

    // delete：会话与消息一并删除
    kernel
        .message_send_text(PERSONAL, &conv.id, "msg-2", "再发", None)
        .unwrap();
    kernel.message_delete(PERSONAL, &conv.id, "msg-2").unwrap();
    kernel.message_delete_conversation(PERSONAL, &conv.id).unwrap();
    assert!(kernel.message_list_conversations(PERSONAL).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 入站编排：chat / read / recall
// ---------------------------------------------------------------------------

#[test]
fn inbound_chat_persists_and_emits() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hello"),
        &key,
    );

    let result = handle_inbound_dm(&mut s, &my_root, "我昵称", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(result.events.len(), 1);
    let P2pEvent::ChatReceived(data) = &result.events[0] else {
        panic!("应发出 ChatReceived 事件");
    };
    assert_eq!(data["spaceKey"], PERSONAL);
    assert_eq!(data["conversation"]["id"], direct_conversation_id(&from));
    // 事件会话为 append/unread 之后的最新快照
    assert_eq!(data["conversation"]["unreadCount"], 1);
    assert_eq!(data["conversation"]["updatedAt"], NOW);
    // 事件消息做 'me' 映射（与列表水合口径一致）：对端消息保持真实 rootId
    assert_eq!(data["message"]["senderId"], from);
    assert_eq!(data["message"]["senderName"], "对方昵称");

    let conv = MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&from))
        .unwrap()
        .expect("会话已建");
    assert_eq!(conv.title, "对方昵称", "无朋友记录时标题取 senderName");
    assert_eq!(conv.peer.as_ref().unwrap().peer_id, "peer-xyz");
    assert_eq!(conv.unread_count, 1);
    let messages = MessageService::get_messages(&s, PERSONAL, &conv.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, None, "入站消息不携带发送状态");
    assert_eq!(messages[0].content, "hello");
}

#[test]
fn inbound_chat_title_prefers_friend_remark() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let mut friend = friend_record(&from, false);
    friend.remark = "备注名".to_string();
    ContactService::upsert_friend(&mut s, &friend).unwrap();

    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    let conv = MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&from))
        .unwrap()
        .unwrap();
    assert_eq!(conv.title, "备注名", "会话标题优先朋友备注");
}

#[test]
fn inbound_read_marks_my_messages_read() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let conv_id = direct_conversation_id(&from);
    MessageService::upsert_conversation(&mut s, PERSONAL, &make_conversation(&conv_id, &from))
        .unwrap();
    MessageService::append_message(&mut s, PERSONAL, &conv_id, &make_message("m1", &my_root, Some("delivered")))
        .unwrap();
    MessageService::append_message(&mut s, PERSONAL, &conv_id, &make_message("m2", &from, None))
        .unwrap();

    let envelope = dm_envelope::build_envelope(
        "read",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": PERSONAL }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let P2pEvent::ChatStatus(data) = &result.events[0] else {
        panic!("应发出 ChatStatus 事件");
    };
    assert_eq!(data["peerRead"], true);
    assert_eq!(data["convId"], conv_id);

    let messages = MessageService::get_messages(&s, PERSONAL, &conv_id).unwrap();
    assert_eq!(messages[0].status.as_deref(), Some("read"), "我发的消息置已读");
    assert_eq!(messages[1].status, None, "对方发的消息不受影响");
}

#[test]
fn inbound_read_without_conversation_emits_nothing() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let envelope = dm_envelope::build_envelope(
        "read",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": PERSONAL }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }), "应答仍 ok");
    assert!(result.events.is_empty(), "无实际改动不发 peerRead 事件");
}

#[test]
fn inbound_recall_marks_recalled() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let conv_id = direct_conversation_id(&from);
    MessageService::upsert_conversation(&mut s, PERSONAL, &make_conversation(&conv_id, &from))
        .unwrap();
    MessageService::append_message(&mut s, PERSONAL, &conv_id, &make_message("m1", &from, None))
        .unwrap();

    let envelope = dm_envelope::build_envelope(
        "recall",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": PERSONAL, "messageId": "m1" }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let P2pEvent::ChatStatus(data) = &result.events[0] else {
        panic!("应发出 ChatStatus 事件");
    };
    assert_eq!(data["recalled"], true);
    assert_eq!(data["messageId"], "m1");
    let messages = MessageService::get_messages(&s, PERSONAL, &conv_id).unwrap();
    assert!(messages[0].recalled, "入站 recall 不判 2 分钟窗口");
}

// ---------------------------------------------------------------------------
// 入站编排：friend-request / friend-accept
// ---------------------------------------------------------------------------

#[test]
fn inbound_friend_request_idempotent() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let body = json!({
        "requestId": "req-1",
        "nickname": "小明",
        "message": "交个朋友",
        "source": "扫码",
        "nodeInfo": { "peerId": "peer-a", "addresses": ["/ip4/1.2.3.4/tcp/9000"] },
    });
    let envelope = dm_envelope::build_envelope("friend-request", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "我昵称", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true, "nickname": "我昵称" }));
    // 入站申请 id 为复合形式 {from}:{原 requestId}（防跨发送者撞 id）
    let composite_id = format!("{from}:req-1");
    let P2pEvent::FriendRequestReceived(data) = &result.events[0] else {
        panic!("应发出 FriendRequestReceived 事件");
    };
    assert_eq!(data["request"]["id"], json!(composite_id));

    let stored = ContactService::get_incoming_request(&s, &composite_id).unwrap().unwrap();
    assert_eq!(stored.root_id, from);
    assert_eq!(stored.peer.as_ref().unwrap().peer_id, "peer-a");

    // 幂等：同 rootId 再次申请（不同 requestId、不带 nodeInfo）更新原记录，
    // 不产生重复；寻址信息（peer）保留原值
    let body2 = json!({
        "requestId": "req-2",
        "nickname": "小明2",
        "message": "再试一次",
        "source": "扫码",
    });
    let envelope2 =
        dm_envelope::build_envelope("friend-request", &from, &my_root, NOW + 1, body2, &key);
    handle_inbound_dm(&mut s, &my_root, "我昵称", envelope2, "peer-a", &HashSet::new(), NOW).unwrap();
    let overview = ContactService::overview(&s, PERSONAL).unwrap();
    assert_eq!(overview.requests.len(), 1, "同 rootId pending 申请幂等更新");
    assert_eq!(overview.requests[0].id, composite_id, "保留原申请 id");
    assert_eq!(overview.requests[0].nickname, "小明2");
    assert_eq!(
        overview.requests[0].peer.as_ref().unwrap().peer_id,
        "peer-a",
        "重试不带 nodeInfo 时 peer 保留原值"
    );
}

#[test]
fn inbound_friend_accept_builds_friend() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let outgoing = ContactService::create_outgoing_request(
        &mut s, &from, "", "hi", "扫码", None, NOW,
    )
    .unwrap();

    let body = json!({
        "requestId": outgoing.id,
        "nickname": "对方昵称",
        "nodeInfo": { "peerId": "peer-a", "addresses": [] },
    });
    let envelope = dm_envelope::build_envelope("friend-accept", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let P2pEvent::FriendRequestAccepted(data) = &result.events[0] else {
        panic!("应发出 FriendRequestAccepted 事件");
    };
    assert_eq!(data["request"]["status"], "accepted");
    assert_eq!(data["friend"]["rootId"], from);

    let friend = ContactService::get_friend(&s, &from).unwrap().expect("朋友已建");
    assert_eq!(friend.nickname, "对方昵称");
    assert_eq!(friend.peer.as_ref().unwrap().peer_id, "peer-a");
}

#[test]
fn inbound_friend_accept_composite_id_compat() {
    // 旧版对端回发的 requestId 误带 `{from}:` 复合前缀（其本地入站记录 id），
    // 接收侧归一化为原始 id 后照常接受
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let outgoing = ContactService::create_outgoing_request(
        &mut s, &from, "", "hi", "扫码", None, NOW,
    )
    .unwrap();

    let body = json!({
        "requestId": format!("{from}:{}", outgoing.id),
        "nickname": "对方昵称",
    });
    let envelope = dm_envelope::build_envelope("friend-accept", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let stored = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(stored.status, spark_core::contact::FriendRequestStatus::Accepted);
    assert!(ContactService::get_friend(&s, &from).unwrap().is_some());
}

#[test]
fn inbound_friend_accept_forgery_rejected() {
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let third = "dd".repeat(32);
    let accept_body = |request_id: &str| {
        json!({ "requestId": request_id, "nickname": "攻击者" })
    };

    // (a) 申请不存在：不落朋友、不发事件、ok:false
    let mut s = MemoryStorage::new();
    let envelope =
        dm_envelope::build_envelope("friend-accept", &from, &my_root, NOW, accept_body("req-x"), &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-body" }));
    assert!(result.events.is_empty());
    assert!(ContactService::get_friend(&s, &from).unwrap().is_none());

    // (b) 申请 pending 但指向第三方：from 与 record.rootId 不符 → 拒
    let mut s = MemoryStorage::new();
    let outgoing =
        ContactService::create_outgoing_request(&mut s, &third, "", "hi", "扫码", None, NOW)
            .unwrap();
    let envelope = dm_envelope::build_envelope(
        "friend-accept",
        &from,
        &my_root,
        NOW,
        accept_body(&outgoing.id),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-body" }));
    assert!(result.events.is_empty());
    // 第三方申请未被标记 accepted，攻击者未成为朋友
    let still = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(still.status, spark_core::contact::FriendRequestStatus::Pending);
    assert!(ContactService::get_friend(&s, &from).unwrap().is_none());

    // (c) 申请已 accepted（重放）：非 pending → 拒
    let mut s = MemoryStorage::new();
    let outgoing =
        ContactService::create_outgoing_request(&mut s, &from, "", "hi", "扫码", None, NOW)
            .unwrap();
    ContactService::mark_outgoing_accepted(&mut s, &outgoing.id, NOW).unwrap();
    let envelope = dm_envelope::build_envelope(
        "friend-accept",
        &from,
        &my_root,
        NOW,
        accept_body(&outgoing.id),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-body" }));
    assert!(result.events.is_empty());
    assert!(ContactService::get_friend(&s, &from).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// 入站编排：拒绝路径
// ---------------------------------------------------------------------------

#[test]
fn inbound_blocked_rejects_chat_and_request() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 拉黑集合独立于朋友记录：陌生人（无 friend 记录）拉黑即生效
    ContactService::set_blocked(&mut s, PERSONAL, &from, true, NOW).unwrap();

    let chat = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", chat, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "blocked" }));
    assert!(result.events.is_empty());
    assert!(
        MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&from))
            .unwrap()
            .is_none(),
        "被拉黑落库为空"
    );

    let req = dm_envelope::build_envelope(
        "friend-request",
        &from,
        &my_root,
        NOW,
        json!({ "requestId": "req-1", "nickname": "x" }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", req, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "blocked" }));
}

#[test]
fn inbound_invalid_envelope_rejected() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);

    // 验签失败：篡改 body
    let mut envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    envelope["body"]["spaceKey"] = json!("tampered");
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "bad-signature" }));

    // 非发给我
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &"cc".repeat(32),
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-for-me" }));

    // pubKey 与 from 不绑定
    let (other_key, _) = peer_root(9);
    let mut envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &other_key,
    );
    envelope["from"] = json!(from);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "bad-pubkey" }));

    // 未知 kind
    let envelope = dm_envelope::build_envelope("weird", &from, &my_root, NOW, json!({}), &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "unknown-kind" }));
}

#[test]
fn inbound_org_chat_member_check() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);

    // 非成员（组织不存在但 spaceKey 形状合法）：not-member
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body("org:org_0123456789abcdef", &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-member" }));

    // 建组织并把 from 加为成员：放行
    let org = OrganizationService::create_organization(
        &mut s,
        &CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            base_plugin_domain: Some("plugin:notes".to_string()),
        },
        &my_root,
        NOW,
    )
    .unwrap();
    OrganizationService::add_member(&mut s, &org.org_id, &from, None, &my_root, NOW).unwrap();
    let space = format!("org:{}", org.org_id);
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(&space, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let messages =
        MessageService::get_messages(&s, &space, &direct_conversation_id(&from)).unwrap();
    assert_eq!(messages.len(), 1, "组织成员消息落库");
}

// ---------------------------------------------------------------------------
// 自己作为会话对端（同身份多设备同步）
// ---------------------------------------------------------------------------

#[test]
fn send_text_to_self_delivered_and_online() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _) = init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();

    let conv = kernel.message_ensure_direct(PERSONAL, &root_id, "我").unwrap();
    assert!(conv.online, "自己的会话 online 恒 true");

    // 无 p2p、无配对设备：本机副本天然送达，status 仍 delivered
    let view = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-self-1", "同步到各设备", None)
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("delivered"));
    assert_eq!(view.sender_id, "me");

    let messages = kernel.message_list_messages(PERSONAL, &conv.id).unwrap();
    assert_eq!(messages[0].status.as_deref(), Some("delivered"));

    // resend 自消息：重投向各设备投递，状态仍 delivered
    let view = kernel
        .message_resend(PERSONAL, &conv.id, "msg-self-1")
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("delivered"));

    // 会话列表里自己的会话 online 恒 true（p2p 未启动也一样）
    let convs = kernel.message_list_conversations(PERSONAL).unwrap();
    assert!(convs[0].online);
}

#[test]
fn inbound_chat_from_self_no_unread() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = peer_root(7);
    // from==to==我：同身份另一台设备同步过来的自消息
    let envelope = dm_envelope::build_envelope(
        "chat",
        &my_root,
        &my_root,
        NOW,
        chat_body(PERSONAL, &my_root, "m1", "来自另一台设备"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "我昵称", envelope, "peer-b", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(result.events.len(), 1, "事件照发（前端按 senderId 渲染）");
    let P2pEvent::ChatReceived(data) = &result.events[0] else {
        panic!("应发出 ChatReceived 事件");
    };
    assert_eq!(data["conversation"]["unreadCount"], 0, "自消息事件快照未读为 0");
    assert_eq!(
        data["message"]["senderId"], "me",
        "自己设备同步来的消息 senderId 映射为 me（与列表水合口径一致）"
    );
    assert_eq!(data["message"]["senderName"], "我");

    let conv = MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&my_root))
        .unwrap()
        .expect("会话已建");
    assert_eq!(conv.unread_count, 0, "自己的消息不产生未读");
    let messages = MessageService::get_messages(&s, PERSONAL, &conv.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].read, "自消息落库即置本地已读标记");
}

#[test]
fn inbound_friend_request_from_self_auto_accept() {
    let mut s = MemoryStorage::new();
    let (key, my_root) = peer_root(7);
    let body = json!({
        "requestId": "req-dev-1",
        "nickname": "设备B",
        "message": "",
        "source": "扫码",
        "nodeInfo": { "peerId": "peer-dev-b", "addresses": ["/ip4/1.2.3.4/tcp/9001"] },
    });
    let envelope =
        dm_envelope::build_envelope("friend-request", &my_root, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "我昵称", envelope, "peer-dev-b", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true, "nickname": "我昵称" }));

    // 自动接受：直接建设备 FriendRecord，不产生「新的朋友」申请
    let device = ContactService::get_friend(&s, &my_root)
        .unwrap()
        .expect("设备记录已建");
    assert_eq!(device.nickname, "设备B");
    assert_eq!(device.peer.as_ref().unwrap().peer_id, "peer-dev-b");
    let overview = ContactService::overview(&s, PERSONAL).unwrap();
    assert!(overview.requests.is_empty(), "设备配对不产生申请记录");

    // auto_accept 回发指令：目标为请求方 nodeInfo，requestId 原样回显
    let auto = result.auto_accept.expect("带 auto_accept 标志");
    assert_eq!(auto.request_id, "req-dev-1");
    assert_eq!(auto.to_root_id, my_root, "设备配对回发 to=自己");
    assert_eq!(auto.target.peer_id.as_deref(), Some("peer-dev-b"));
    assert_eq!(auto.target.addresses, vec!["/ip4/1.2.3.4/tcp/9001".to_string()]);
}

#[test]
fn inbound_friend_request_from_friend_reaccepts() {
    // 已是朋友（对方曾接受过我的申请，但没收到 accept 回执，其 outbox 仍
    // pending）又发来申请：不再生成申请，回发 friend-accept 重确认
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    ContactService::upsert_friend(&mut s, &friend_record(&from, false)).unwrap();
    let body = json!({
        "requestId": "req-retry-1",
        "nickname": "对方",
        "nodeInfo": { "peerId": "peer-a", "addresses": ["/ip4/1.2.3.4/tcp/9000"] },
    });
    let envelope = dm_envelope::build_envelope("friend-request", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "我昵称", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true, "nickname": "我昵称" }));

    let overview = ContactService::overview(&s, PERSONAL).unwrap();
    assert!(overview.requests.is_empty(), "已是朋友不再生成申请记录");
    let auto = result.auto_accept.expect("回发 friend-accept 重确认");
    assert_eq!(auto.request_id, "req-retry-1");
    assert_eq!(auto.to_root_id, from, "重确认回发 to=请求方");
    assert_eq!(auto.target.peer_id.as_deref(), Some("peer-a"));
}

#[test]
fn inbound_chat_implicitly_accepts_pending_outgoing() {
    // 我主动发过申请且仍 pending（对方接受了但 accept 回执丢失）：对方先来
    // 消息即视为已通过——outbox 置 accepted、建朋友、发 FriendRequestAccepted，
    // 消息照常落库
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let outgoing = ContactService::create_outgoing_request(
        &mut s, &from, "", "交个朋友", "名片", None, NOW,
    )
    .unwrap();

    let body = chat_body(PERSONAL, &from, "msg-1", "在吗");
    let envelope = dm_envelope::build_envelope("chat", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "我", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    assert_eq!(result.events.len(), 2, "FriendRequestAccepted + ChatReceived");
    let P2pEvent::FriendRequestAccepted(data) = &result.events[0] else {
        panic!("首个事件应为 FriendRequestAccepted");
    };
    assert_eq!(data["request"]["id"], json!(outgoing.id));
    assert_eq!(data["request"]["status"], "accepted");
    assert_eq!(data["friend"]["rootId"], json!(from));

    let stored = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(stored.status, spark_core::contact::FriendRequestStatus::Accepted);
    let friend = ContactService::get_friend(&s, &from).unwrap().expect("隐含确认建朋友");
    assert_eq!(friend.nickname, "对方昵称", "昵称取消息自报 senderName");
    let conv = MessageService::find_direct_conversation(&s, PERSONAL, &from).unwrap().unwrap();
    assert_eq!(conv.title, "对方昵称", "朋友先建，会话标题取朋友昵称");
    let messages = MessageService::get_messages(&s, PERSONAL, &conv.id).unwrap();
    assert_eq!(messages.len(), 1, "消息照常落库");
}

#[test]
fn inbound_forged_self_envelope_rejected() {
    let mut s = MemoryStorage::new();
    let (_, my_root) = peer_root(7);
    let (other_key, _) = peer_root(9);
    // from 伪造为我的 rootId，但签名/公钥是别人的 key → bad-pubkey
    let mut envelope = dm_envelope::build_envelope(
        "friend-request",
        &my_root,
        &my_root,
        NOW,
        json!({ "requestId": "req-x", "nickname": "伪造者" }),
        &other_key,
    );
    envelope["from"] = json!(my_root);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-x", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "bad-pubkey" }));
    assert!(result.auto_accept.is_none());
    assert!(
        ContactService::get_friend(&s, &my_root).unwrap().is_none(),
        "伪造信封不落设备记录"
    );
}


// ---------------------------------------------------------------------------
// 安全/正确性回归：地址回退、recall 归属、senderId 绑定、按 id 去重、
// 未来窗、信封 stale、spaceKey 形状、friend-accept 合并
// ---------------------------------------------------------------------------

#[test]
fn send_falls_back_to_friend_addresses_when_conv_peer_empty() {
    // p2p 运行中（init_identity 登录链路自动启动）
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();
    // 模拟入站建的会话：peer 只有 peerId 无地址
    let mut storage = kernel.__test_storage().unwrap();
    let mut stored = MessageService::get_conversation(&storage, PERSONAL, &conv.id)
        .unwrap()
        .unwrap();
    stored.peer = Some(spark_core::message::PeerRef {
        peer_id: "peer-x".to_string(),
        addresses: vec![],
    });
    MessageService::upsert_conversation(&mut storage, PERSONAL, &stored).unwrap();

    // 朋友记录带地址：回退命中 → 投递 spawn，命令立即返回 sending
    let mut f = friend_record(&peer, false);
    f.peer = Some(spark_core::message::PeerRef {
        peer_id: "peer-y".to_string(),
        addresses: vec!["/ip4/127.0.0.1/tcp/19999".to_string()],
    });
    ContactService::upsert_friend(&mut storage, &f).unwrap();
    let view = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-f2", "hi", None)
        .unwrap();
    assert_eq!(
        view.status.as_deref(),
        Some("sending"),
        "回退到朋友记录地址后投递 spawn，命令立即返回 sending"
    );

    // 会话 peer 与朋友记录都无地址：同步判 failed
    stored.peer = None;
    MessageService::upsert_conversation(&mut storage, PERSONAL, &stored).unwrap();
    f.peer = None;
    ContactService::upsert_friend(&mut storage, &f).unwrap();
    let view = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-f1", "hi", None)
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("failed"));
}

#[test]
fn outbound_recall_rejects_peer_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();
    let mut storage = kernel.__test_storage().unwrap();
    MessageService::append_message(&mut storage, PERSONAL, &conv.id, &make_message("m1", &peer, None))
        .unwrap();
    let err = kernel.message_recall(PERSONAL, &conv.id, "m1").unwrap_err();
    assert_eq!(err.to_string(), "只能撤回自己发送的消息");
    assert!(
        !MessageService::get_messages(&storage, PERSONAL, &conv.id).unwrap()[0].recalled,
        "对方消息未被撤回"
    );
}

#[test]
fn inbound_recall_cannot_recall_my_message() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let conv_id = direct_conversation_id(&from);
    MessageService::upsert_conversation(&mut s, PERSONAL, &make_conversation(&conv_id, &from))
        .unwrap();
    // 我发出的消息
    MessageService::append_message(&mut s, PERSONAL, &conv_id, &make_message("m1", &my_root, Some("delivered")))
        .unwrap();

    let envelope = dm_envelope::build_envelope(
        "recall",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": PERSONAL, "messageId": "m1" }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }), "幂等应答 ok");
    assert!(result.events.is_empty(), "归属不匹配不发事件");
    assert!(
        !MessageService::get_messages(&s, PERSONAL, &conv_id).unwrap()[0].recalled,
        "对端不能撤回我方消息"
    );
}

#[test]
fn inbound_chat_binds_sender_id_to_envelope_from() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 对端自报 senderId 伪造为「我」（签名照过——签的是 body 整体）
    let mut body = chat_body(PERSONAL, &from, "m1", "hi");
    body["message"]["senderId"] = json!(my_root);
    let envelope = dm_envelope::build_envelope("chat", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let messages =
        MessageService::get_messages(&s, PERSONAL, &direct_conversation_id(&from)).unwrap();
    assert_eq!(messages[0].sender_id, from, "senderId 强制绑定信封 from");
}

#[test]
fn inbound_chat_dedupes_by_message_id() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let conv_id = direct_conversation_id(&from);
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hello"),
        &key,
    );
    let first = handle_inbound_dm(&mut s, &my_root, "", envelope.clone(), "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(first.events.len(), 1);

    // 同 id 重放：幂等 ok，不重复 append/未读/事件
    let second = handle_inbound_dm(&mut s, &my_root, "", envelope.clone(), "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(second.response, json!({ "ok": true }));
    assert!(second.events.is_empty());
    let messages = MessageService::get_messages(&s, PERSONAL, &conv_id).unwrap();
    assert_eq!(messages.len(), 1, "重放不重复落库");
    let conv = MessageService::get_conversation(&s, PERSONAL, &conv_id).unwrap().unwrap();
    assert_eq!(conv.unread_count, 1, "重放不重复计数未读");

    // 重放不撤销已撤回状态（recall 后重放原消息仍 recalled）
    MessageService::force_recall(&mut s, PERSONAL, &conv_id, "m1", &from).unwrap();
    let third = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert!(third.events.is_empty());
    let messages = MessageService::get_messages(&s, PERSONAL, &conv_id).unwrap();
    assert!(messages[0].recalled, "重放不撤销 recalled");
}

#[test]
fn inbound_chat_rejects_far_future_message() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let mut body = chat_body(PERSONAL, &from, "m1", "hi");
    body["message"]["createdAt"] = json!(NOW + 11 * 60_000);
    let envelope = dm_envelope::build_envelope("chat", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-message" }));
    assert!(result.events.is_empty());
    assert!(
        MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&from))
            .unwrap()
            .is_none(),
        "远未来消息拒收，不建会话"
    );
}

#[test]
fn inbound_stale_envelope_rejected() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 过旧（ts 比 now 早 11 分钟）
    let old = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW - 11 * 60_000,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", old, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "stale" }));
    assert!(result.events.is_empty());
    // 过新（ts 比 now 晚 11 分钟）
    let future = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW + 11 * 60_000,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", future, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "stale" }));
}

#[test]
fn inbound_space_key_injection_rejected() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    for space in [
        "personal:x",                // 冒号注入：绕过校验且落在 personal 前缀内
        "Personal",                  // 大小写
        "org:o1",                    // orgId 形状非法
        "org:org_ABCDEF0123456789",  // 大写 hex
        "org:org_0123456789abcde",   // 15 位
        "org:org_0123456789abcdef:extra", // 多余冒号段
        "weird",
    ] {
        let envelope = dm_envelope::build_envelope(
            "chat",
            &from,
            &my_root,
            NOW,
            chat_body(space, &from, "m1", "hi"),
            &key,
        );
        let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
        assert_eq!(
            result.response,
            json!({ "ok": false, "reason": "invalid-body" }),
            "spaceKey {space:?} 应拒收"
        );
        assert!(result.events.is_empty());
    }
    // 形状合法但不存在的 org：落到 not-member 而非 invalid-body
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body("org:org_0123456789abcdef", &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-member" }));
}

#[test]
fn inbound_friend_accept_merges_existing_friend() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 既有朋友记录：带本地资料（备注/标签/分组）与既有 addedAt
    let mut existing = friend_record(&from, false);
    existing.remark = "旧备注".to_string();
    existing.tag_ids = vec!["tag-1".to_string()];
    existing.group_id = "group-1".to_string();
    existing.added_at = NOW - 1000;
    ContactService::upsert_friend(&mut s, &existing).unwrap();
    let outgoing = ContactService::create_outgoing_request(
        &mut s, &from, "", "hi", "扫码", None, NOW,
    )
    .unwrap();

    let body = json!({
        "requestId": outgoing.id,
        "nickname": "新昵称",
        "nodeInfo": { "peerId": "peer-a", "addresses": ["/ip4/1.2.3.4/tcp/9000"] },
    });
    let envelope = dm_envelope::build_envelope("friend-accept", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));

    let friend = ContactService::get_friend(&s, &from).unwrap().unwrap();
    assert_eq!(friend.nickname, "新昵称", "非空 nickname 刷新");
    assert_eq!(friend.peer.as_ref().unwrap().peer_id, "peer-a", "Some peer 刷新");
    assert_eq!(friend.remark, "旧备注", "本地资料保留");
    assert_eq!(friend.tag_ids, vec!["tag-1".to_string()], "标签保留");
    assert_eq!(friend.group_id, "group-1", "分组保留");
    assert_eq!(friend.added_at, NOW - 1000, "addedAt 保留首次时间");
}

// ---------------------------------------------------------------------------
// 三轮评审修复回归：resend 门槛与文案、read/recall 组织成员校验、
// friend-accept 拉黑、会话 peer 回填、created_at 下限、正文长度上限、
// ChatReceived online 标志
// ---------------------------------------------------------------------------

#[test]
fn resend_rejects_recalled_and_allows_sending() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();

    // 已撤回的消息不可重发（防对端「复活」已撤回内容；自己会话同口径）
    kernel
        .message_send_text(PERSONAL, &conv.id, "msg-recall", "撤回后重发", None)
        .unwrap();
    assert!(kernel.message_recall(PERSONAL, &conv.id, "msg-recall").unwrap());
    let err = kernel
        .message_resend(PERSONAL, &conv.id, "msg-recall")
        .unwrap_err();
    assert_eq!(err.to_string(), "已撤回的消息不能重发");

    // 卡在 sending 的消息可重发（崩溃恢复路径；无 p2p 仍 failed，但流程走通）
    let mut storage = kernel.__test_storage().unwrap();
    let my_root = kernel.current_root_id().unwrap().unwrap();
    MessageService::append_message(
        &mut storage,
        PERSONAL,
        &conv.id,
        &make_message("msg-stuck", &my_root, Some("sending")),
    )
    .unwrap();
    let view = kernel
        .message_resend(PERSONAL, &conv.id, "msg-stuck")
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("failed"));
}

#[test]
fn inbound_read_recall_require_org_membership() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let org = OrganizationService::create_organization(
        &mut s,
        &CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            base_plugin_domain: None,
        },
        &my_root,
        NOW,
    )
    .unwrap();
    let space = format!("org:{}", org.org_id);

    // 非成员：read / recall 均 not-member（与 chat 对齐）
    let read_env = dm_envelope::build_envelope(
        "read",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": space }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", read_env, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-member" }));
    let recall_env = dm_envelope::build_envelope(
        "recall",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": space, "messageId": "m1" }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", recall_env, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "not-member" }));

    // 成员：放行（无会话/消息时幂等 ok，不发事件）
    OrganizationService::add_member(&mut s, &org.org_id, &from, None, &my_root, NOW).unwrap();
    let read_env = dm_envelope::build_envelope(
        "read",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": space }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", read_env, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let recall_env = dm_envelope::build_envelope(
        "recall",
        &from,
        &my_root,
        NOW,
        json!({ "spaceKey": space, "messageId": "m1" }),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", recall_env, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
}

#[test]
fn inbound_friend_accept_rejected_when_blocked() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let outgoing = ContactService::create_outgoing_request(
        &mut s, &from, "", "hi", "扫码", None, NOW,
    )
    .unwrap();
    // 申请存在且 pending，但 from 已被拉黑（拉黑集合独立于朋友记录）
    ContactService::set_blocked(&mut s, PERSONAL, &from, true, NOW).unwrap();

    let body = json!({ "requestId": outgoing.id, "nickname": "对方昵称" });
    let envelope = dm_envelope::build_envelope("friend-accept", &from, &my_root, NOW, body, &key);
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-a", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "blocked" }));
    assert!(result.events.is_empty());
    assert!(
        ContactService::get_friend(&s, &from).unwrap().is_none(),
        "被拉黑者的 accept 不建朋友"
    );
    let still = ContactService::get_outgoing_request(&s, &outgoing.id).unwrap().unwrap();
    assert_eq!(
        still.status,
        spark_core::contact::FriendRequestStatus::Pending,
        "被拉黑不消费出站申请"
    );
}

#[test]
fn inbound_chat_backfills_missing_conv_peer() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 模拟 message_ensure_direct 先建的会话：peer 为 None（无寻址信息）
    let conv_id = direct_conversation_id(&from);
    MessageService::upsert_conversation(&mut s, PERSONAL, &make_conversation(&conv_id, &from))
        .unwrap();
    assert!(
        MessageService::get_conversation(&s, PERSONAL, &conv_id).unwrap().unwrap().peer.is_none()
    );

    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let conv = MessageService::get_conversation(&s, PERSONAL, &conv_id).unwrap().unwrap();
    assert_eq!(
        conv.peer.as_ref().unwrap().peer_id,
        "peer-xyz",
        "peer 为 None 的既有会话回填连接层对端 peerId"
    );
    assert!(conv.peer.as_ref().unwrap().addresses.is_empty());
}

#[test]
fn inbound_chat_rejects_non_positive_created_at() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 负/零 createdAt 会破坏消息键的字典序=时间序，一律拒收
    for bad in [0i64, -1, -1_720_000_000_000] {
        let mut body = chat_body(PERSONAL, &from, &format!("m{bad}"), "hi");
        body["message"]["createdAt"] = json!(bad);
        let envelope = dm_envelope::build_envelope("chat", &from, &my_root, NOW, body, &key);
        let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
        assert_eq!(
            result.response,
            json!({ "ok": false, "reason": "invalid-message" }),
            "createdAt={bad} 应拒收"
        );
        assert!(result.events.is_empty());
    }
    assert!(
        MessageService::get_conversation(&s, PERSONAL, &direct_conversation_id(&from))
            .unwrap()
            .is_none(),
        "负/零时间戳消息拒收，不建会话"
    );
}

#[test]
fn inbound_chat_rejects_oversize_text() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    // 超过 16 KiB（UTF-8 字节）：invalid-message，不落库
    let big = "长".repeat(spark_core::message::MAX_TEXT_BYTES); // 3 字节/字，远超上限
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m-big", &big),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": false, "reason": "invalid-message" }));
    assert!(result.events.is_empty());

    // 恰好 16 KiB：放行
    let exact = "x".repeat(spark_core::message::MAX_TEXT_BYTES);
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m-exact", &exact),
        &key,
    );
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &HashSet::new(), NOW).unwrap();
    assert_eq!(result.response, json!({ "ok": true }));
    let messages =
        MessageService::get_messages(&s, PERSONAL, &direct_conversation_id(&from)).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content.len(), spark_core::message::MAX_TEXT_BYTES);
}

#[test]
fn send_text_rejects_oversize_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    let (_, peer) = peer_root(7);
    let conv = kernel.message_ensure_direct(PERSONAL, &peer, "对方").unwrap();

    let big = "x".repeat(spark_core::message::MAX_TEXT_BYTES + 1);
    let err = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-big", &big, None)
        .unwrap_err();
    assert!(
        err.to_string().contains("长度上限"),
        "超长正文报错，实际：{err}"
    );
    assert!(
        kernel.message_list_messages(PERSONAL, &conv.id).unwrap().is_empty(),
        "超长正文不落库"
    );

    // 恰好 16 KiB：放行（无 p2p 落库 failed）
    let exact = "x".repeat(spark_core::message::MAX_TEXT_BYTES);
    let view = kernel
        .message_send_text(PERSONAL, &conv.id, "msg-exact", &exact, None)
        .unwrap();
    assert_eq!(view.status.as_deref(), Some("failed"));
}

#[test]
fn inbound_chat_event_online_flag_follows_online_peers() {
    let mut s = MemoryStorage::new();
    let my_root = "aa".repeat(32);
    let (key, from) = peer_root(7);
    let envelope = dm_envelope::build_envelope(
        "chat",
        &from,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from, "m1", "hi"),
        &key,
    );
    // 连接层对端 peerId 在在线集合内：事件会话 online = true
    let online: HashSet<String> = ["peer-xyz".to_string()].into_iter().collect();
    let result = handle_inbound_dm(&mut s, &my_root, "", envelope, "peer-xyz", &online, NOW).unwrap();
    let P2pEvent::ChatReceived(data) = &result.events[0] else {
        panic!("应发出 ChatReceived 事件");
    };
    assert_eq!(data["conversation"]["online"], true, "对端在线时 online 为 true");

    // 不在线集合内：online = false
    let (key2, from2) = peer_root(9);
    let envelope2 = dm_envelope::build_envelope(
        "chat",
        &from2,
        &my_root,
        NOW,
        chat_body(PERSONAL, &from2, "m2", "hi"),
        &key2,
    );
    let result =
        handle_inbound_dm(&mut s, &my_root, "", envelope2, "peer-offline", &online, NOW).unwrap();
    let P2pEvent::ChatReceived(data) = &result.events[0] else {
        panic!("应发出 ChatReceived 事件");
    };
    assert_eq!(data["conversation"]["online"], false, "对端不在线时 online 为 false");
}
