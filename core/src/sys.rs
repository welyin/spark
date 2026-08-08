//! 系统能力：外部命令执行与 HTTP 请求代理。
//!
//! 对应插件权限 `system:exec` / `network:fetch` 的内核侧实现，由壳层
//! `sys_exec`/`sys_fetch` 命令与插件后台运行时的 `sys.exec`/`sys.fetch`
//! 能力共用（单一实现下沉内核，壳层只做命令包装）。
//!
//! 安全边界：本模块不做鉴权——调用方（桥调度层 / 插件运行时 capability
//! 分发）已完成权限校验，此处为纯代理。

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

/// HTTP 代理读环境变量（HTTP_PROXY/HTTPS_PROXY），与壳层 reqwest 行为一致。
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// 外部命令执行结果。序列化为 camelCase 与前端约定一致（exitCode）——
/// 漏掉此属性曾导致前端读取恒为 undefined，探测全部误判失败。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SysExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// HTTP 请求结果。
#[derive(Serialize, Clone)]
pub struct SysFetchResult {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// GUI 进程（Tauri/WebView）的环境变量通常只继承系统 PATH，缺用户 PATH——
/// 终端能跑 npm 但应用 spawn 报 program not found 即源于此。
/// 合并 系统PATH + 用户PATH，让子进程拿到与终端一致的搜索路径。
#[cfg(target_os = "windows")]
fn merged_path_env() -> String {
    let machine = std::env::var("PATH").unwrap_or_default();
    let user = std::env::var("USERPATH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| machine.clone());
    // USERPATH 是 Windows 为用户 PATH 预留的变量名；若为空则退回系统 PATH。
    // 再去注册表取一次用户 PATH 作为兜底（USERPATH 并非总会被设置）。
    if user == machine {
        if let Ok(user_path) = read_user_path_from_registry() {
            if !user_path.is_empty() && !machine.contains(&user_path) {
                return format!("{machine};{user_path}");
            }
        }
        return machine;
    }
    format!("{machine};{user}")
}

#[cfg(target_os = "windows")]
fn read_user_path_from_registry() -> Result<String, String> {
    // HKEY_CURRENT_USER\Environment\Path 是用户 PATH 的权威来源
    let output = Command::new("reg")
        .args(["query", r"HKCU\Environment", "/v", "Path"])
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 输出形如：    Path    REG_EXPAND_SZ    D:\nodejs;...
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Path") {
            if let Some(idx) = trimmed.find("REG_") {
                let value = trimmed[idx..]
                    .splitn(2, char::is_whitespace)
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !value.is_empty() {
                    return Ok(value);
                }
            }
        }
    }
    Err("user Path not found".into())
}

/// Windows 下 Command::new("npm") 只解析 .exe，不会找 npm.cmd/npm.ps1。
/// 对无扩展名的程序名，按 PATHEXT 顺序尝试补全常见脚本后缀。
#[cfg(target_os = "windows")]
fn resolve_program(program: &str) -> String {
    use std::path::Path;
    // 已带扩展名或是明确路径，直接返回
    if program.contains(std::path::MAIN_SEPARATOR)
        || program.contains('/')
        || Path::new(program).extension().is_some()
    {
        return program.to_string();
    }
    // 优先 .cmd（npm/yarn/pnpm 等 Node 工具的标准形态），再 .exe，最后原样
    for ext in [".cmd", ".exe", ".bat"] {
        let candidate = format!("{program}{ext}");
        if program_in_path(&candidate) {
            return candidate;
        }
    }
    program.to_string()
}

#[cfg(target_os = "windows")]
fn program_in_path(name: &str) -> bool {
    let path = merged_path_env();
    for dir in path.split(';').filter(|d| !d.is_empty()) {
        if std::path::Path::new(dir).join(name).exists() {
            return true;
        }
    }
    false
}

/// 执行外部命令（同步阻塞；调用方负责放阻塞线程——壳层命令与插件运行时
/// 均以 spawn_blocking 包裹）。
///
/// workdir：可选工作目录。缺省时子进程继承宿主进程 cwd（不可控），CLI 工具
/// 对 cwd 敏感，应由调用方显式指定。
pub fn exec_blocking(
    program: &str,
    args: &[String],
    workdir: Option<&str>,
) -> Result<SysExecResult, String> {
    #[cfg(target_os = "windows")]
    let program = resolve_program(program);

    let mut cmd = Command::new(&program);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // GUI 进程补全 PATH（合并用户 PATH），让子进程能找到终端可见的命令
    #[cfg(target_os = "windows")]
    cmd.env("PATH", merged_path_env());

    // 指定工作目录（CLI 工具的上下文根）
    if let Some(dir) = workdir {
        if !dir.is_empty() {
            cmd.current_dir(dir);
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("启动命令 {program} 失败: {e}"))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("等待命令 {program} 退出失败: {e}"))?;

    Ok(SysExecResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// 发起 HTTP 请求（async，透传 reqwest；代理由环境变量自行感知）。
pub async fn fetch(
    url: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
) -> Result<SysFetchResult, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;

    let mut req = match method.unwrap_or("GET") {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        _ => client.get(url),
    };

    if let Some(headers) = headers {
        for (k, v) in headers {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                req = req.header(name, v.as_str());
            }
        }
    }

    if let Some(body) = body {
        req = req.body(body.to_string());
    }

    let response = req.send().await.map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status().as_u16();
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;

    Ok(SysFetchResult {
        status,
        headers: resp_headers,
        body,
    })
}
