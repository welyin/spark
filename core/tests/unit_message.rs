//! message 模块单元测试：1:1 会话与消息存储（对齐 messages.ts store 语义）。

use spark_core::message::{
    ConversationKind, MessageError, MessageRecord, MessageService, MessageType, PeerRef,
    RECALL_WINDOW_MS, generate_message_id, message_key,
};
use spark_core::storage::{MemoryStorage, StorageBackend};

const NOW: i64 = 1_720_000_000_000;
const ME: &str = "aaaa";
const PEER: &str = "bbbb";

fn msg(id: &str, sender_id: &str, created_at: i64, status: Option<&str>) -> MessageRecord {
    MessageRecord {
        id: id.to_string(),
        sender_id: sender_id.to_string(),
        sender_name: if sender_id == ME { "我" } else { "对方" }.to_string(),
        msg_type: MessageType::Text,
        content: format!("content of {id}"),
        created_at,
        status: status.map(str::to_string),
        ..Default::default()
    }
}

/// 建一个 personal 空间的 1:1 会话。
fn setup(storage: &mut MemoryStorage) -> spark_core::message::ConversationRecord {
    MessageService::ensure_direct_conversation(
        storage,
        "personal",
        PEER,
        "王小明",
        Some(PeerRef {
            peer_id: "12D3KooW".to_string(),
            addresses: vec!["/ip4/1.2.3.4/tcp/4001".to_string()],
        }),
        NOW,
    )
    .unwrap()
}

// ---------- 会话建/查/ensure ----------

#[test]
fn ensure_direct_conversation_creates_and_is_idempotent() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    assert_eq!(conv.kind, ConversationKind::Direct);
    assert_eq!(conv.peer_root_id, PEER);
    assert_eq!(conv.title, "王小明");
    assert_eq!(conv.unread_count, 0);
    assert_eq!(conv.pinned_at, 0);
    assert!(!conv.muted);
    assert_eq!(conv.draft, "");
    assert_eq!(conv.updated_at, NOW);
    assert_eq!(conv.peer.as_ref().unwrap().peer_id, "12D3KooW");

    // 幂等：再次 ensure 返回同一会话，不产生新记录
    let again =
        MessageService::ensure_direct_conversation(&mut s, "personal", PEER, "别的标题", None, NOW + 1)
            .unwrap();
    assert_eq!(again.id, conv.id);
    assert_eq!(again.title, "王小明");
    assert_eq!(MessageService::list_conversations(&s, "personal").unwrap().len(), 1);

    // get / find 读取
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap(),
        Some(conv.clone())
    );
    assert_eq!(
        MessageService::find_direct_conversation(&s, "personal", PEER).unwrap(),
        Some(conv)
    );
    assert_eq!(
        MessageService::find_direct_conversation(&s, "personal", "cccc").unwrap(),
        None
    );
}

#[test]
fn spaces_are_isolated() {
    let mut s = MemoryStorage::new();
    setup(&mut s);
    assert_eq!(MessageService::list_conversations(&s, "personal").unwrap().len(), 1);
    assert!(MessageService::list_conversations(&s, "org:o1").unwrap().is_empty());
    // 同 peer 在另一个空间可独立建会话
    let org_conv = MessageService::ensure_direct_conversation(
        &mut s, "org:o1", PEER, "张伟", None, NOW,
    )
    .unwrap();
    assert_eq!(
        MessageService::find_direct_conversation(&s, "org:o1", PEER)
            .unwrap()
            .unwrap()
            .id,
        org_conv.id
    );
    assert_eq!(MessageService::list_conversations(&s, "personal").unwrap().len(), 1);
}

// ---------- 消息 append 与顺序 ----------

#[test]
fn append_message_stores_in_time_order_and_bumps_updated_at() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    // 乱序追加：键内 13 位零填充时间戳保证 scan 字典序 = 时间序
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", PEER, NOW + 2_000, None))
        .unwrap();
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m0", PEER, NOW, None))
        .unwrap();
    MessageService::append_message(
        &mut s,
        "personal",
        &conv.id,
        &msg("m1", ME, NOW + 1_000, Some("sent")),
    )
    .unwrap();

    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert_eq!(
        list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["m0", "m1", "m2"]
    );
    // updatedAt 取现有值与消息 createdAt 的较大者（乱序/补发不回退）
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id)
            .unwrap()
            .unwrap()
            .updated_at,
        NOW + 2_000
    );
}

#[test]
fn append_message_requires_conversation() {
    let mut s = MemoryStorage::new();
    let err = MessageService::append_message(&mut s, "personal", "no-such", &msg("m0", ME, NOW, None))
        .unwrap_err();
    assert!(matches!(err, MessageError::ConversationNotFound));
}

#[test]
fn message_key_uses_zero_padded_timestamp() {
    assert_eq!(
        message_key("personal", "c1", 42, "m1"),
        "msg:item:personal:c1:0000000000042:m1"
    );
    assert_ne!(generate_message_id(NOW), generate_message_id(NOW));
}

// ---------- 状态流转 ----------

#[test]
fn set_message_status_flows_and_skips_recalled() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", ME, NOW, Some("sending")))
        .unwrap();

    for status in ["sent", "delivered", "read"] {
        MessageService::set_message_status(&mut s, "personal", &conv.id, "m1", status).unwrap();
        let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
        assert_eq!(list[0].status.as_deref(), Some(status));
    }

    // 撤回后不再改状态
    assert!(MessageService::recall_message(&mut s, "personal", &conv.id, "m1", NOW + 1_000).unwrap());
    MessageService::set_message_status(&mut s, "personal", &conv.id, "m1", "failed").unwrap();
    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert_eq!(list[0].status.as_deref(), Some("read"));
    assert!(list[0].recalled);

    // 不存在的消息/会话：静默不动（对齐 TS setStatus）
    MessageService::set_message_status(&mut s, "personal", &conv.id, "no-such", "sent").unwrap();
    MessageService::set_message_status(&mut s, "personal", "no-such", "m1", "sent").unwrap();
}

#[test]
fn mark_peer_messages_read_only_touches_own_sent_or_delivered() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    let rows = [
        msg("own-delivered", ME, NOW, Some("delivered")),
        msg("own-sent", ME, NOW + 1, Some("sent")),
        msg("own-read", ME, NOW + 2, Some("read")),
        msg("own-failed", ME, NOW + 3, Some("failed")),
        msg("own-sending", ME, NOW + 4, Some("sending")),
        msg("peer-delivered", PEER, NOW + 5, Some("delivered")),
        msg("peer-none", PEER, NOW + 6, None),
    ];
    for m in &rows {
        MessageService::append_message(&mut s, "personal", &conv.id, m).unwrap();
    }

    // 「对方已读我发的」：调用方传自己的 rootId
    let changed =
        MessageService::mark_peer_messages_read(&mut s, "personal", &conv.id, ME).unwrap();
    assert_eq!(changed, vec!["own-delivered".to_string(), "own-sent".to_string()]);

    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    let status_of = |id: &str| list.iter().find(|m| m.id == id).unwrap().status.clone();
    assert_eq!(status_of("own-delivered").as_deref(), Some("read"));
    assert_eq!(status_of("own-sent").as_deref(), Some("read"));
    assert_eq!(status_of("own-read").as_deref(), Some("read"));
    assert_eq!(status_of("own-failed").as_deref(), Some("failed"));
    assert_eq!(status_of("own-sending").as_deref(), Some("sending"));
    assert_eq!(status_of("peer-delivered").as_deref(), Some("delivered"));
    assert_eq!(status_of("peer-none"), None);

    // 幂等：再跑一次没有可改的
    assert!(MessageService::mark_peer_messages_read(&mut s, "personal", &conv.id, ME)
        .unwrap()
        .is_empty());
}

// ---------- 未读 / 置顶 / 免打扰 / 草稿 ----------

#[test]
fn unread_pin_mute_draft() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);

    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();
    macro_rules! get {
        () => {
            MessageService::get_conversation(&s, "personal", &conv.id)
                .unwrap()
                .unwrap()
        };
    }
    assert_eq!(get!().unread_count, 2);

    MessageService::mark_read(&mut s, "personal", &conv.id).unwrap();
    assert_eq!(get!().unread_count, 0);

    MessageService::toggle_pin(&mut s, "personal", &conv.id, NOW + 9).unwrap();
    assert_eq!(get!().pinned_at, NOW + 9);
    MessageService::toggle_pin(&mut s, "personal", &conv.id, NOW + 10).unwrap();
    assert_eq!(get!().pinned_at, 0);

    MessageService::toggle_mute(&mut s, "personal", &conv.id, NOW + 11).unwrap();
    assert!(get!().muted);
    MessageService::toggle_mute(&mut s, "personal", &conv.id, NOW + 12).unwrap();
    assert!(!get!().muted);

    MessageService::set_draft(&mut s, "personal", &conv.id, "下周的材料", NOW + 13).unwrap();
    assert_eq!(get!().draft, "下周的材料");
    MessageService::set_draft(&mut s, "personal", &conv.id, "", NOW + 14).unwrap();
    assert_eq!(get!().draft, "");

    // 缺失会话的各变更静默不动
    MessageService::increment_unread(&mut s, "personal", "no-such").unwrap();
    MessageService::mark_read(&mut s, "personal", "no-such").unwrap();
    MessageService::toggle_pin(&mut s, "personal", "no-such", NOW).unwrap();
    MessageService::toggle_mute(&mut s, "personal", "no-such", NOW).unwrap();
    MessageService::set_draft(&mut s, "personal", "no-such", "x", NOW).unwrap();
}

// ---------- 清空 / 删除 ----------

#[test]
fn clear_messages_keeps_conversation_and_resets_unread() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    for i in 0..3 {
        MessageService::append_message(
            &mut s,
            "personal",
            &conv.id,
            &msg(&format!("m{i}"), PEER, NOW + i, None),
        )
        .unwrap();
    }
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();

    MessageService::clear_messages(&mut s, "personal", &conv.id).unwrap();
    assert!(MessageService::get_messages(&s, "personal", &conv.id).unwrap().is_empty());
    let conv_after = MessageService::get_conversation(&s, "personal", &conv.id)
        .unwrap()
        .unwrap();
    assert_eq!(conv_after.unread_count, 0);
}

#[test]
fn delete_conversation_removes_everything() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m0", PEER, NOW, None))
        .unwrap();

    MessageService::delete_conversation(&mut s, "personal", &conv.id).unwrap();
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap(),
        None
    );
    assert!(MessageService::get_messages(&s, "personal", &conv.id).unwrap().is_empty());
    // 存储层也无残留
    assert!(s.scan(&spark_core::storage::ScanOptions::prefix("msg:")).unwrap().is_empty());
}

// ---------- 撤回 / 删除消息 ----------

#[test]
fn recall_message_two_minute_window() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", ME, NOW, Some("sent")))
        .unwrap();

    // 边界：恰好 2 分钟仍允许（TS 口径为 > 2min 拒绝）
    assert!(
        MessageService::recall_message(&mut s, "personal", &conv.id, "m1", NOW + RECALL_WINDOW_MS)
            .unwrap()
    );
    // 重复撤回失败
    assert!(
        !MessageService::recall_message(&mut s, "personal", &conv.id, "m1", NOW + 1).unwrap()
    );
    assert!(MessageService::get_messages(&s, "personal", &conv.id).unwrap()[0].recalled);

    // 超过窗口拒绝
    MessageService::append_message(
        &mut s,
        "personal",
        &conv.id,
        &msg("m2", ME, NOW, Some("sent")),
    )
    .unwrap();
    assert!(!MessageService::recall_message(
        &mut s,
        "personal",
        &conv.id,
        "m2",
        NOW + RECALL_WINDOW_MS + 1
    )
    .unwrap());
    assert!(!MessageService::get_messages(&s, "personal", &conv.id).unwrap()[1].recalled);

    // 消息不存在：false
    assert!(!MessageService::recall_message(&mut s, "personal", &conv.id, "no-such", NOW).unwrap());
}

#[test]
fn delete_message_removes_only_target() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    for i in 0..3 {
        MessageService::append_message(
            &mut s,
            "personal",
            &conv.id,
            &msg(&format!("m{i}"), PEER, NOW + i, None),
        )
        .unwrap();
    }

    MessageService::delete_message(&mut s, "personal", &conv.id, "m1").unwrap();
    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert_eq!(
        list.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["m0", "m2"]
    );
    // 不存在时静默不动
    MessageService::delete_message(&mut s, "personal", &conv.id, "no-such").unwrap();
    MessageService::delete_message(&mut s, "personal", "no-such", "m0").unwrap();
}

#[test]
fn delete_peer_message_decrements_unread() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", PEER, NOW, None)).unwrap();
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", ME, NOW + 1, Some("delivered"))).unwrap();
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();

    // 删对端消息且 unread>0：未读 -1
    MessageService::delete_message(&mut s, "personal", &conv.id, "m1").unwrap();
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap().unwrap().unread_count,
        0
    );
    // unread==0 时不出现下溢；删自己发的消息不影响未读
    MessageService::delete_message(&mut s, "personal", &conv.id, "m2").unwrap();
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap().unwrap().unread_count,
        0
    );
}

#[test]
fn force_recall_requires_matching_sender() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", PEER, NOW, None)).unwrap();
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", ME, NOW + 1, Some("delivered"))).unwrap();

    // 归属匹配：发送者撤回自己的消息
    assert!(MessageService::force_recall(&mut s, "personal", &conv.id, "m1", PEER).unwrap());
    // 归属不匹配：对端不能撤回我方消息（幂等 false）
    assert!(!MessageService::force_recall(&mut s, "personal", &conv.id, "m2", PEER).unwrap());
    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert!(list[0].recalled);
    assert!(!list[1].recalled, "sender 不匹配不撤回");
    // 已撤回/不存在：幂等 false
    assert!(!MessageService::force_recall(&mut s, "personal", &conv.id, "m1", PEER).unwrap());
    assert!(!MessageService::force_recall(&mut s, "personal", &conv.id, "no-such", PEER).unwrap());
}

// ---------- 序列化口径 ----------

#[test]
fn records_serialize_camel_case_and_skip_none() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", ME, NOW, None))
        .unwrap();

    let raw_msg = s
        .get(&message_key("personal", &conv.id, NOW, "m1"))
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw_msg).unwrap();
    // camelCase 键名 + `"type"` 原样 + None 字段缺省（对齐 JSON.stringify 丢 undefined）
    for key in ["senderId", "senderName", "type", "createdAt", "recalled"] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    for key in ["fileSize", "duration", "link", "quote", "status"] {
        assert!(v.get(key).is_none(), "unexpected {key}");
    }
    assert_eq!(v["type"], "text");

    let raw_conv = s
        .get(&spark_core::message::conversation_key("personal", &conv.id))
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw_conv).unwrap();
    for key in [
        "peerRootId", "peer", "unreadCount", "pinnedAt", "muted", "draft", "updatedAt",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert_eq!(v["kind"], "direct");
}

// ---------- 三轮评审修复：已读标记 / 状态 CAS / msgId 二级索引 ----------

#[test]
fn mark_read_flags_peer_messages_and_delete_skips_read_ones() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", PEER, NOW, None)).unwrap();
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", PEER, NOW + 1, None)).unwrap();
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();

    // mark_read：未读清零，同时把对端消息批量置本地已读标记
    MessageService::mark_read(&mut s, "personal", &conv.id).unwrap();
    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert!(list.iter().all(|m| m.read), "对端消息均被标记已读");

    // 新到一条未读
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m3", PEER, NOW + 2, None)).unwrap();
    MessageService::increment_unread(&mut s, "personal", &conv.id).unwrap();

    // 删已读的历史消息：不清真正未读的角标（修复前的启发式会误 -1）
    MessageService::delete_message(&mut s, "personal", &conv.id, "m1").unwrap();
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap().unwrap().unread_count,
        1,
        "删已读消息不影响未读"
    );
    // 删真正未读的消息：未读 -1
    MessageService::delete_message(&mut s, "personal", &conv.id, "m3").unwrap();
    assert_eq!(
        MessageService::get_conversation(&s, "personal", &conv.id).unwrap().unwrap().unread_count,
        0
    );
}

#[test]
fn set_message_status_if_sending_is_compare_and_set() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", ME, NOW, Some("sending")))
        .unwrap();

    // sending → delivered：写入成功
    assert!(
        MessageService::set_message_status_if_sending(&mut s, "personal", &conv.id, "m1", "delivered")
            .unwrap()
    );
    // 已是终态：旧投递任务的迟到回写放弃（resend 竞态防护）
    assert!(
        !MessageService::set_message_status_if_sending(&mut s, "personal", &conv.id, "m1", "failed")
            .unwrap()
    );
    let list = MessageService::get_messages(&s, "personal", &conv.id).unwrap();
    assert_eq!(list[0].status.as_deref(), Some("delivered"), "终态不被过期回写覆盖");

    // 已撤回/不存在：均不写入
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", ME, NOW + 1, Some("sending")))
        .unwrap();
    assert!(MessageService::recall_message(&mut s, "personal", &conv.id, "m2", NOW + 2).unwrap());
    assert!(
        !MessageService::set_message_status_if_sending(&mut s, "personal", &conv.id, "m2", "failed")
            .unwrap()
    );
    assert!(
        !MessageService::set_message_status_if_sending(&mut s, "personal", &conv.id, "no-such", "failed")
            .unwrap()
    );
}

#[test]
fn message_id_index_maintained_on_append_delete_clear() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m1", PEER, NOW, None)).unwrap();

    // append 写索引：msg:byid:{space}:{convId}:{msgId} → 消息存储键
    let index_key = spark_core::message::message_id_index_key("personal", &conv.id, "m1");
    assert_eq!(
        s.get(&index_key).unwrap(),
        Some(message_key("personal", &conv.id, NOW, "m1")),
        "append 维护 msgId 二级索引"
    );
    // get_message 走索引直取
    assert_eq!(
        MessageService::get_message(&s, "personal", &conv.id, "m1").unwrap().unwrap().id,
        "m1"
    );

    // delete_message 清理索引
    MessageService::delete_message(&mut s, "personal", &conv.id, "m1").unwrap();
    assert_eq!(s.get(&index_key).unwrap(), None, "delete 清理索引项");
    assert_eq!(MessageService::get_message(&s, "personal", &conv.id, "m1").unwrap(), None);

    // clear_messages 清理全部索引项
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m2", PEER, NOW, None)).unwrap();
    MessageService::append_message(&mut s, "personal", &conv.id, &msg("m3", PEER, NOW + 1, None)).unwrap();
    MessageService::clear_messages(&mut s, "personal", &conv.id).unwrap();
    assert!(
        s.scan(&spark_core::storage::ScanOptions::prefix("msg:byid:")).unwrap().is_empty(),
        "clear 清理会话全部索引项"
    );
}

#[test]
fn get_message_falls_back_to_scan_for_legacy_rows_without_index() {
    let mut s = MemoryStorage::new();
    let conv = setup(&mut s);
    // 模拟索引机制上线前的存量消息：直接写消息键，无 msg:byid 索引项
    let legacy = msg("legacy-1", PEER, NOW, None);
    s.put(
        &message_key("personal", &conv.id, NOW, "legacy-1"),
        &serde_json::to_string(&legacy).unwrap(),
    )
    .unwrap();

    // 索引缺失回退会话内扫描，旧数据的去重/撤回/回写路径语义不变
    let found = MessageService::get_message(&s, "personal", &conv.id, "legacy-1").unwrap();
    assert_eq!(found.as_ref().map(|m| m.id.as_str()), Some("legacy-1"));
    assert!(MessageService::force_recall(&mut s, "personal", &conv.id, "legacy-1", PEER).unwrap());
    MessageService::delete_message(&mut s, "personal", &conv.id, "legacy-1").unwrap();
    assert_eq!(MessageService::get_message(&s, "personal", &conv.id, "legacy-1").unwrap(), None);
}
