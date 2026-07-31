//! kernel 应用消息门面与 message::app 服务测试（p2p-messages.md §20）：
//! - 门面：appSend 惰性建会话（`app:{pluginId}` id 约定、kind=app）、未读聚合、
//!   状态恒 local、appList/appMarkRead/appDeleteConversation、水合（重启后仍在）；
//! - 校验：summary 缺失/空白/超长、pluginId 字符集、校验先于限流（非法消息
//!   不消耗配额）；
//! - 限流：每插件每会话 10 条/分钟，超限拒绝并计数、窗口过期重置、插件间隔离；
//!   内置 system 会话（壳层系统通知）豁免限流。

mod common;

use serde_json::json;
use spark_core::kernel::app_conversation_id;
use spark_core::message::{
    APP_MSG_RATE_LIMIT, AppMessageRateLimiter, AppMessageService, ConversationKind, MessageError,
    MessageService,
};
use spark_core::storage::MemoryStorage;

use common::*;

const PERSONAL: &str = "personal";
const NOW: i64 = 1_720_000_000_000;

fn payload(summary: &str) -> serde_json::Value {
    json!({ "summary": summary, "kind": "notice", "detail": "正文" })
}

// ---------------------------------------------------------------------------
// 服务层（MemoryStorage 纯逻辑）
// ---------------------------------------------------------------------------

#[test]
fn build_app_message_validation() {
    // 正常：summary 提取并 trim 提升
    let record = AppMessageService::build_app_message(
        "spark-example",
        payload("  摘要  "),
        None,
        "m1".to_string(),
        NOW,
    )
    .unwrap();
    assert_eq!(record.summary, "摘要");
    assert_eq!(record.status, "local");
    assert!(!record.read);

    // summary 缺失 / 非字符串 / 空白 → MissingSummary
    for bad in [
        json!({ "kind": "notice" }),
        json!({ "summary": 42 }),
        json!({ "summary": "   " }),
    ] {
        assert!(matches!(
            AppMessageService::build_app_message("spark-example", bad, None, "m1".into(), NOW),
            Err(MessageError::MissingSummary)
        ));
    }

    // summary 超 200 字符 → SummaryTooLong
    let long = "长".repeat(201);
    assert!(matches!(
        AppMessageService::build_app_message("spark-example", payload(&long), None, "m1".into(), NOW),
        Err(MessageError::SummaryTooLong)
    ));
    // 恰好 200 字符放行
    let exact = "好".repeat(200);
    assert!(
        AppMessageService::build_app_message("spark-example", payload(&exact), None, "m1".into(), NOW)
            .is_ok()
    );

    // pluginId 字符集：大写 / 含冒号 / 空串 / 超长 → InvalidPluginId
    for bad in ["SparkExample", "plugin:spark-example", "", "a".repeat(65).as_str()] {
        assert!(matches!(
            AppMessageService::build_app_message(bad, payload("s"), None, "m1".into(), NOW),
            Err(MessageError::InvalidPluginId)
        ));
    }
}

#[test]
fn service_append_list_mark_read_delete() {
    let mut storage = MemoryStorage::default();

    // 未建会话直接 append → ConversationNotFound
    let record = AppMessageService::build_app_message(
        "spark-example",
        payload("第一条"),
        None,
        "m1".to_string(),
        NOW,
    )
    .unwrap();
    assert!(matches!(
        AppMessageService::append_app_message(&mut storage, PERSONAL, &record),
        Err(MessageError::ConversationNotFound)
    ));

    // ensure 幂等 + id 约定 + kind=app
    let conv = AppMessageService::ensure_app_conversation(&mut storage, PERSONAL, "spark-example", NOW).unwrap();
    assert_eq!(conv.id, app_conversation_id("spark-example"));
    assert_eq!(conv.id, "app:spark-example");
    assert_eq!(conv.kind, ConversationKind::App);
    let again =
        AppMessageService::ensure_app_conversation(&mut storage, PERSONAL, "spark-example", NOW + 1).unwrap();
    assert_eq!(again.id, conv.id);

    // 写入两条：未读 +2，updatedAt 前进；乱序写入不回退 updatedAt
    AppMessageService::append_app_message(&mut storage, PERSONAL, &record).unwrap();
    let second = AppMessageService::build_app_message(
        "spark-example",
        payload("第二条"),
        None,
        "m2".to_string(),
        NOW + 1000,
    )
    .unwrap();
    AppMessageService::append_app_message(&mut storage, PERSONAL, &second).unwrap();
    let conv = MessageService::get_conversation(&storage, PERSONAL, &conv.id)
        .unwrap()
        .unwrap();
    assert_eq!(conv.unread_count, 2);
    assert_eq!(conv.updated_at, NOW + 1000);

    // 列表：时间升序
    let messages = AppMessageService::list_app_messages(&storage, PERSONAL, "spark-example").unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].id, "m1");
    assert_eq!(messages[1].summary, "第二条");

    // 空间隔离：其他空间看不到
    assert!(
        AppMessageService::list_app_messages(&storage, "org:org_0123456789abcdef", "spark-example")
            .unwrap()
            .is_empty()
    );

    // 标记已读：未读清零 + 消息 read 批量置位（幂等）
    AppMessageService::mark_app_read(&mut storage, PERSONAL, "spark-example").unwrap();
    let conv = MessageService::get_conversation(&storage, PERSONAL, &conv.id)
        .unwrap()
        .unwrap();
    assert_eq!(conv.unread_count, 0);
    assert!(
        AppMessageService::list_app_messages(&storage, PERSONAL, "spark-example")
            .unwrap()
            .iter()
            .all(|m| m.read)
    );
    AppMessageService::mark_app_read(&mut storage, PERSONAL, "spark-example").unwrap();

    // 删除会话：消息与会话一并删除
    AppMessageService::delete_app_conversation(&mut storage, PERSONAL, "spark-example").unwrap();
    assert!(
        AppMessageService::list_app_messages(&storage, PERSONAL, "spark-example")
            .unwrap()
            .is_empty()
    );
    assert!(
        MessageService::get_conversation(&storage, PERSONAL, &conv.id)
            .unwrap()
            .is_none()
    );
    // 幂等：再删不报错
    AppMessageService::delete_app_conversation(&mut storage, PERSONAL, "spark-example").unwrap();
}

#[test]
fn rate_limiter_window_and_isolation() {
    let mut limiter = AppMessageRateLimiter::default();

    // 窗口内 10 条放行，第 11 条拒绝并计数
    for _ in 0..APP_MSG_RATE_LIMIT {
        assert!(limiter.check(PERSONAL, "spark-example", NOW));
    }
    assert!(!limiter.check(PERSONAL, "spark-example", NOW + 1));
    assert!(!limiter.check(PERSONAL, "spark-example", NOW + 2));
    assert_eq!(limiter.rejected_count(PERSONAL, "spark-example"), 2);

    // 其他插件 / 其他空间互不影响
    assert!(limiter.check(PERSONAL, "other-app", NOW + 3));
    assert!(limiter.check("org:org_0123456789abcdef", "spark-example", NOW + 3));

    // 窗口过期重置（拒绝计数保留）
    assert!(limiter.check(PERSONAL, "spark-example", NOW + 60_000));
    assert_eq!(limiter.rejected_count(PERSONAL, "spark-example"), 2);
}

// ---------------------------------------------------------------------------
// kernel 门面
// ---------------------------------------------------------------------------

#[test]
fn app_send_list_mark_read_delete_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();

    // appSend：惰性建会话、状态 local、未读 +1；卡片透传
    let card: spark_core::message::AppMessageCard =
        serde_json::from_value(json!({ "viewId": "notice-card", "data": { "level": "info" } }))
            .unwrap();
    let view = kernel
        .message_app_send(PERSONAL, "spark-example", payload("欢迎"), Some(card.clone()))
        .unwrap();
    assert_eq!(view.plugin_id, "spark-example");
    assert_eq!(view.summary, "欢迎");
    assert_eq!(view.status, "local");
    assert_eq!(view.card, Some(card));
    assert!(!view.read);

    // 会话出现在会话列表：id 约定 + kind=app + 未读聚合
    let convs = kernel.message_list_conversations(PERSONAL).unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].id, "app:spark-example");
    assert_eq!(convs[0].kind, ConversationKind::App);
    assert_eq!(convs[0].unread_count, 1);

    // appList 与 appMarkRead
    kernel
        .message_app_send(PERSONAL, "spark-example", payload("第二条"), None)
        .unwrap();
    let messages = kernel.message_app_list(PERSONAL, "spark-example").unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|m| m.status == "local"));
    kernel.message_app_mark_read(PERSONAL, "spark-example").unwrap();
    assert_eq!(
        kernel.message_list_conversations(PERSONAL).unwrap()[0].unread_count,
        0
    );
    assert!(
        kernel
            .message_app_list(PERSONAL, "spark-example")
            .unwrap()
            .iter()
            .all(|m| m.read)
    );

    // 校验失败不落库、不计未读
    assert!(
        kernel
            .message_app_send(PERSONAL, "spark-example", json!({ "kind": "notice" }), None)
            .is_err()
    );
    assert_eq!(kernel.message_app_list(PERSONAL, "spark-example").unwrap().len(), 2);

    // 删除会话
    kernel
        .message_app_delete_conversation(PERSONAL, "spark-example")
        .unwrap();
    assert!(kernel.message_app_list(PERSONAL, "spark-example").unwrap().is_empty());
    assert!(kernel.message_list_conversations(PERSONAL).unwrap().is_empty());
    kernel.shutdown().unwrap();
}

#[test]
fn app_send_rate_limited_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();

    // 校验先于限流：先发一条非法消息（不消耗配额），再连续 10 条合法消息
    assert!(
        kernel
            .message_app_send(PERSONAL, "spark-example", json!({}), None)
            .is_err()
    );
    for i in 0..APP_MSG_RATE_LIMIT {
        kernel
            .message_app_send(PERSONAL, "spark-example", payload(&format!("第{i}条")), None)
            .unwrap();
    }
    // 第 11 条：rate-limited，不落库、未读不变、拒绝计数 +1
    let err = kernel
        .message_app_send(PERSONAL, "spark-example", payload("超限"), None)
        .unwrap_err();
    assert!(
        err.to_string().contains("rate-limited"),
        "限流错误应含 reason：{err}"
    );
    assert_eq!(kernel.message_app_rate_rejected(PERSONAL, "spark-example"), 1);
    assert_eq!(
        kernel.message_app_list(PERSONAL, "spark-example").unwrap().len() as u32,
        APP_MSG_RATE_LIMIT
    );
    assert_eq!(
        kernel.message_list_conversations(PERSONAL).unwrap()[0].unread_count,
        APP_MSG_RATE_LIMIT
    );
    kernel.shutdown().unwrap();
}

#[test]
fn app_messages_hydrate_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();
    kernel
        .message_app_send(PERSONAL, "spark-example", payload("重启前的消息"), None)
        .unwrap();
    kernel.shutdown().unwrap();

    // 重开：sled 持久化水合，消息与会话未读仍在
    let mut kernel = fresh_kernel(dir.path());
    kernel.stop_p2p().unwrap();
    kernel.unlock(PASSWORD, None).unwrap();
    let messages = kernel.message_app_list(PERSONAL, "spark-example").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].summary, "重启前的消息");
    let convs = kernel.message_list_conversations(PERSONAL).unwrap();
    assert_eq!(convs[0].id, "app:spark-example");
    assert_eq!(convs[0].unread_count, 1);
    kernel.shutdown().unwrap();
}

#[test]
fn app_send_system_exempt_from_rate_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel.stop_p2p().unwrap();

    // 内置 system 会话（壳层系统通知写入方）豁免限流：超过 10 条/60s 仍放行、
    // 不累计拒绝计数；普通插件同窗口仍被限流
    for i in 0..APP_MSG_RATE_LIMIT + 2 {
        kernel
            .message_app_send(PERSONAL, "system", payload(&format!("系统通知{i}")), None)
            .unwrap();
    }
    assert_eq!(
        kernel.message_app_list(PERSONAL, "system").unwrap().len() as u32,
        APP_MSG_RATE_LIMIT + 2
    );
    assert_eq!(kernel.message_app_rate_rejected(PERSONAL, "system"), 0);
    kernel.shutdown().unwrap();
}
