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
use spark_core::plugindata::DeclareInput;
use spark_core::storage::StorageBackend;

const PERSONAL: &str = "personal";
const ECHO_BOT: &str = "bot:echo-plugin:helper";

/// echo 插件：收到消息原样回显。
const ECHO_SCRIPT: &str = r#"
spark.onMessage(function (payload) {
    spark.reply(payload, 'echo: ' + payload.message.content);
});
"#;

/// 测试插件的授权清单（对齐市场 grantedPermissions 形态：基础权限恒在列 +
/// 声明的高级权限；capability 分发按此清单逐调用强制）。
fn test_permissions() -> Vec<String> {
    [
        "storage:read",
        "storage:write",
        "identity:verify",
        "identity:sign",
        "message:app",
        "system:exec",
        "network:fetch",
        "feed:deliver",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 以插件域声明一个 personal 集合（测试侧声明，供 JS `data.save` 与测试侧
/// `data_get` 共用同一解析）。
fn declare_plugin_collection(kernel: &mut Kernel, plugin_id: &str, name: &str) {
    kernel
        .data_declare_collection(
            &format!("plugin:{plugin_id}"),
            DeclareInput {
                name: name.to_string(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
}

/// 写入一个朋友（feed 通道放行：permission "open"，带可寻址 peer 供投递）+
/// 对端 root 公钥（出站 E2E 读取，social-feed §4.1）。
fn seed_feed_recipient(kernel: &mut Kernel, root_id: &str) {
    use spark_core::contact::{ContactService, FriendRecord, PeerRef};
    let friend = FriendRecord {
        root_id: root_id.to_string(),
        permission: "open".to_string(),
        peers: vec![PeerRef {
            peer_id: "peer-1".to_string(),
            addresses: vec![],
        ..Default::default()}],
        ..Default::default()
    };
    let mut storage = kernel.__test_storage().unwrap();
    ContactService::upsert_friend(&mut storage, &friend).unwrap();
    // 出站 E2E 需对端 root 公钥（正常路径入站验签积累，测试直写）
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use spark_core::dm_e2e::record_inbound_peer_root_pub;
    use spark_core::p2p::node::system_now_ms;
    let key = SigningKey::from_bytes(&[7; 32]);
    let pub_b64 = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
    record_inbound_peer_root_pub(&mut storage, root_id, &pub_b64, "local-node", system_now_ms()).unwrap();
}

fn kernel_with_identity() -> (tempfile::TempDir, Kernel, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, _mnemonic) = init_identity(&mut kernel);
    (dir, kernel, root_id)
}

/// 注册 bot 联系人并建会话，返回会话 id。
fn setup_bot_conv(kernel: &mut Kernel) -> String {
    setup_bot_conv_named(kernel, ECHO_BOT, "Echo Bot")
}

fn setup_bot_conv_named(kernel: &mut Kernel, bot_root_id: &str, name: &str) -> String {
    kernel.contact_ensure_bot(bot_root_id, name).unwrap();
    kernel
        .message_ensure_direct(PERSONAL, bot_root_id, name)
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
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT, &test_permissions()).unwrap();

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
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT, &test_permissions()).unwrap();

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

    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT, &test_permissions()).unwrap();
    // 重复启动被拒
    let duplicated = kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT, &test_permissions());
    assert!(duplicated.is_err(), "重复启动应报 AlreadyRunning");

    kernel.plugin_stop_background("echo-plugin").unwrap();
    assert!(!kernel.plugin_background_running("echo-plugin"));

    send_text(&mut kernel, &conv_id, "while-stopped");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(bot_replies(&kernel, &conv_id).is_empty(), "停机后不得处理消息");

    // 停机后可重新启动并恢复处理
    kernel.plugin_start_background("echo-plugin", ECHO_SCRIPT, &test_permissions()).unwrap();
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
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();

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
fn docs_capability_roundtrip() {
    // docs 能力：JS 侧 put/get/query 全链路（域恒为插件 id）
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
try { spark.docs.defineCollection('notes', { syncStrategy: 'lww', enableEvidence: false }); } catch (e) {}
spark.docs.put('notes', 'n1', { title: 'hello', kind: 'post' }, { syncStrategy: 'lww', enableEvidence: false });
var got = spark.docs.get('notes', 'n1');
if (!got || got.title !== 'hello') throw new Error('docs.get mismatch: ' + JSON.stringify(got));
var result = spark.docs.query('notes', { filter: [{ field: 'kind', value: 'post' }] }, { syncStrategy: 'lww', enableEvidence: false });
if (result.items.length !== 1) throw new Error('docs.query mismatch');
spark.ensureBot('docs-bot', 'Docs Bot');
spark.onMessage(function () {});
"#;
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();
    // 脚本加载即执行 docs 写入（JS 线程异步，轮询等待）
    wait_until(
        || {
            kernel
                .doc_get("echo-plugin", "notes", "n1")
                .unwrap()
                .is_some_and(|doc| doc["title"] == "hello")
        },
        5_000,
        "docs 能力写入落库",
    );
}

#[test]
fn docs_domain_whitelist() {
    // 域约束：缺省/自身域/plugin: 根域可读写；空间根域仅限白名单遗留集合
    // 的只读（写一律拒，防伪造共享空间数据）；其他插件域、组织域拒绝
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
function tryQuery(domain, collection) {
    try { spark.docs.query(collection || 'c', {}, null, domain); return 'ok'; }
    catch (e) { return 'err'; }
}
function tryPut(domain, collection) {
    try { spark.docs.put(collection || 'c', 'x', { v: 1 }, null, domain); return 'ok'; }
    catch (e) { return 'err'; }
}
var report = [
    tryQuery(null),                          // 缺省 → 插件自身域
    tryQuery('echo-plugin'),                 // 自身域
    tryQuery('plugin:echo-plugin'),          // plugin: 根域（UI 桥历史数据面）
    tryQuery('space:personal', 'ai_chat_bots'), // 空间域遗留集合 → 只读放行
    tryQuery('space:org', 'ai_chat_bots'),
    tryQuery('space:personal'),              // 空间域非白名单集合 → 拒
    tryPut('space:personal', 'ai_chat_bots'),   // 空间域写（白名单集合也拒）
    tryPut('space:org'),                     // 空间域写 → 拒
    tryPut(null),                            // 自身域写 → 放行
    tryQuery('other-plugin'),                // 他插件域 → 拒
    tryQuery('plugin:other-plugin'),         // 他插件根域 → 拒
    tryQuery('org:some-org')                 // 组织域 → 拒
].join(',');
spark.ensureBot('d-bot', 'D Bot');
spark.onMessage(function (payload) { spark.reply(payload, report); });
"#;
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:d-bot", "D Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| {
                    m.sender_id == "bot:echo-plugin:d-bot"
                        && m.content == "ok,ok,ok,ok,ok,err,err,err,ok,err,err,err"
                })
        },
        5_000,
        "域白名单判定回传",
    );
}

#[test]
fn capability_permission_enforced_and_denial_rejects_promise() {
    // 权限强制：未声明 system:exec 的插件调 sys.exec → Promise 拒绝
    // （错误经 startAsync 回流），插件线程存活；免权限调用（log）不受影响
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.ensureBot('perm-bot', 'Perm Bot');
spark.onMessage(function (payload) {
    spark.sys.exec('definitely-not-a-real-program', []).then(function () {
        spark.reply(payload, 'unexpected-ok');
    }, function (err) {
        spark.reply(payload, 'denied: ' + err.message);
    });
});
"#;
    // 仅授予 message:app：sys.exec（system:exec）应被拒
    let permissions = vec!["message:app".to_string()];
    kernel
        .plugin_start_background("echo-plugin", script, &permissions)
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:perm-bot", "Perm Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| {
                    m.sender_id == "bot:echo-plugin:perm-bot"
                        && m.content.contains("denied:")
                        && m.content.contains("system:exec")
                })
        },
        5_000,
        "未授权 sys.exec 的 Promise 拒绝回流",
    );
    // 拒绝是错误回流而非线程崩溃：运行时仍存活
    assert!(kernel.plugin_background_running("echo-plugin"));
}

#[test]
fn docs_capability_requires_storage_permission() {
    // docs 写能力未授权（缺 storage:write）：同步抛错被脚本 catch 后回复；
    // 授权后（重启带全量清单）同一调用放行
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.ensureBot('docs-bot', 'Docs Bot');
spark.onMessage(function (payload) {
    try {
        spark.docs.put('notes', 'n1', { title: 'x' }, null);
        spark.reply(payload, 'put-ok');
    } catch (e) {
        spark.reply(payload, 'put-denied');
    }
});
"#;
    let permissions = vec!["message:app".to_string(), "storage:read".to_string()];
    kernel
        .plugin_start_background("echo-plugin", script, &permissions)
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:docs-bot", "Docs Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| m.sender_id == "bot:echo-plugin:docs-bot" && m.content == "put-denied")
        },
        5_000,
        "未授权 docs.put 被拒",
    );
    assert!(
        kernel.doc_get("echo-plugin", "notes", "n1").unwrap().is_none(),
        "被拒的写入不得落库"
    );
}

#[test]
fn host_query_roundtrip_and_unknown_plugin() {
    // 宿主 → 插件反向查询：JS 处理器应答经 query.respond 回流
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.onQuery('bot:query', function (payload) {
    return { exists: payload.contactId === 'bot:echo-plugin:helper' };
});
"#;
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();

    // JS 线程异步加载脚本，先等对 handler 注册完成（首次查询可能赶在加载前）
    wait_until(
        || {
            kernel
                .plugin_host_query("echo-plugin", "bot:query", serde_json::json!({"contactId": "bot:echo-plugin:helper"}))
                .is_some_and(|reply| reply["exists"] == true)
        },
        5_000,
        "宿主查询回流 exists=true",
    );
    let negative = kernel
        .plugin_host_query("echo-plugin", "bot:query", serde_json::json!({"contactId": "bot:echo-plugin:ghost"}))
        .unwrap();
    assert_eq!(negative["exists"], false);
    // 未运行的插件：立即 None（不等超时）
    assert!(
        kernel
            .plugin_host_query("no-such-plugin", "bot:query", serde_json::json!({}))
            .is_none()
    );
}

#[test]
fn host_query_sync_throw_returns_error_and_thread_survives() {
    // 查询处理器同步抛错：必须转为错误应答回流（`{error: ...}`），插件
    // 线程存活并继续处理后续查询/消息
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.onQuery('bot:query', function (payload) {
    if (payload.boom) throw new Error('sync-boom');
    return { ok: true };
});
"#;
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();

    // JS 线程异步加载脚本，先等对 handler 注册完成（首次查询可能赶在加载前）
    wait_until(
        || {
            kernel
                .plugin_host_query("echo-plugin", "bot:query", serde_json::json!({}))
                .is_some_and(|reply| reply["ok"] == true)
        },
        5_000,
        "查询处理器注册就绪",
    );
    let reply = kernel
        .plugin_host_query("echo-plugin", "bot:query", serde_json::json!({ "boom": true }))
        .expect("同步抛错应回流错误应答，而非超时无应答");
    assert!(
        reply["error"]
            .as_str()
            .is_some_and(|e| e.contains("sync-boom")),
        "错误应答应含抛出信息: {reply}"
    );
    // 线程存活：后续查询照常应答
    let after = kernel
        .plugin_host_query("echo-plugin", "bot:query", serde_json::json!({}))
        .expect("插件线程应存活并继续应答");
    assert_eq!(after["ok"], true);
    assert!(kernel.plugin_background_running("echo-plugin"));
}

#[test]
fn host_query_wait_does_not_hold_kernel_state() {
    // Issue 4 回归：宿主查询等待应答（上界 2s）期间不得持有 Mutex<Kernel>。
    // 壳层用法：锁内仅克隆查询句柄（Arc 共享），释锁后等待——等待期间其他
    // 内核操作必须能拿到锁。
    let (_dir, mut kernel, _root) = kernel_with_identity();
    // onQuery 返回永不兑现的 Promise：查询投递成功但等满 2s 超时
    let script = r#"
spark.onQuery('bot:query', function () { return new Promise(function () {}); });
"#;
    kernel.plugin_start_background("echo-plugin", script, &test_permissions()).unwrap();
    let kernel = std::sync::Arc::new(std::sync::Mutex::new(kernel));

    let waiter_kernel = kernel.clone();
    let waiter = std::thread::spawn(move || {
        let query = waiter_kernel.lock().unwrap().plugin_host_query_handle();
        // 句柄已脱离内核锁；查询无应答，等满 2s 超时
        query.query("echo-plugin", "bot:query", serde_json::json!({}))
    });

    // 给查询线程留出克隆句柄/投递并进入等待的时间；随后主线程拿锁做内核
    // 操作。若等待期间仍持锁（旧行为），此处将阻塞至查询超时结束（≈2s）。
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    assert!(
        kernel.lock().unwrap().plugin_background_running("echo-plugin"),
        "等待中的宿主查询不得挡住其他内核操作"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "内核锁被宿主查询等待占住: {:?}",
        started.elapsed()
    );
    assert!(
        waiter.join().unwrap().is_none(),
        "无应答的查询应超时返回 None"
    );
}

#[test]
fn sys_exec_async_roundtrip() {
    // sys.exec 异步能力：启动即返，结果经事件队列回流兑现 Promise。
    // 用平台必有的 shell 回显命令（跨平台分支选择）
    let (_dir, mut kernel, _root) = kernel_with_identity();
    #[cfg(target_os = "windows")]
    let (program, args): (&str, &[&str]) = ("cmd", &["/c", "echo", "async-ok"]);
    #[cfg(not(target_os = "windows"))]
    let (program, args): (&str, &[&str]) = ("sh", &["-c", "echo async-ok"]);
    let script = format!(
        r#"
spark.ensureBot('exec-bot', 'Exec Bot');
spark.onMessage(function (payload) {{
    spark.sys.exec('{program}', {args_json}).then(function (result) {{
        spark.reply(payload, 'exit=' + result.exitCode + ' out=' + result.stdout.trim());
    }});
}});
"#,
        args_json = serde_json::to_string(&args).unwrap()
    );
    kernel.plugin_start_background("echo-plugin", &script, &test_permissions()).unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:exec-bot", "Exec Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| m.sender_id == "bot:echo-plugin:exec-bot" && m.content.contains("async-ok"))
        },
        10_000,
        "sys.exec 异步结果回流并回复",
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
    kernel.plugin_start_background("echo-plugin", &script, &test_permissions()).unwrap();

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

/// S9：spark.feed.deliver QuickJS 后台全链路——脚本调 deliver 走 capability →
/// feed_deliver_shared → 投递（p2p 未启动 → 入 dm:pending 离线队列），返回
/// 聚合计数；topic 前缀非本插件被拒（InvalidTopic）；第 11 次调用触发
/// RateLimited 限流（§9.3，与 iframe 桥共享同一内核限流器）。
#[test]
fn feed_deliver_quickjs_full_chain_and_rate_limit() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let bob = "bb".repeat(32);
    seed_feed_recipient(&mut kernel, &bob);
    let script = format!(
        r#"
spark.ensureBot('feed-bot', 'Feed Bot');
var results = [];
// 前 10 次合法（topic 前缀 == 本插件 id），第 11 次应触发 RateLimited
for (var i = 0; i < 11; i++) {{
    try {{
        var r = spark.feed.deliver({{ topic: 'echo-plugin:posts', payload: {{ n: i }}, recipients: ['{bob}'] }});
        results.push('ok:' + r.requested + ':' + r.accepted);
    }} catch (e) {{
        results.push('err:' + e.message);
    }}
}}
// 前缀非本插件 id → InvalidTopic（能力层拒绝，同步抛错）
try {{
    spark.feed.deliver({{ topic: 'evil:posts', payload: {{}}, recipients: ['{bob}'] }});
    results.push('prefix-ok');
}} catch (e) {{
    results.push('prefix-err:' + e.message);
}}
spark.onMessage(function (payload) {{ spark.reply(payload, results.join(',')); }});
"#
    );
    kernel
        .plugin_start_background("echo-plugin", &script, &test_permissions())
        .unwrap();

    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:feed-bot", "Feed Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| m.sender_id == "bot:echo-plugin:feed-bot" && m.content.contains("err:RateLimited"))
        },
        5_000,
        "feed.deliver 全链路结果回传（含限流/前缀校验）",
    );
    let msgs = kernel.message_list_messages(PERSONAL, &conv_id).unwrap();
    let report = msgs
        .iter()
        .find(|m| m.sender_id == "bot:echo-plugin:feed-bot")
        .map(|m| m.content.clone())
        .unwrap();
    // 前 10 次 ok（requested/accepted 按入参），第 11 次 err:RateLimited，
    // 前缀非本插件 err:InvalidTopic
    let parts: Vec<&str> = report.split(',').collect();
    assert_eq!(parts.len(), 12, "10 次 deliver + 1 次超限 + 1 次前缀，报告：{report}");
    for i in 0..10 {
        assert_eq!(parts[i], "ok:1:1", "第 {i} 次 deliver 应放行并计数 requested=1 accepted=1");
    }
    assert!(
        parts[10].contains("RateLimited"),
        "第 11 次应 RateLimited：{}",
        parts[10]
    );
    assert!(
        parts[11].contains("InvalidTopic"),
        "前缀非本插件应 InvalidTopic：{}",
        parts[11]
    );

    // 投递全链路落地：p2p 未启动 → 出站 feed 失败入 dm:pending 离线队列（收件人
    // bob）。E2E 加密下信封 body 为密文，`enqueue_feed_pending` 不能从 body 取
    // feedId——修复后明文 feedId 由调用方显式传入，每条 deliver 各生成独立 feedId
    // （QuickJS 缺省时 `generate_feed_id_for_plugin`），按 message_id 键区分不再
    // 互相覆盖：前 10 次合法 deliver 各入 1 条，共精确 10 条（social-feed §6.4
    // 离线队列以 message_id 为键）。
    let storage = kernel.__test_storage().unwrap();
    let pending_prefix = format!("dm:pending:{bob}:");
    let pending: Vec<_> = storage
        .scan(&spark_core::storage::ScanOptions::prefix(&pending_prefix))
        .unwrap();
    assert_eq!(
        pending.len(),
        10,
        "10 次合法 deliver（每次独立 feedId）各入 1 条 pending，不得互相覆盖"
    );
}

/// S9：spark.feed.pull 接收侧免权限——插件脚本补读收件箱（本地落库后 pull
/// 返回 items）。
#[test]
fn feed_pull_quickjs_reads_inbox() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    // 预写一条收件箱记录（feed:inbox:{pluginId}:...）
    let rec = serde_json::json!({
        "from": "alice",
        "topic": "echo-plugin:posts",
        "feedId": "f1",
        "payload": { "text": "hello" },
        "ts": 1000,
    });
    {
        let mut storage = kernel.__test_storage().unwrap();
        storage
            .put("feed:inbox:echo-plugin:0000000001000:f1", &rec.to_string())
            .unwrap();
    }
    let script = r#"
spark.ensureBot('pull-bot', 'Pull Bot');
spark.onMessage(function (payload) {
    var out = spark.feed.pull({ topic: 'echo-plugin:posts', limit: 10 });
    var first = out.items && out.items.length ? out.items[0] : null;
    spark.reply(payload, 'count=' + (out.items ? out.items.length : 0) + ' feedId=' + (first ? first.feedId : 'none'));
});
"#;
    kernel
        .plugin_start_background("echo-plugin", script, &test_permissions())
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:pull-bot", "Pull Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| m.sender_id == "bot:echo-plugin:pull-bot" && m.content == "count=1 feedId=f1")
        },
        5_000,
        "spark.feed.pull 补读收件箱并回传",
    );
}

/// B3：feed-blob 请求方发起链路——`data.readBlob` 未命中且该 hash 有 feed 来源
/// 登记 → 进入 feed-blob-req 出站路径（p2p 未启动时 `blob:req:{hash}` 节流键
/// 被置，证明请求路径被触发）；无来源登记 → 维持 pdsync want（节流键不置）。
#[test]
fn read_blob_miss_triggers_feed_blob_req_when_source_registered() {
    use spark_core::contact::{ContactService, FriendRecord, PeerRef};
    let (_dir, mut kernel, _root) = kernel_with_identity();
    // 朋友 + 来源登记（feed:blob-src:{hash} → fromRootId，仿 feed 入站落库形态）
    let source = "src".repeat(16);
    {
        let mut storage = kernel.__test_storage().unwrap();
        ContactService::upsert_friend(
            &mut storage,
            &FriendRecord {
                root_id: source.clone(),
                permission: "open".to_string(),
                peers: vec![PeerRef {
                    peer_id: "peer-1".to_string(),
                    addresses: vec![],
                ..Default::default()}],
                ..Default::default()
            },
        )
        .unwrap();
    }
    // 有来源登记的 hash 与无来源登记的 hash
    let hash_with_src = "a".repeat(64);
    let hash_no_src = "b".repeat(64);
    {
        let mut storage = kernel.__test_storage().unwrap();
        // feed:blob-src:{hash} → {from}:{since}（键线形见 feed/mod.rs 存储键）。
        // since 用当前时间戳——过期判定按 now-since > TTL（30 天），用旧值会被判过期。
        storage
            .put(
                &format!("feed:blob-src:{hash_with_src}"),
                &format!("{source}:{}", spark_core::p2p::node::system_now_ms()),
            )
            .unwrap();
    }
    let script = format!(
        r#"
spark.ensureBot('blob-bot', 'Blob Bot');
spark.onMessage(function (payload) {{
    var withSrc = spark.data.readBlob('{hash_with_src}');
    var noSrc = spark.data.readBlob('{hash_no_src}');
    spark.reply(payload, 'withSrc=' + withSrc.status + ' noSrc=' + noSrc.status);
}});
"#
    );
    kernel
        .plugin_start_background("echo-plugin", &script, &test_permissions())
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:echo-plugin:blob-bot", "Blob Bot");
    send_text(&mut kernel, &conv_id, "go");
    wait_until(
        || {
            kernel
                .message_list_messages(PERSONAL, &conv_id)
                .unwrap()
                .iter()
                .any(|m| m.sender_id == "bot:echo-plugin:blob-bot" && m.content.starts_with("withSrc=pending"))
        },
        5_000,
        "data.readBlob 未命中回 pending",
    );

    let storage = kernel.__test_storage().unwrap();
    // 有来源登记：节流键被置（请求路径已进入；p2p 未启动仅跳过实际投递）
    assert!(
        storage.get(&spark_core::plugindata::blob::blob_req_key(&hash_with_src)).unwrap().is_some(),
        "有来源登记 → 触发 feed-blob-req 路径（blob:req 节流键置位）"
    );
    // 无来源登记：节流键不置（维持 pdsync 自设备拉取现状）
    assert!(
        storage.get(&spark_core::plugindata::blob::blob_req_key(&hash_no_src)).unwrap().is_none(),
        "无来源登记 → 不触发 feed-blob-req（维持 pdsync want）"
    );
    // 两者都置了 want 标记（未命中回 pending 的通用行为）
    assert!(
        storage.get(&spark_core::plugindata::blob::blob_want_key(&hash_no_src)).unwrap().is_some(),
        "未命中置 want 标记"
    );
}

/// spark.identity.verify / spark.identity.sign 全链路（QuickJS 后台）：
/// - verify：JS 侧用种子派生域身份公钥验签（纯函数，返回布尔）；
/// - sign：后台签 payload（域缺省 = plugin 根域），公钥可反向验签一致。
#[test]
fn background_identity_verify_and_sign_roundtrip() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    declare_plugin_collection(&mut kernel, "spark-moments", "spark-moments:posts");
    let script = r#"
var payload = 'spark-moments:post:abc123';
// sign：以域身份签 payload，返回 {domain, domainId, publicKey, signature, payloadHash}
var sig = spark.identity.sign(payload);
spark.data.save('spark-moments:posts', 'sig-result', {
    payload: payload,
    domain: sig.domain,
    domainId: sig.domainId,
    publicKey: sig.publicKey,
    signature: sig.signature,
    payloadHash: sig.payloadHash,
    valid: spark.identity.verify({ payload: payload, sig: sig.signature, pubKey: sig.publicKey }),
    tampered: spark.identity.verify({ payload: payload + 'x', sig: sig.signature, pubKey: sig.publicKey })
});
"#;
    kernel
        .plugin_start_background("spark-moments", script, &test_permissions())
        .unwrap();
    // 等待脚本顶层执行落库（JS 线程异步；data.save 为同步 host 调用）
    wait_until(
        || {
            kernel
                .data_get("plugin:spark-moments", "spark-moments:posts", "sig-result", None, None)
                .unwrap()
                .is_some()
        },
        5_000,
        "identity.sign/verify 结果落库",
    );
    let result = kernel
        .data_get("plugin:spark-moments", "spark-moments:posts", "sig-result", None, None)
        .unwrap()
        .unwrap();
    assert_eq!(result["domain"], "plugin:spark-moments", "sign 域缺省 = 插件根域");
    assert_eq!(result["valid"], serde_json::json!(true), "原始 payload 验签通过");
    assert_eq!(
        result["tampered"],
        serde_json::json!(false),
        "篡改 payload 验签失败"
    );
    // 域身份确定性：同一域重复 sign 得同一公钥（派生可复现）
    let payload_hash_len = result["payloadHash"].as_str().unwrap().len();
    assert_eq!(payload_hash_len, 64, "payloadHash 为 sha256 hex");
    assert!(result["signature"].as_str().unwrap().len() > 0);
}

/// 未授权插件的 identity.sign 被拒（高级权限，capability 前置强制）。
#[test]
fn background_identity_sign_denied_without_permission() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    declare_plugin_collection(&mut kernel, "spark-moments", "spark-moments:posts");
    let script = r#"
try {
    spark.identity.sign('payload');
    spark.data.save('spark-moments:posts', 'deny-check', { ok: true });
} catch (e) {
    spark.data.save('spark-moments:posts', 'deny-check', { denied: true, msg: String(e) });
}
"#;
    // 有 storage 写权限但无 identity:sign（高级）→ sign 被拒、结果可落库
    let perms = vec!["storage:read".to_string(), "storage:write".to_string()];
    kernel
        .plugin_start_background("spark-moments", script, &perms)
        .unwrap();
    wait_until(
        || {
            kernel
                .data_get("plugin:spark-moments", "spark-moments:posts", "deny-check", None, None)
                .unwrap()
                .is_some()
        },
        5_000,
        "未授权 sign 拒绝分支落库",
    );
    let result = kernel
        .data_get("plugin:spark-moments", "spark-moments:posts", "deny-check", None, None)
        .unwrap()
        .unwrap();
    assert_eq!(result["denied"], serde_json::json!(true), "未授权 sign 应被拒");
    assert!(
        result["msg"].as_str().unwrap().contains("identity:sign"),
        "拒绝消息应含缺失权限：{}",
        result["msg"]
    );
}

/// spark.messages.sendAppMessage 全链路（QuickJS 后台）：
/// - 写 `app:{pluginId}` 会话（运行时绑定 plugin_id，不信 JS 自报）；
/// - summary 纯文本摘要 + 可选 card；落库可经内核 message_app_list 读取。
#[test]
fn background_send_app_message_persists() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    let script = r#"
spark.onMessage(function (payload) {
    for (var i = 0; i < 3; i++) {
        spark.messages.sendAppMessage({
            summary: '互动通知 #' + i,
            card: { viewId: 'notify-card', data: { postId: 'p1', type: 'like' } }
        });
    }
});
"#;
    kernel
        .plugin_start_background("spark-moments", script, &test_permissions())
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:spark-moments:nbot", "N Bot");
    send_text(&mut kernel, &conv_id, "go");
    // 3 条在 10 条限额内，均应成功写入 app 会话
    wait_until(
        || kernel.message_app_list(PERSONAL, "spark-moments").unwrap().len() == 3,
        5_000,
        "3 条互动通知全部写入 app 会话",
    );
    let msgs = kernel.message_app_list(PERSONAL, "spark-moments").unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].summary, "互动通知 #0");
    assert_eq!(msgs[0].plugin_id, "spark-moments");
    assert_eq!(
        msgs[0].card.as_ref().map(|c| c.view_id.as_str()),
        Some("notify-card"),
        "card 透传"
    );
}

/// spark.messages.sendAppMessage 限流：单次回调连发 12 条，第 11 条起被
/// RateLimited 拒绝（10 条/60s，与 Kernel 门面共享同一 app_msg_limiter）。
#[test]
fn background_send_app_message_rate_limits() {
    let (_dir, mut kernel, _root) = kernel_with_identity();
    declare_plugin_collection(&mut kernel, "spark-moments", "spark-moments:posts");
    let script = r#"
spark.onMessage(function (payload) {
    var rejected = null;
    var sent = 0;
    for (var i = 0; i < 12; i++) {
        try {
            spark.messages.sendAppMessage({ summary: 'burst #' + i });
            sent++;
        } catch (e) {
            rejected = String(e);
            break;
        }
    }
    spark.data.save('spark-moments:posts', 'burst-result', { rejected: rejected, sent: sent });
});
"#;
    kernel
        .plugin_start_background("spark-moments", script, &test_permissions())
        .unwrap();
    let conv_id = setup_bot_conv_named(&mut kernel, "bot:spark-moments:rbot", "R Bot");
    send_text(&mut kernel, &conv_id, "burst");
    wait_until(
        || {
            kernel
                .data_get("plugin:spark-moments", "spark-moments:posts", "burst-result", None, None)
                .unwrap()
                .is_some()
        },
        5_000,
        "burst 限流分支落库",
    );
    let result = kernel
        .data_get("plugin:spark-moments", "spark-moments:posts", "burst-result", None, None)
        .unwrap()
        .unwrap();
    assert!(
        result["rejected"].as_str().unwrap().contains("RateLimited"),
        "超过 10 条/60s 应被限流拒绝：{}",
        result["rejected"]
    );
    assert_eq!(result["sent"], serde_json::json!(10), "前 10 条成功、第 11 条被拒");
}
