//! 消息命令单测：直调 *_inner，不依赖 WebView。

use super::*;
use spark_core::kernel::KernelConfig;
use spark_core::message::{MessageRecord, MessageService, MessageType};
use spark_core::message::types::ConversationKind;
use spark_core::p2p::node::system_now_ms;

const PASSWORD: &str = "correct-horse-battery";
const PERSONAL: &str = "personal";

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

fn peer_id() -> String {
    "dd".repeat(32)
}

/// 直接落一条指定时间/状态的消息（测试辅助）。
fn seed_message(kernel: &mut Kernel, conv_id: &str, id: &str, created_at: i64, status: Option<&str>) {
    let record = MessageRecord {
        id: id.to_string(),
        sender_id: kernel.current_root_id().unwrap().unwrap(),
        sender_name: "alice".to_string(),
        msg_type: MessageType::Text,
        content: format!("content of {id}"),
        file_size: None,
        duration: None,
        link: None,
        quote: None,
        created_at,
        status: status.map(str::to_string),
        recalled: false,
        read: false,
    };
    let mut storage = kernel.__test_storage().unwrap();
    MessageService::append_message(&mut storage, PERSONAL, conv_id, &record).unwrap();
}

#[test]
fn ensure_direct_idempotent() {
    let (_dir, mut kernel) = unlocked_kernel();
    let peer = peer_id();

    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob").unwrap();
    assert_eq!(conv.id, format!("dm:{peer}"));
    assert_eq!(conv.kind, ConversationKind::Direct);
    assert_eq!(conv.peer_id, peer);
    assert_eq!(conv.title, "Bob");
    assert!(!conv.online); // p2p 未启动恒 false

    // 幂等：重复调用同 id，不新增会话（标题保留首次值）
    let again = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob 改名").unwrap();
    assert_eq!(again.id, conv.id);
    let convs = list_conversations_inner(&kernel, PERSONAL).unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].title, "Bob");
}

#[test]
fn send_text_persists_failed_without_p2p() {
    let (_dir, mut kernel) = unlocked_kernel();
    let peer = peer_id();
    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob").unwrap();

    // p2p 未启动 → 落库且 status=failed；视图层 senderId/senderName 映射 me/我
    let view = send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_001", "你好", None, None).unwrap();
    assert_eq!(view.id, "msg_001");
    assert_eq!(view.sender_id, "me");
    assert_eq!(view.sender_name, "我");
    assert_eq!(view.status.as_deref(), Some("failed"));
    assert_eq!(view.content, "你好");

    // list_messages 同样走 'me' 映射
    let messages = list_messages_inner(&kernel, PERSONAL, &conv.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender_id, "me");
    assert_eq!(messages[0].status.as_deref(), Some("failed"));

    // 引用回复透传落库
    let quote: QuoteRef = serde_json::from_value(serde_json::json!({
        "messageId": "msg_001",
        "senderName": "我",
        "preview": "你好"
    }))
    .unwrap();
    let view =
        send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_002", "引用回复", Some(quote), None)
            .unwrap();
    assert_eq!(view.quote.as_ref().unwrap().message_id, "msg_001");

    // 链接预览透传落库（抓取在壳层，见 link_preview.rs）
    let link: spark_core::message::LinkPreview = serde_json::from_value(serde_json::json!({
        "url": "https://github.com/spark",
        "title": "Spark",
        "description": "",
        "siteName": "GitHub",
        "domain": "github.com"
    }))
    .unwrap();
    let view = send_text_inner(
        &mut kernel,
        PERSONAL,
        &conv.id,
        "msg_004",
        "看 https://github.com/spark",
        None,
        Some(link.clone()),
    )
    .unwrap();
    assert_eq!(view.link, Some(link));

    // resend：failed 可重发，p2p 未启动仍 failed
    let view = resend_inner(&mut kernel, PERSONAL, &conv.id, "msg_001").unwrap();
    assert_eq!(view.status.as_deref(), Some("failed"));
    // 非 failed/sending 状态不可重发
    seed_message(&mut kernel, &conv.id, "msg_003", system_now_ms(), Some("delivered"));
    assert_eq!(
        resend_inner(&mut kernel, PERSONAL, &conv.id, "msg_003").unwrap_err(),
        "仅失败或发送中的消息可以重发"
    );
}

#[test]
fn recall_window_semantics() {
    let (_dir, mut kernel) = unlocked_kernel();
    let peer = peer_id();
    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob").unwrap();

    // 窗口内：撤回成功，视图 recalled=true
    let view = send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_001", "撤回我", None, None).unwrap();
    assert!(!view.recalled);
    assert!(recall_inner(&mut kernel, PERSONAL, &conv.id, "msg_001")
        .unwrap()
        .success);
    let messages = list_messages_inner(&kernel, PERSONAL, &conv.id).unwrap();
    assert!(messages[0].recalled);
    // 重复撤回 → false
    assert!(!recall_inner(&mut kernel, PERSONAL, &conv.id, "msg_001")
        .unwrap()
        .success);

    // 2 分钟窗口外 → false（内核语义：不报错）
    let old_ts = system_now_ms() - 3 * 60_000;
    seed_message(&mut kernel, &conv.id, "msg_old", old_ts, Some("failed"));
    assert!(!recall_inner(&mut kernel, PERSONAL, &conv.id, "msg_old")
        .unwrap()
        .success);

    // 不存在的消息 → false
    assert!(!recall_inner(&mut kernel, PERSONAL, &conv.id, "msg_nope")
        .unwrap()
        .success);
}

#[test]
fn mark_read_clear_delete_conversation_flow() {
    let (_dir, mut kernel) = unlocked_kernel();
    let peer = peer_id();
    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob").unwrap();
    send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_001", "hi", None, None).unwrap();

    // 手动置未读 → mark_read 清零
    let mut storage = kernel.__test_storage().unwrap();
    let mut record =
        MessageService::get_conversation(&storage, PERSONAL, &conv.id).unwrap().unwrap();
    record.unread_count = 3;
    MessageService::upsert_conversation(&mut storage, PERSONAL, &record).unwrap();
    drop(storage);
    assert_eq!(
        list_conversations_inner(&kernel, PERSONAL).unwrap()[0].unread_count,
        3
    );
    assert!(mark_read_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert_eq!(
        list_conversations_inner(&kernel, PERSONAL).unwrap()[0].unread_count,
        0
    );

    // 删除单条消息
    assert!(delete_inner(&mut kernel, PERSONAL, &conv.id, "msg_001")
        .unwrap()
        .success);
    assert!(list_messages_inner(&kernel, PERSONAL, &conv.id).unwrap().is_empty());

    // clear：清空消息保留会话入口
    send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_002", "again", None, None).unwrap();
    assert!(clear_inner(&mut kernel, PERSONAL, &conv.id).unwrap().success);
    assert!(list_messages_inner(&kernel, PERSONAL, &conv.id).unwrap().is_empty());
    assert_eq!(list_conversations_inner(&kernel, PERSONAL).unwrap().len(), 1);

    // delete_conversation：会话与消息一并删除
    assert!(delete_conversation_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap().is_empty());
}

#[test]
fn draft_pin_mute_roundtrip() {
    let (_dir, mut kernel) = unlocked_kernel();
    let peer = peer_id();
    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &peer, "Bob").unwrap();

    // 草稿
    assert!(set_draft_inner(&mut kernel, PERSONAL, &conv.id, "草稿内容")
        .unwrap()
        .success);
    assert_eq!(
        list_conversations_inner(&kernel, PERSONAL).unwrap()[0].draft,
        "草稿内容"
    );
    assert!(set_draft_inner(&mut kernel, PERSONAL, &conv.id, "")
        .unwrap()
        .success);
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap()[0]
        .draft
        .is_empty());

    // 置顶往返
    assert!(toggle_pin_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap()[0].pinned_at > 0);
    assert!(toggle_pin_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert_eq!(
        list_conversations_inner(&kernel, PERSONAL).unwrap()[0].pinned_at,
        0
    );

    // 免打扰往返
    assert!(toggle_mute_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap()[0].muted);
    assert!(toggle_mute_inner(&mut kernel, PERSONAL, &conv.id)
        .unwrap()
        .success);
    assert!(!list_conversations_inner(&kernel, PERSONAL).unwrap()[0].muted);
}

#[test]
fn send_text_to_self_delivered_and_online() {
    let (_dir, mut kernel) = unlocked_kernel();
    let my_root = kernel.current_root_id().unwrap().unwrap();

    // 自己的会话：online 恒 true（p2p 未启动也一样）
    let conv = ensure_direct_inner(&mut kernel, PERSONAL, &my_root, "我").unwrap();
    assert_eq!(conv.id, format!("dm:{my_root}"));
    assert!(conv.online);

    // 无 p2p、无配对设备：本机副本天然送达，status 直接 delivered
    let view =
        send_text_inner(&mut kernel, PERSONAL, &conv.id, "msg_self_1", "同步到各设备", None, None)
            .unwrap();
    assert_eq!(view.sender_id, "me");
    assert_eq!(view.status.as_deref(), Some("delivered"));

    let messages = list_messages_inner(&kernel, PERSONAL, &conv.id).unwrap();
    assert_eq!(messages[0].status.as_deref(), Some("delivered"));

    let convs = list_conversations_inner(&kernel, PERSONAL).unwrap();
    assert!(convs[0].online);
}

// ------------------------------------------------------------------
// 应用消息（服务号模型，p2p-messages.md §20）
// ------------------------------------------------------------------

fn app_payload(summary: &str) -> serde_json::Value {
    serde_json::json!({ "summary": summary, "kind": "notice" })
}

#[test]
fn app_send_list_mark_read_delete_flow() {
    let (_dir, mut kernel) = unlocked_kernel();

    // appSend：惰性建会话（app:{pluginId}，kind=app），状态恒 local，未读 +1
    let card: spark_core::message::AppMessageCard = serde_json::from_value(serde_json::json!({
        "viewId": "notice-card",
        "data": { "level": "info" }
    }))
    .unwrap();
    let view = app_send_inner(
        &mut kernel,
        PERSONAL,
        "weibo-core",
        app_payload("系统升级通知"),
        Some(card),
    )
    .unwrap();
    assert_eq!(view.plugin_id, "weibo-core");
    assert_eq!(view.summary, "系统升级通知");
    assert_eq!(view.status, "local");
    assert!(view.card.is_some());

    let convs = list_conversations_inner(&kernel, PERSONAL).unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].id, "app:weibo-core");
    assert_eq!(convs[0].kind, ConversationKind::App);
    assert_eq!(convs[0].unread_count, 1);

    // appList / appMarkRead
    app_send_inner(&mut kernel, PERSONAL, "weibo-core", app_payload("第二条"), None).unwrap();
    assert_eq!(app_list_inner(&kernel, PERSONAL, "weibo-core").unwrap().len(), 2);
    assert!(app_mark_read_inner(&mut kernel, PERSONAL, "weibo-core")
        .unwrap()
        .success);
    assert_eq!(
        list_conversations_inner(&kernel, PERSONAL).unwrap()[0].unread_count,
        0
    );
    assert!(app_list_inner(&kernel, PERSONAL, "weibo-core")
        .unwrap()
        .iter()
        .all(|m| m.read));

    // 删除应用会话：会话与消息一并删除
    assert!(app_delete_conversation_inner(&mut kernel, PERSONAL, "weibo-core")
        .unwrap()
        .success);
    assert!(app_list_inner(&kernel, PERSONAL, "weibo-core").unwrap().is_empty());
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap().is_empty());
}

#[test]
fn app_send_validation_and_rate_limit() {
    let (_dir, mut kernel) = unlocked_kernel();

    // summary 缺失 / 空白 / 超长 → 拒绝且不建会话
    for bad in [
        serde_json::json!({ "kind": "notice" }),
        serde_json::json!({ "summary": "   " }),
        app_payload(&"长".repeat(201)),
    ] {
        assert!(app_send_inner(&mut kernel, PERSONAL, "weibo-core", bad, None).is_err());
    }
    // pluginId 非法（含冒号/大写）→ 拒绝
    assert!(
        app_send_inner(&mut kernel, PERSONAL, "plugin:weibo-core", app_payload("s"), None)
            .is_err()
    );
    assert!(list_conversations_inner(&kernel, PERSONAL).unwrap().is_empty());

    // 限流：10 条/分钟，第 11 条 rate-limited（不落库、未读不变）
    for i in 0..10 {
        app_send_inner(&mut kernel, PERSONAL, "weibo-core", app_payload(&format!("第{i}条")), None)
            .unwrap();
    }
    let err = app_send_inner(&mut kernel, PERSONAL, "weibo-core", app_payload("超限"), None)
        .unwrap_err();
    assert!(err.contains("rate-limited"), "限流错误应含 reason：{err}");
    assert_eq!(app_list_inner(&kernel, PERSONAL, "weibo-core").unwrap().len(), 10);
}
