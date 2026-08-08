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
    /// JS 事件分发：`kind` 路由到 prelude 注册的处理器（`message` 会话消息、
    /// `sys-exec-result`/`sys-fetch-result` 异步能力结果回流）。
    Dispatch { kind: String, payload: Value },
    /// 宿主 → 插件查询（`plugin_host_query`，如删除联系人前的「bot 还在吗」
    /// 询问）：JS 应答经 `query.respond` host 调用回流到在途表。
    Query {
        query_id: u64,
        kind: String,
        payload: Value,
    },
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
    permissions: Vec<String>,
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
    let rtx = super::host_env::PluginRuntimeContext {
        plugin_id: plugin_id_owned.clone(),
        event_tx: handle.event_tx.clone(),
        permissions,
    };
    let join = std::thread::Builder::new()
        .name(format!("plugin-{plugin_id}"))
        .spawn(move || {
            if let Err(error) = run_plugin(&script_owned, host, rtx, &event_rx, &stop) {
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
    script: &str,
    host: PluginHostShared,
    rtx: super::host_env::PluginRuntimeContext,
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
        let result = install_and_load(&ctx, &host, &rtx, script);
        deadline.store(NO_DEADLINE, Ordering::Relaxed);
        result
    })?;

    loop {
        match rx.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok(PluginEvent::Dispatch { kind, payload }) => {
                let payload_json = payload.to_string();
                // 时限覆盖回调与随后的 microtask 排空：await 续体（Promise
                // 回调）里的死循环同样被熔断，不得逃逸时限
                deadline.store(now_epoch_ms() + JS_CALLBACK_DEADLINE_MS, Ordering::Relaxed);
                let outcome = context.with(|ctx| dispatch_js(&ctx, &kind, &payload_json));
                let drained = drain_pending_jobs(&runtime);
                deadline.store(NO_DEADLINE, Ordering::Relaxed);
                // JS 回调/续体异常（含超时中断）：崩溃即退出，不拖着错误状态继续跑
                outcome?;
                drained?;
            }
            Ok(PluginEvent::Query {
                query_id,
                kind,
                payload,
            }) => {
                let payload_json = payload.to_string();
                deadline.store(now_epoch_ms() + JS_CALLBACK_DEADLINE_MS, Ordering::Relaxed);
                let outcome = context.with(|ctx| query_js(&ctx, query_id, &kind, &payload_json));
                let drained = drain_pending_jobs(&runtime);
                deadline.store(NO_DEADLINE, Ordering::Relaxed);
                outcome?;
                drained?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    Ok(())
}

/// 装宿主能力 + 注入 prelude + 加载插件脚本（均在引擎上下文中执行）。
fn install_and_load(
    ctx: &Ctx<'_>,
    host: &PluginHostShared,
    rtx: &super::host_env::PluginRuntimeContext,
    script: &str,
) -> Result<()> {
    let host_call = Function::new(ctx.clone(), {
        let host = host.clone();
        let rtx = rtx.clone();
        move |capability: String, payload: String| host.call(&rtx, &capability, &payload)
    })
    .map_err(|e| PluginError::Script(format!("install host call: {e}")))?;
    ctx.globals()
        .set("__spark_host_call", host_call)
        .map_err(|e| PluginError::Script(format!("install host call: {e}")))?;
    ctx.globals()
        .set("__spark_plugin_id", rtx.plugin_id.clone())
        .map_err(|e| PluginError::Script(format!("install plugin id: {e}")))?;
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

/// 调用 JS 侧查询处理函数（PRELUDE 注入的 `__spark_query`；应答由 JS 经
/// `query.respond` host 调用异步回流）。
fn query_js(ctx: &Ctx<'_>, query_id: u64, kind: &str, payload_json: &str) -> Result<()> {
    let query: Function = ctx
        .globals()
        .get("__spark_query")
        .map_err(|e| PluginError::Script(format!("missing __spark_query: {e}")))?;
    query
        .call::<_, ()>((query_id, kind.to_string(), payload_json.to_string()))
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

/// 排空 microtask 队列（Promise 回调）。调用方须保持中断时限武装到本函数
/// 返回，使续体（await 之后的代码）里的死循环同样被熔断；任务出错（含
/// 超时中断）即报错返回。
fn drain_pending_jobs(runtime: &Runtime) -> Result<()> {
    while runtime.is_job_pending() {
        runtime
            .execute_pending_job()
            .map_err(|e| PluginError::Script(format!("microtask job failed: {e}")))?;
    }
    Ok(())
}

/// JS 侧运行时门面（插件后台 API 面；`__spark_host_call` 由 Rust 侧注入，
/// 返回 JSON 字符串，错误以 `{"error": ...}` 表达并在 `call()` 转为异常）。
///
/// 结构：
/// - `handlers`：事件回调（`message` 会话消息；异步能力结果 kind 内置消化）；
/// - `queryHandlers`：宿主查询回调（`spark.onQuery(kind, fn)`，应答经
///   `query.respond` 回流，支持异步处理器——Promise 由引擎 job 队列排空）；
/// - `pending`：异步能力（sys.exec/fetch）的 callId → Promise 解析器配对表，
///   结果经 `*-result` 事件回流时兑现。
const PRELUDE: &str = r#"
(function () {
    var handlers = {};
    var queryHandlers = {};
    var pending = {};
    var nextCallId = 1;

    function call(capability, payload) {
        var result = JSON.parse(__spark_host_call(capability, JSON.stringify(payload)));
        if (result && result.error) throw new Error(result.error);
        return result;
    }

    // 异步能力统一发起：登记 pending → 启动 host 任务（立即返回）→
    // 结果事件回流时兑现 Promise
    function startAsync(capability, payload) {
        return new Promise(function (resolve, reject) {
            var callId = nextCallId++;
            pending[callId] = { resolve: resolve, reject: reject };
            try {
                call(capability, Object.assign({ callId: callId }, payload));
            } catch (error) {
                delete pending[callId];
                reject(error);
            }
        });
    }

    function settleAsync(payload) {
        var slot = pending[payload.callId];
        if (!slot) return;
        delete pending[payload.callId];
        if (payload.error) slot.reject(new Error(payload.error));
        else slot.resolve(payload);
    }

    function makeConsole(level) {
        return function () {
            var parts = [];
            for (var i = 0; i < arguments.length; i++) {
                var v = arguments[i];
                parts.push(typeof v === 'string' ? v : JSON.stringify(v));
            }
            __spark_host_call('log', JSON.stringify({ message: level + ' ' + parts.join(' ') }));
        };
    }
    globalThis.console = {
        log: makeConsole(''),
        info: makeConsole(''),
        warn: makeConsole('[warn]'),
        error: makeConsole('[error]')
    };

    globalThis.spark = {
        onMessage: function (fn) { handlers.message = fn; },
        onQuery: function (kind, fn) { queryHandlers[kind] = fn; },
        get pluginId() { return __spark_plugin_id; },
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
        },
        docs: {
            // domain 可选：缺省为插件自身域；跨域仅限显式指定（见 host_env
            // resolve_doc_domain 的合法性约束）
            get: function (collection, id, domain) {
                return call('docs.get', { collection: collection, id: id, domain: domain || null });
            },
            put: function (collection, id, doc, config, domain) {
                call('docs.put', { collection: collection, id: id, doc: doc, config: config || null, domain: domain || null });
            },
            delete: function (collection, id, config, domain) {
                call('docs.delete', { collection: collection, id: id, config: config || null, domain: domain || null });
            },
            query: function (collection, options, config, domain) {
                return call('docs.query', { collection: collection, options: options || null, config: config || null, domain: domain || null });
            },
            defineCollection: function (collection, schema) {
                call('docs.defineCollection', { collection: collection, schema: schema });
            }
        },
        sys: {
            exec: function (program, args, workdir) {
                return startAsync('sys.exec.start', {
                    program: program, args: args || [], workdir: workdir || null
                });
            },
            fetch: function (url, options) {
                options = options || {};
                return startAsync('sys.fetch.start', {
                    url: url,
                    method: options.method || 'GET',
                    headers: options.headers || null,
                    body: options.body || null
                });
            }
        }
    };

    globalThis.__spark_dispatch = function (kind, payloadJson) {
        var payload = JSON.parse(payloadJson);
        if (kind === 'sys-exec-result' || kind === 'sys-fetch-result') {
            settleAsync(payload);
            return;
        }
        var fn = handlers[kind];
        if (typeof fn === 'function') fn(payload);
    };

    globalThis.__spark_query = function (queryId, kind, payloadJson) {
        var fn = queryHandlers[kind];
        var result;
        try {
            // 同步调用必须包 try/catch：处理器同步抛错（含 payload JSON
            // 解析失败）要转成拒绝应答回流，不能冒出 __spark_query 终结线程
            result = typeof fn === 'function' ? fn(JSON.parse(payloadJson)) : null;
        } catch (error) {
            result = Promise.reject(error);
        }
        Promise.resolve(result).then(
            function (value) {
                call('query.respond', { queryId: queryId, result: value === undefined ? null : value });
            },
            function (error) {
                call('query.respond', { queryId: queryId, result: { error: String(error) } });
            }
        );
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
            collection_configs: Arc::new(Mutex::new(Default::default())),
            pending_queries: Arc::new(Mutex::new(Default::default())),
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
            Vec::new(),
        )
        .unwrap();
        handle
            .dispatch(PluginEvent::Dispatch {
                kind: "message".to_string(),
                payload: serde_json::json!({
                    "spaceKey": "personal",
                    "conversation": { "id": "dm:x", "peerId": "bot:loop-test:x" },
                    "message": { "senderId": "me" }
                }),
            })
            .unwrap();
        wait_until(|| join.is_finished(), "死循环线程被超时中断退出");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn infinite_loop_in_continuation_interrupted_by_deadline() {
        // Promise 续体在 microtask 排空阶段执行（事件分发之后）：其中的
        // 死循环同样受 5s 时限熔断，线程随之退出
        let script = r#"
spark.onMessage(function () {
    Promise.resolve().then(function () { while (true) {} });
});
"#;
        let (handle, join) = spawn_plugin_runtime(
            "loop-then-test",
            script,
            bare_host(),
            PluginRuntimeRegistry::default(),
            Vec::new(),
        )
        .unwrap();
        handle
            .dispatch(PluginEvent::Dispatch {
                kind: "message".to_string(),
                payload: serde_json::json!({
                    "spaceKey": "personal",
                    "conversation": { "id": "dm:x", "peerId": "bot:loop-then-test:x" },
                    "message": { "senderId": "me" }
                }),
            })
            .unwrap();
        wait_until(|| join.is_finished(), "续体死循环线程被超时中断退出");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn script_syntax_error_exits_thread() {
        let (handle, join) = spawn_plugin_runtime(
            "bad-script",
            "this is not js",
            bare_host(),
            PluginRuntimeRegistry::default(),
            Vec::new(),
        )
        .unwrap();
        wait_until(|| join.is_finished(), "语法错误脚本线程退出");
        // 线程退出后 dispatch 失败（调用方据此注销残柄）
        assert!(
            handle
                .dispatch(PluginEvent::Dispatch {
                    kind: "message".to_string(),
                    payload: Value::Null,
                })
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_request_exits_idle_thread() {
        let (handle, join) = spawn_plugin_runtime(
            "idle-test",
            "",
            bare_host(),
            PluginRuntimeRegistry::default(),
            Vec::new(),
        )
        .unwrap();
        assert!(!join.is_finished());
        handle.request_stop();
        wait_until(|| join.is_finished(), "停机请求后空闲线程退出");
    }
}
