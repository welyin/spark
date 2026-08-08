//! 单插件后台运行时：专用 OS 线程 + 独立 QuickJS 实例（plugin-runtime 设计 §二）。
//!
//! - **一插件一线程一实例**：QuickJS 非线程安全，天然一线程一 runtime；
//!   插件崩溃/死循环只影响本线程，内核与其他插件无感；
//! - **熔断三件套**：堆上限 64MB、栈上限 1MB、单次回调 5s 超时（经引擎
//!   interrupt handler 强制中断）；停机请求也走 interrupt handler，保证
//!   JS 死循环中的线程也能退出；
//! - **事件模型**：内核 → 插件为 mpsc 事件入队（当前仅会话消息），插件 →
//!   内核为同步 host 调用（`__spark_host_call`，经 [`PluginHostShared`]
//!   分发）；JS 侧 `spark.onMessage` 注册的回调在事件循环内逐个执行。
//!
//! JS API（PRELUDE 注入，与插件 SDK 的 API 面对齐的最小子集）：
//! `spark.onMessage(fn)` / `spark.reply(payload, text)` /
//! `spark.ensureBot(botId, displayName)` / `spark.log(msg)`。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rquickjs::{Context, Ctx, Function, Runtime};
use serde_json::Value;

use super::error::{PluginError, Result};
use super::host_env::PluginHostShared;

/// 单次 JS 回调执行时限（超时由 interrupt handler 强制中断，防死循环）。
const JS_CALLBACK_DEADLINE_MS: u64 = 5_000;
/// 每插件 QuickJS 堆上限（引擎级熔断；普通 bot 逻辑占用在个位数 MB）。
const JS_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// JS 栈上限（防无限递归撑爆线程栈）。
const JS_STACK_LIMIT_BYTES: usize = 1024 * 1024;
/// 事件轮询间隔（也是停机信号的最大响应延迟）。
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 无进行中回调时的 deadline 哨兵（interrupt handler 只看停机标志）。
const NO_DEADLINE: u64 = u64::MAX;

/// 发往插件运行时的事件（内核 → 插件单向队列的元素）。
pub(crate) enum PluginEvent {
    /// 会话消息（与 ChatReceived 同构载荷：`{spaceKey, conversation, message}`）。
    Chat(Value),
}

/// 运行中插件的句柄（注册表持有；线程存活以通道可达 + 未置停机标志为准）。
pub(crate) struct PluginRuntimeHandle {
    event_tx: Sender<PluginEvent>,
    stop: Arc<AtomicBool>,
    /// 本次启动的世代号：线程退出自注销时按世代比对，防崩溃线程误删
    /// 同名插件的新一轮启动句柄。
    pub(crate) generation: u64,
}

impl PluginRuntimeHandle {
    /// 入队事件（非阻塞；线程已退出返回 Err，调用方据此注销残柄）。
    pub(crate) fn dispatch(&self, event: PluginEvent) -> std::result::Result<(), ()> {
        self.event_tx.send(event).map_err(|_| ())
    }

    /// 请求停机：置标志（事件循环 100ms 内响应；JS 死循环由 interrupt
    /// handler 强制中断），随后通道随句柄 drop 断开兜底。
    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 启动世代计数（进程内单调递增）。
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// 启动插件后台线程，返回（事件句柄，线程 JoinHandle）。线程退出（含崩溃）
/// 时经 `registry` 按世代自注销。
pub(crate) fn spawn_plugin_runtime(
    plugin_id: &str,
    script: &str,
    host: PluginHostShared,
    registry: super::registry::PluginRuntimeRegistry,
) -> Result<(PluginRuntimeHandle, std::thread::JoinHandle<()>)> {
    let (event_tx, event_rx) = channel::<PluginEvent>();
    let stop = Arc::new(AtomicBool::new(false));
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let handle = PluginRuntimeHandle {
        event_tx,
        stop: Arc::clone(&stop),
        generation,
    };
    let plugin_id_owned = plugin_id.to_string();
    let script_owned = script.to_string();
    let join = std::thread::Builder::new()
        .name(format!("plugin-{plugin_id}"))
        .spawn(move || {
            if let Err(error) = run_plugin(&plugin_id_owned, &script_owned, host, &event_rx, &stop)
            {
                // 崩溃隔离：脚本错误/超时中断只终止本插件线程；重启策略
                // （指数退避 + 故障标记）留给 supervisor 完善阶段
                eprintln!("[plugin:{plugin_id_owned}] background runtime exited: {error}");
            }
            registry.remove_if_generation(&plugin_id_owned, generation);
        })
        .map_err(|e| PluginError::Script(format!("spawn plugin thread: {e}")))?;
    Ok((handle, join))
}

/// 插件线程主流程：建引擎 → 装宿主能力 → 加载脚本 → 事件循环。
fn run_plugin(
    plugin_id: &str,
    script: &str,
    host: PluginHostShared,
    rx: &Receiver<PluginEvent>,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let runtime = Runtime::new()
        .map_err(|e| PluginError::Script(format!("create quickjs runtime: {e}")))?;
    runtime.set_memory_limit(JS_MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(JS_STACK_LIMIT_BYTES);
    // 中断源二合一：外部停机请求，或单次回调超时（deadline 为 epoch millis）
    let deadline = Arc::new(AtomicU64::new(NO_DEADLINE));
    runtime.set_interrupt_handler(Some(Box::new({
        let stop = Arc::clone(stop);
        let deadline = Arc::clone(&deadline);
        move || {
            stop.load(Ordering::Relaxed) || now_epoch_ms() >= deadline.load(Ordering::Relaxed)
        }
    })));
    let context = Context::full(&runtime)
        .map_err(|e| PluginError::Script(format!("create quickjs context: {e}")))?;

    context.with(|ctx| {
        // 脚本加载同样受回调时限约束（顶层代码也可能是死循环）
        deadline.store(now_epoch_ms() + JS_CALLBACK_DEADLINE_MS, Ordering::Relaxed);
        let result = install_and_load(&ctx, plugin_id, &host, script);
        deadline.store(NO_DEADLINE, Ordering::Relaxed);
        result
    })?;

    loop {
        match rx.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok(PluginEvent::Chat(payload)) => {
                let payload_json = payload.to_string();
                let outcome = context.with(|ctx| {
                    deadline.store(now_epoch_ms() + JS_CALLBACK_DEADLINE_MS, Ordering::Relaxed);
                    let result = dispatch_js(&ctx, "message", &payload_json);
                    deadline.store(NO_DEADLINE, Ordering::Relaxed);
                    result
                });
                // JS 回调异常（含超时中断）：崩溃即退出，不拖着错误状态继续跑
                outcome?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // drain microtask 队列（Promise 回调；host 调用当前全同步，本循环
        // 为后续异步能力预留）
        while runtime.is_job_pending() {
            if runtime.execute_pending_job().is_err() {
                break;
            }
        }
    }
    Ok(())
}

/// 装宿主能力 + 注入 prelude + 加载插件脚本（均在引擎上下文中执行）。
fn install_and_load(
    ctx: &Ctx<'_>,
    plugin_id: &str,
    host: &PluginHostShared,
    script: &str,
) -> Result<()> {
    let host_call = Function::new(ctx.clone(), {
        let host = host.clone();
        let plugin_id = plugin_id.to_string();
        move |capability: String, payload: String| host.call(&plugin_id, &capability, &payload)
    })
    .map_err(|e| PluginError::Script(format!("install host call: {e}")))?;
    ctx.globals()
        .set("__spark_host_call", host_call)
        .map_err(|e| PluginError::Script(format!("install host call: {e}")))?;
    eval_js(ctx, PRELUDE)?;
    eval_js(ctx, script)
}

/// 调用 JS 侧事件分发函数（PRELUDE 注入的 `__spark_dispatch`）。
fn dispatch_js(ctx: &Ctx<'_>, kind: &str, payload_json: &str) -> Result<()> {
    let dispatch: Function = ctx
        .globals()
        .get("__spark_dispatch")
        .map_err(|e| PluginError::Script(format!("missing __spark_dispatch: {e}")))?;
    dispatch
        .call::<_, ()>((kind.to_string(), payload_json.to_string()))
        .map_err(|e| js_error_detail(ctx, e))
}

fn eval_js(ctx: &Ctx<'_>, source: &str) -> Result<()> {
    ctx.eval::<(), _>(source)
        .map_err(|e| js_error_detail(ctx, e))
}

/// 提取 JS 异常细节：`Error::Exception` 时 message/stack 在上下文异常值里。
fn js_error_detail(ctx: &Ctx<'_>, error: rquickjs::Error) -> PluginError {
    if matches!(error, rquickjs::Error::Exception) {
        let caught = ctx.catch();
        if let Some(object) = caught.as_object() {
            let message: String = object.get("message").unwrap_or_default();
            let stack: String = object.get("stack").unwrap_or_default();
            let detail = if stack.is_empty() {
                message
            } else {
                format!("{message}\n{stack}")
            };
            if !detail.is_empty() {
                return PluginError::Script(detail);
            }
        }
    }
    PluginError::Script(error.to_string())
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// JS 侧运行时门面（插件 SDK 后台 API 面的最小子集；`__spark_host_call`
/// 由 Rust 侧注入，返回 JSON 字符串，错误以 `{"error": ...}` 表达并在此
/// 转为 JS 异常）。
const PRELUDE: &str = r#"
(function () {
    var handlers = {};
    function call(capability, payload) {
        var result = JSON.parse(__spark_host_call(capability, JSON.stringify(payload)));
        if (result && result.error) throw new Error(result.error);
        return result;
    }
    globalThis.spark = {
        onMessage: function (fn) { handlers.message = fn; },
        log: function (msg) {
            __spark_host_call('log', JSON.stringify({ message: String(msg) }));
        },
        ensureBot: function (botId, displayName) {
            return call('contact.ensureBot', { botId: botId, displayName: displayName }).botRootId;
        },
        reply: function (payload, text) {
            return call('message.reply', {
                spaceKey: payload.spaceKey,
                convId: payload.conversation && payload.conversation.id,
                text: String(text)
            });
        }
    };
    globalThis.__spark_dispatch = function (kind, payloadJson) {
        var fn = handlers[kind];
        if (typeof fn !== 'function') return;
        fn(JSON.parse(payloadJson));
    };
})();
"#;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::broadcast;

    use super::super::registry::PluginRuntimeRegistry;
    use super::*;

    /// 最小宿主面：无存储（log 能力不碰存储），验证引擎生命周期与 JS 桥。
    fn bare_host() -> PluginHostShared {
        PluginHostShared {
            storage: Arc::new(Mutex::new(None)),
            io_lock: Arc::new(Mutex::new(())),
            event_tx: broadcast::channel(16).0,
            my_root_id: Arc::new(Mutex::new(None)),
            p2p_node: Arc::new(Mutex::new(None)),
            signing_key: Arc::new(Mutex::new(None)),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// 启动运行时并等待断言成立（轮询 100ms；预算 10s——须大于
    /// JS_CALLBACK_DEADLINE_MS 的 5s 熔断时限，留出中断后的退出余量）。
    fn wait_until(cond: impl FnMut() -> bool, what: &str) {
        wait_until_within(cond, Duration::from_secs(10), what);
    }

    fn wait_until_within(mut cond: impl FnMut() -> bool, budget: Duration, what: &str) {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timeout waiting for: {what}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn infinite_loop_interrupted_by_deadline() {
        // 死循环回调必须在 5s 时限内被 interrupt handler 中断，线程随之退出
        let script = r#"spark.onMessage(function () { while (true) {} });"#;
        let (handle, join) = spawn_plugin_runtime(
            "loop-test",
            script,
            bare_host(),
            PluginRuntimeRegistry::default(),
        )
        .unwrap();
        handle
            .dispatch(PluginEvent::Chat(serde_json::json!({
                "spaceKey": "personal",
                "conversation": { "id": "dm:x", "peerId": "bot:loop-test:x" },
                "message": { "senderId": "me" }
            })))
            .unwrap();
        wait_until(|| join.is_finished(), "死循环线程被超时中断退出");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn script_syntax_error_exits_thread() {
        let (handle, join) = spawn_plugin_runtime(
            "bad-script",
            "this is not js",
            bare_host(),
            PluginRuntimeRegistry::default(),
        )
        .unwrap();
        wait_until(|| join.is_finished(), "语法错误脚本线程退出");
        // 线程退出后 dispatch 失败（调用方据此注销残柄）
        assert!(handle.dispatch(PluginEvent::Chat(Value::Null)).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_request_exits_idle_thread() {
        let (handle, join) = spawn_plugin_runtime(
            "idle-test",
            "",
            bare_host(),
            PluginRuntimeRegistry::default(),
        )
        .unwrap();
        assert!(!join.is_finished());
        handle.request_stop();
        wait_until(|| join.is_finished(), "停机请求后空闲线程退出");
    }
}
