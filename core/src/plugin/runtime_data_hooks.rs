//! 插件后台运行时 JS prelude（O3 起独立成文件，Z5 650 行硬线）。
//!
//! 从 `runtime.rs` 拆出的子模块：JS 侧运行时门面（插件后台 API 面），含
//! `spark.onMessage/reply/ensureBot/log`、`docs.*`、`data.*`（含 O3 filtered
//! 权限钩子 `onReadFilter`/`onWriteFilter` + `data.canRead`/`data.canWrite`
//! 宿主查询裁决）、`sys.*`。`__spark_host_call` 由 Rust 侧注入，返回 JSON
//! 字符串，错误以 `{"error": ...}` 表达并在 `call()` 转为异常。
//!
//! 结构：
//! - `handlers`：事件回调（`message` 会话消息；异步能力结果 kind 内置消化）；
//! - `queryHandlers`：宿主查询回调（`spark.onQuery(kind, fn)`，应答经
//!   `query.respond` 回流，支持异步处理器——Promise 由引擎 job 队列排空）；
//! - `pending`：异步能力（sys.exec/fetch）的 callId → Promise 解析器配对表，
//!   结果经 `*-result` 事件回流时兑现；
//! - `readFilters`/`writeFilters`：O3 filtered 权限钩子注册表（按集合名，
//!   不含 @v 代际）——prelude 经 `data.onReadFilter`/`onWriteFilter` 注册，
//!   同步宿主 filter_caps；宿主经 PluginEvent::Query（data.canRead/canWrite）
//!   在事件循环内查表裁决。

pub(crate) const PRELUDE: &str = r#"
(function () {
    var handlers = {};
    var queryHandlers = {};
    var pending = {};
    var nextCallId = 1;
    // O3 filtered 权限钩子注册表（按集合名，不含 @v 代际）
    var readFilters = {};
    var writeFilters = {};
    // sys.fetchStream 逐块回调表：callId → onChunk（发起时登记，done 时清除）
    var streamChunks = {};

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
        // 流式终态顺带清逐块回调表（防泄漏——done 块经 chunk 通道到达时已清，
        // 错误路径（无 done 块）由这里兜底）
        if (streamChunks[payload.callId]) delete streamChunks[payload.callId];
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
        // 流式回复：先落一条 streaming 占位消息（status='streaming'），逐 chunk
        // 追加 content 并重发 ChatReceived（前端按消息 id 更新），end 收尾为
        // 终态。占位消息 id 由 start 返回，chunk/end 按 id 定位。
        replyStreamStart: function (payload) {
            return call('message.replyStreamStart', {
                spaceKey: payload.spaceKey,
                convId: payload.conversation && payload.conversation.id
            }).messageId;
        },
        replyStreamChunk: function (payload, messageId, text) {
            call('message.replyStreamChunk', {
                spaceKey: payload.spaceKey,
                convId: payload.conversation && payload.conversation.id,
                messageId: messageId,
                text: String(text)
            });
        },
        replyStreamEnd: function (payload, messageId, error) {
            call('message.replyStreamEnd', {
                spaceKey: payload.spaceKey,
                convId: payload.conversation && payload.conversation.id,
                messageId: messageId,
                error: error ? String(error) : null
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
        // P6 声明式数据 API：策略随声明走，读写零同步参数（personal scope；
        // 写库即同步，版本化/墓碑/删除日志/驻留裁剪由内核完成）
        data: {
            declareCollection: function (decl) {
                return call('data.declareCollection', decl);
            },
            save: function (name, key, value, version) {
                call('data.save', { name: name, key: key, value: value, version: version || null });
            },
            del: function (name, key, version) {
                call('data.delete', { name: name, key: key, version: version || null });
            },
            get: function (name, key, version) {
                return call('data.get', { name: name, key: key, version: version || null });
            },
            query: function (name, options, version) {
                options = options || {};
                return call('data.query', {
                    name: name,
                    prefix: options.prefix || null,
                    limit: options.limit || null,
                    cursor: options.cursor || null,
                    version: version || null
                });
            },
            dropVersion: function (name, version) {
                call('data.dropVersion', { name: name, version: String(version) });
            },
            // 内建 blob：base64 入、{hash,size} 出；记录内以 {$blob:hash,...} 引用
            saveBlob: function (base64Data) {
                return call('data.saveBlob', { data: base64Data });
            },
            // 命中 → {status:'ready', data(base64)}；未命中 → {status:'pending'}
            // （已置 want 标记，调和拉取后重读）
            readBlob: function (hash) {
                return call('data.readBlob', { hash: hash });
            },
            // 远端合入本插件集合（pdoc/pdecl）时回调 {pluginId,name,keys}；
            // 本地写不触发（本地路径即时可见）
            onChange: function (fn) {
                handlers['data-change'] = fn;
            },
            // O3 filtered 权限钩子（数据账号侧执行；plugin-data-api §5.1）：
            // 注册回调并同步宿主能力注册表（filter_caps）——数据账号侧 orgq-req
            // 据此判定该集合能否服务（插件未运行即无能力 → fail-closed 降级）。
            onReadFilter: function (name, fn) {
                readFilters[name] = fn;
                call('data.onReadFilter', { collection: name });
            },
            onWriteFilter: function (name, fn) {
                writeFilters[name] = fn;
                call('data.onWriteFilter', { collection: name });
            },
            // O4 encrypted 授权名单（R2）：owner 侧维护名单。orgId 按运行空间
            // 绑定注入——QuickJS 侧不传 orgId（内核从声明记录取 org 绑定），
            // 不接受 payload 自报（自报可越权到其它组织）。
            grantAccess: function (name, members, version) {
                return call('data.grantAccess', { collection: name, members: members || [], version: version || null });
            },
            revokeAccess: function (name, members, version) {
                return call('data.revokeAccess', { collection: name, members: members || [], version: version || null });
            },
            listAccess: function (name, version) {
                return call('data.listAccess', { collection: name, version: version || null });
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
            },
            // 流式 HTTP（ai-chat 主聊天窗口流式回复用）：发起后每收到一个
            // 响应体文本块，宿主经 `sys-stream-chunk` 事件回流（callId 配对），
            // 最后一块 done=true。返回 Promise 在流结束后兑现（done 块载荷）；
            // 逐块内容经 onChunk 回调（可选第二参）。
            fetchStream: function (url, options, onChunk) {
                options = options || {};
                var callId = nextCallId;
                var promise = startAsync('sys.fetchStream.start', {
                    url: url,
                    method: options.method || 'GET',
                    headers: options.headers || null,
                    body: options.body || null
                });
                // startAsync 内部已把 callId 注入 payload；这里取同一个 id 注册
                // 逐块回调（callId 在 startAsync 内 nextCallId++ 前的值）。
                if (typeof onChunk === 'function') streamChunks[callId] = onChunk;
                return promise;
            },
            // 流式执行外部命令（codebuddy `--output-format stream-json` 等
            // NDJSON 流工具）：stdout 按完整行逐块经 `sys-exec-chunk` 回流，
            // 进程退出经 `sys-exec-result` 兑现 Promise（exitCode/stdout/stderr）。
            // 逐行内容经 onChunk 回调（可选第四参）。
            execStream: function (program, args, workdir, onChunk) {
                var callId = nextCallId;
                var promise = startAsync('sys.execStream.start', {
                    program: program,
                    args: args || [],
                    workdir: workdir || null
                });
                if (typeof onChunk === 'function') streamChunks[callId] = onChunk;
                return promise;
            }
        }
    };

    // O3 filtered 钩子执行：宿主（数据账号侧 orgq-req）经 PluginEvent::Query
    // 同步查询 canRead/canWrite——在事件循环内查 readFilters/writeFilters 并
    // 返回布尔裁决（未注册该集合过滤器时返回 true，能力判定由 host 侧兜底）。
    // 钩子抛异常由宿主 fail-closed 处理（拒绝）。
    spark.onQuery('data.canRead', function (p) {
        var f = readFilters[p.collection];
        return typeof f === 'function' ? !!f(p.member, p.key) : true;
    });
    spark.onQuery('data.canWrite', function (p) {
        var f = writeFilters[p.collection];
        return typeof f === 'function' ? !!f(p.member, p.key, p.value) : true;
    });

    globalThis.__spark_dispatch = function (kind, payloadJson) {
        var payload = JSON.parse(payloadJson);
        if (kind === 'sys-exec-result' || kind === 'sys-fetch-result' || kind === 'sys-stream-result') {
            settleAsync(payload);
            return;
        }
        if (kind === 'sys-stream-chunk' || kind === 'sys-exec-chunk') {
            var onChunk = streamChunks[payload.callId];
            if (typeof onChunk === 'function') onChunk(payload.chunk);
            if (payload.chunk && payload.chunk.done) delete streamChunks[payload.callId];
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
