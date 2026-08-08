//! 插件后台运行时（QuickJS 沙箱）集成测试：
//! - 本机发送路径：bot 会话消息 → JS 回调 → `message.reply` 落库 +
//!   ChatReceived 广播（与真人入站同口径）；
//! - 路由防循环：bot 自己的回复不回投 JS（echo 插件消息数恒为 2）；
//! - 广播路由路径：ChatReceived 广播（多设备回同步 echo 的事件形态）经
//!   路由任务到达插件；
//! - 归属校验：插件 reply 到非自有会话被拒，JS 异常致线程崩溃隔离退出；
//! - 启停语义：停机后不再处理消息，重复启动报 AlreadyRunning。

mod common;

use common::*;
use spark_core::kernel::Kernel;
use spark_core::message::generate_message_id;
use spark_core::p2p::P2pEvent;
use spark_core::p2p::node::system_now_ms;

const PERSONAL: &str = "personal";
const ECHO_BOT: &str = "bot:echo-plugin:helper";

/// echo 插件：收到消息原样回显。
const ECHO_SCRIPT: &str = r#"
spark.onMessage(function (payload) {
    spark.reply(payload, 'echo: ' + payload.message.content);
});
"#;

fn kernel_with_identity() -> (tempfile::TempDir, Kernel, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _mnemonic) = init_identity(&mut kernel);
    (dir, kernel, root_id)
}

/// 注册 bot 联系人并建会话，返回会话 id。
fn setup_bot_conv(kernel: &mut Kernel) -> String {
    kernel.contact_ensure_bot(ECHO_BOT, "Echo Bot").unwrap();
    kernel
        .message_ensure_direct(PERSONAL, ECHO_BOT, "Echo Bot")
        .unwrap()
        .id
}

fn send_text(kernel: &mut Kernel, conv_id: &str, text: &str) {
    let message_id = generate_message_id(system_now_ms());
    kernel
        .message_send_text(PERSONAL, conv_id, &message_id, text, None, None)
        .unwrap();
}

/// bot 发出的消息内容列表（时间升序）。
fn bot_replies(kernel: &Kernel, conv_id: &str) -> Vec<String> {
    kernel
        .message_list_messages(PERSONAL, conv_id)
        .unwrap()
        .into_iter()
        .filter(|m| m.sender_id == ECHO_BOT)
        .map(|m| m.content)
        .collect()
}

#[test]
fn bot_message_reaches_runtime_and_reply_persisted() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let conv_id = setup_bot_conv(&mut kernel);
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT).unwrap();

    send_text(&mut kernel, &conv_id, "hello");

    wait_until(
        || bot_replies(&kernel, &conv_id) == vec!["echo: hello".to_string()],
        5_000,
        "bot 回复落库",
    );
    // 防循环：bot 回复不回投 JS，会话消息恒为用户一条 + 回复一条
    let total = kernel.message_list_messages(PERSONAL, &conv_id).unwrap().len();
    assert_eq!(total, 2, "echo 回复不得再触发 JS（防循环）");
    assert!(kernel.plugin_background_running("echo-plugin"));
}

#[test]
fn chat_received_broadcast_routed_to_runtime() {
    // 多设备回同步 echo 的事件形态：ChatReceived 广播（host 外发入站事件的
    // 同款）→ 路由任务 → 插件。这里直接经测试口发广播模拟。
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let conv_id = setup_bot_conv(&mut kernel);
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT).unwrap();

    let conversations = kernel.message_list_conversations(PERSONAL).unwrap();
    let conv = conversations.iter().find(|c| c.id == conv_id).unwrap();
    let event_tx = kernel.__test_event_tx();
    event_tx
        .send(P2pEvent::ChatReceived(serde_json::json!({
            "spaceKey": PERSONAL,
            "conversation": conv,
            "message": { "id": "m-echo-1", "senderId": "me", "content": "from-other-device" }
        })))
        .unwrap();

    wait_until(
        || bot_replies(&kernel, &conv_id) == vec!["echo: from-other-device".to_string()],
        5_000,
        "广播路由触发的 bot 回复落库",
    );
}

#[test]
fn stop_halts_processing_and_restart_allowed() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let conv_id = setup_bot_conv(&mut kernel);

    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT).unwrap();
    // 重复启动被拒
    let duplicated = kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT);
    assert!(duplicated.is_err(), "重复启动应报 AlreadyRunning");

    kernel.plugin_stop_background("echo-plugin").unwrap();
    assert!(!kernel.plugin_background_running("echo-plugin"));

    send_text(&mut kernel, &conv_id, "while-stopped");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(bot_replies(&kernel, &conv_id).is_empty(), "停机后不得处理消息");

    // 停机后可重新启动并恢复处理
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT).unwrap();
    send_text(&mut kernel, &conv_id, "after-restart");
    wait_until(
        || bot_replies(&kernel, &conv_id) == vec!["echo: after-restart".to_string()],
        5_000,
        "重启后恢复处理",
    );
}

#[test]
fn plugin_registers_own_bot_via_capability() {
    // contact.ensureBot 能力：插件脚本自行注册 bot 联系人（rootId 由内核
    // 拼定为 bot:{pluginId}:{botId}），随后消息收发全链路可用
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.ensureBot('helper', '自助 Bot');
spark.onMessage(function (payload) { spark.reply(payload, 'pong'); });
"#;
    kernel.plugin_start_background("echo-plugin", script).unwrap();

    wait_until(
        || {
            kernel
                .contact_overview(PERSONAL)
                .unwrap()
                .friends
                .iter()
                .any(|f| f.root_id == ECHO_BOT && f.nickname == "自助 Bot")
        },
        5_000,
        "bot 联系人经 ensureBot 能力注册",
    );
    let conv_id = kernel.message_ensure_direct(PERSONAL, ECHO_BOT, "自助 Bot").unwrap().id;
    send_text(&mut kernel, &conv_id, "ping");
    wait_until(
        || bot_replies(&kernel, &conv_id) == vec!["pong".to_string()],
        5_000,
        "自注册 bot 的回复落库",
    );
}

#[test]
fn reply_to_foreign_conversation_rejected() {
    // 归属校验：插件 reply 到非自有会话（伪造 convId）被拒；JS 异常导致
    // 本插件线程崩溃退出（隔离），目标会话无消息落库。
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let conv_id = setup_bot_conv(&mut kernel);
    let foreign = kernel
        .message_ensure_direct(PERSONAL, "bot:other-plugin:x", "Other Bot")
        .unwrap()
        .id;
    let foreign_id = foreign.clone();
    let script = format!(
        r#"
spark.onMessage(function (payload) {{
    spark.reply({{ spaceKey: payload.spaceKey, conversation: {{ id: '{foreign_id}' }} }}, 'intrude');
}});
"#
    );
    kernel.plugin_start_background("echo-plugin", &script).unwrap();

    send_text(&mut kernel, &conv_id, "hi");
    std::thread::sleep(std::time::Duration::from_millis(500));

    assert!(
        kernel
            .message_list_messages(PERSONAL, &foreign)
            .unwrap()
            .is_empty(),
        "非自有会话不得被插件写入"
    );
    wait_until(
        || !kernel.plugin_background_running("echo-plugin"),
        5_000,
        "JS 异常后插件线程崩溃退出并注销",
    );
}
