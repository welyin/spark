//! sys 代理命令：插件通过内核代理执行外部命令（sys_exec）或发起 HTTP 请求（sys_fetch）。
//!
//! 实现已下沉内核（`spark_core::sys`，插件后台运行时的 sys.exec/fetch 能力共用）；
//! 本层仅做命令包装与线程模型适配。
//!
//! 安全边界：权限校验由插件桥调度层（bridge-dispatcher.ts）完成，
//! 此命令层仅做纯代理，不额外鉴权。调用方（插件）已授予 system:exec / network:fetch 权限。

use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Emitter;

pub use spark_core::sys::{SysExecResult, SysFetchChunk, SysFetchResult};

/// 单调计数：仅作流 id 组合入参，不做安全凭据（见 next_stream_id 注释）。
static STREAM_SEQ: AtomicU64 = AtomicU64::new(1);

/// 生成不可预测的 streamId（16 位 hex）。
///
/// 流事件名 `sys-stream:{streamId}` 经 Tauri `window.emit` 广播给当前窗口的
/// 所有 `listen` 订阅者，可被同窗口其他插件 iframe `listen` 到——若 streamId
/// 从 0..N 递增可预测，恶意插件可穷举订阅窃读 AI 对话内容（见评审 F1）。
/// 故以系统随机种子的 `RandomState` 哈希（时间戳 + 单调计数）产出随机 id，
/// 不引入新依赖；`RandomState::new()` 的种子来自系统熵源，输出不可预测。
fn next_stream_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = STREAM_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(ts);
    hasher.write_u64(seq);
    format!("{:016x}", hasher.finish())
}

#[derive(Deserialize)]
pub struct SysFetchOptions {
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
}

/// 执行外部命令。async 命令 + spawn_blocking：wait_with_output 是同步阻塞调用，
/// 若为同步 fn 会跑在 Tauri 主线程上——CLI 类命令（如 codebuddy 生成回复数秒）会冻结
/// 整个应用 UI。spawn_blocking 移入阻塞线程池，主事件循环不被占用。
///
/// workdir：可选工作目录。缺省时子进程继承宿主进程 cwd（不可控，曾为 app/ 目录），
/// CLI 工具（如 codebuddy 读当前目录上下文）对 cwd 敏感，应由调用方显式指定。
#[tauri::command]
pub async fn sys_exec(
    program: String,
    args: Vec<String>,
    workdir: Option<String>,
) -> Result<SysExecResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        spark_core::sys::exec_blocking(&program, &args, workdir.as_deref())
    })
    .await
    .map_err(|e| format!("sys_exec 任务执行失败: {e}"))?
}

/// 发起 HTTP 请求（async，透传 reqwest；代理由环境变量 HTTP_PROXY/HTTPS_PROXY 自行感知）
#[tauri::command]
pub async fn sys_fetch(
    url: String,
    options: Option<SysFetchOptions>,
) -> Result<SysFetchResult, String> {
    let opts = options.unwrap_or(SysFetchOptions {
        method: None,
        headers: None,
        body: None,
    });
    spark_core::sys::fetch(
        &url,
        opts.method.as_deref(),
        opts.headers.as_ref(),
        opts.body.as_deref(),
    )
    .await
}

/// 发起 HTTP 流式请求。立即返回 streamId，后续通过
/// `window.emit("sys-stream:{streamId}", chunk)` 逐块推送响应体文本。
#[tauri::command]
pub async fn sys_fetch_stream(
    window: tauri::Window,
    url: String,
    options: Option<SysFetchOptions>,
) -> Result<String, String> {
    let opts = options.unwrap_or(SysFetchOptions {
        method: None,
        headers: None,
        body: None,
    });
    let stream_id = next_stream_id();
    let event_name = format!("sys-stream:{stream_id}");

    let method = opts.method.clone();
    let headers = opts.headers.clone();
    let body = opts.body.clone();

    let event_name_for_chunks = event_name.clone();

    tauri::async_runtime::spawn(async move {
        let window_for_chunks = window.clone();
        let result = spark_core::sys::fetch_stream(
            &url,
            method.as_deref(),
            headers.as_ref(),
            body.as_deref(),
            move |chunk| {
                let _ = window_for_chunks.emit(&event_name_for_chunks, &chunk);
            },
        )
        .await;

        if let Err(e) = result {
            let _ = window.emit(
                &event_name,
                &SysFetchChunk {
                    text: e,
                    done: true,
                    status: 0,
                    headers: HashMap::new(),
                },
            );
        }
    });

    Ok(stream_id)
}
