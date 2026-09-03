//! sys.* 能力（从 `host_env` 拆出，文件长度硬线）：sys.exec / sys.fetch
//! 的 spawn 编排。零逻辑变化。

use serde_json::Value;

use crate::plugin::error::Result;
use crate::plugin::runtime::PluginEvent;

use std::collections::HashMap;

use super::{PluginHostShared, PluginRuntimeContext, required_call_id, required_str};

// ------------------------------------------------------------------
// 系统能力（sys.*）：启动即返，结果经事件队列异步回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `sys.exec.start`：spawn 到内核 runtime（内部 spawn_blocking），完成
    /// 后向本插件事件队列回 `sys-exec-result`。
    pub(super) fn sys_exec_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let program = required_str(payload, "program")?.to_string();
        let args: Vec<String> = payload
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let workdir = payload
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::sys::exec_blocking(&program, &args, workdir.as_deref())
            })
            .await;
            let payload = match result {
                Ok(Ok(r)) => serde_json::json!({
                    "callId": call_id,
                    "exitCode": r.exit_code,
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                }),
                Ok(Err(error)) => serde_json::json!({ "callId": call_id, "error": error }),
                Err(error) => serde_json::json!({
                    "callId": call_id,
                    "error": format!("exec task join failed: {error}")
                }),
            };
            // 插件线程可能已退出：丢弃结果即可（Promise 随线程销毁失去意义）
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-exec-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `sys.execStream.start`：流式执行外部命令（codebuddy `--output-format
    /// stream-json` 等 NDJSON 流工具）。stdout 按完整行逐块回 `sys-exec-chunk`
    /// （callId 配对，prelude 分发到 onChunk），进程退出回 `sys-exec-result`
    /// 终态兑现 Promise。与非流式 `sys.exec.start` 的区别：多次中间事件 + 一次终态。
    pub(super) fn sys_exec_stream_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let program = required_str(payload, "program")?.to_string();
        eprintln!(
            "[stream-dbg] execStream start program={program} args={:?}",
            payload.get("args")
        );
        let args: Vec<String> = payload
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let workdir = payload
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let chunk_tx = event_tx.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::sys::exec_streaming_blocking(
                    &program,
                    &args,
                    workdir.as_deref(),
                    move |chunk| {
                        eprintln!(
                            "[stream-dbg] execStream chunk done={} text_len={} exit={:?}",
                            chunk.done,
                            chunk.text.len(),
                            chunk.exit_code
                        );
                        let _ = chunk_tx.send(PluginEvent::Dispatch {
                            kind: "sys-exec-chunk".to_string(),
                            payload: serde_json::json!({
                                "callId": call_id,
                                "chunk": {
                                    "text": chunk.text,
                                    "done": chunk.done,
                                    "exitCode": chunk.exit_code,
                                },
                            }),
                        });
                    },
                )
            })
            .await;
            let payload = match result {
                Ok(Ok(r)) => {
                    eprintln!(
                        "[stream-dbg] execStream done exit={} stderr_len={}",
                        r.exit_code,
                        r.stderr.len()
                    );
                    serde_json::json!({
                        "callId": call_id,
                        "exitCode": r.exit_code,
                        "stdout": r.stdout,
                        "stderr": r.stderr,
                    })
                }
                Ok(Err(error)) => {
                    eprintln!("[stream-dbg] execStream error={error}");
                    serde_json::json!({ "callId": call_id, "error": error })
                }
                Err(error) => serde_json::json!({
                    "callId": call_id,
                    "error": format!("exec stream task join failed: {error}")
                }),
            };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-exec-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    pub(super) fn sys_fetch_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let url = required_str(payload, "url")?.to_string();
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let headers: Option<HashMap<String, String>> = payload
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });
        let body = payload
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let payload =
                match crate::sys::fetch(&url, method.as_deref(), headers.as_ref(), body.as_deref())
                    .await
                {
                    Ok(r) => serde_json::json!({
                        "callId": call_id,
                        "status": r.status,
                        "headers": r.headers,
                        "body": r.body,
                    }),
                    Err(error) => serde_json::json!({ "callId": call_id, "error": error }),
                };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-fetch-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `sys.fetchStream.start`：流式 HTTP。每收到一个响应体文本块向本插件
    /// 事件队列回 `sys-stream-chunk`（callId 配对，prelude 分发到 onChunk），
    /// 流结束回 `sys-stream-result`（done 块载荷，prelude 兑现 Promise）。
    /// 与非流式 `sys.fetch.start` 的区别：多次中间事件 + 一次终态事件。
    pub(super) fn sys_fetch_stream_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let url = required_str(payload, "url")?.to_string();
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let headers: Option<HashMap<String, String>> = payload
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });
        let body = payload
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let chunk_tx = event_tx.clone();
            let result = crate::sys::fetch_stream(
                &url,
                method.as_deref(),
                headers.as_ref(),
                body.as_deref(),
                move |chunk| {
                    let done = chunk.done;
                    let _ = chunk_tx.send(PluginEvent::Dispatch {
                        kind: "sys-stream-chunk".to_string(),
                        payload: serde_json::json!({
                            "callId": call_id,
                            "chunk": {
                                "text": chunk.text,
                                "done": chunk.done,
                                "status": chunk.status,
                                "headers": chunk.headers,
                            },
                        }),
                    });
                    // done 块经 chunk 通道到达后，终态结果由下方 sys-stream-result
                    // 兑现 Promise（prelude 的 settleAsync 消费）
                    let _ = done;
                },
            )
            .await;
            let payload = match result {
                Ok(()) => serde_json::json!({ "callId": call_id, "done": true }),
                Err(error) => serde_json::json!({ "callId": call_id, "error": error }),
            };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-stream-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }
}
