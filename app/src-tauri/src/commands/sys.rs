//! sys 代理命令：插件通过内核代理执行外部命令（sys_exec）或发起 HTTP 请求（sys_fetch）。
//!
//! 实现已下沉内核（`spark_core::sys`，插件后台运行时的 sys.exec/fetch 能力共用）；
//! 本层仅做命令包装与线程模型适配。
//!
//! 安全边界：权限校验由插件桥调度层（bridge-dispatcher.ts）完成，
//! 此命令层仅做纯代理，不额外鉴权。调用方（插件）已授予 system:exec / network:fetch 权限。

use serde::Deserialize;
use std::collections::HashMap;

pub use spark_core::sys::{SysExecResult, SysFetchResult};

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
