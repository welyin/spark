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

/// 外部命令执行总时长上限（与 FETCH_TIMEOUT 同档）：超时强杀子进程并返回错误，
/// 避免挂死子进程永久占用阻塞线程、JS Promise 永不 settle。
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// stdout/stderr 各自的捕获上限（超出部分截断丢弃，但管道持续排空，
/// 防止子进程写满管道阻塞）。子进程输出在 JS 堆（64MB 上限）之外累积，
/// 不设上限可 OOM 整个宿主进程。
const EXEC_MAX_OUTPUT: usize = 1024 * 1024;

/// HTTP 响应体大小上限（60s 超时挡不住慢速无限流，必须按字节数截停）。
const FETCH_MAX_BODY: usize = 10 * 1024 * 1024;

/// 正常退出后等待读者线程收尾的宽限：覆盖「子进程已退出、孙进程仍握管道
/// 写端」场景——超宽限即放弃等待（读者 detach，随管道关闭自行结束）。
const READER_DRAIN_GRACE: Duration = Duration::from_secs(2);

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

/// HTTP 流式响应块：逐块透传响应体文本，最后一块 done=true。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SysFetchChunk {
    pub text: String,
    pub done: bool,
    pub status: u16,
    pub headers: HashMap<String, String>,
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
///
/// 超时 EXEC_TIMEOUT 后强杀整棵进程树并返回错误；stdout/stderr 各截断至
/// EXEC_MAX_OUTPUT（超出部分丢弃，不报错）。
pub fn exec_blocking(
    program: &str,
    args: &[String],
    workdir: Option<&str>,
) -> Result<SysExecResult, String> {
    exec_with_limits(program, args, workdir, EXEC_TIMEOUT, EXEC_MAX_OUTPUT)
}

/// 丢弃超出上限字节的 sink：管道持续排空（防子进程写满阻塞），内存占用有界。
struct CappedSink {
    buf: Vec<u8>,
    cap: usize,
}

impl std::io::Write for CappedSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let room = self.cap.saturating_sub(self.buf.len());
        self.buf.extend_from_slice(&data[..room.min(data.len())]);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn exec_with_limits(
    program: &str,
    args: &[String],
    workdir: Option<&str>,
    timeout: Duration,
    max_output: usize,
) -> Result<SysExecResult, String> {
    #[cfg(target_os = "windows")]
    let program = resolve_program(program);

    let mut cmd = Command::new(&program);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Unix：子进程设为进程组组长，超时可向整组发 SIGKILL（孙进程继承
    // 管道写端的场景，只杀直接子进程读者线程永远等不到 EOF）
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // GUI 进程补全 PATH（合并用户 PATH），让子进程能找到终端可见的命令
    #[cfg(target_os = "windows")]
    cmd.env("PATH", merged_path_env());

    // 指定工作目录（CLI 工具的上下文根）
    if let Some(dir) = workdir {
        if !dir.is_empty() {
            cmd.current_dir(dir);
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动命令 {program} 失败: {e}"))?;

    // stdout/stderr 各起一个读取线程排空管道（截断式捕获，见 CappedSink）；
    // 单线程依次读会在对端写满管道时死锁。
    let mut child_stdout = child.stdout.take().expect("stdout 已 piped");
    let mut child_stderr = child.stderr.take().expect("stderr 已 piped");
    // 读者经 channel 回传缓冲：正常退出路径按 READER_DRAIN_GRACE 限时
    // 等待——孙进程握着管道写端等不到 EOF 时不永久阻塞（见下方分支）
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let stdout_reader = std::thread::spawn(move || {
        let mut sink = CappedSink { buf: Vec::new(), cap: max_output };
        let _ = std::io::copy(&mut child_stdout, &mut sink);
        let _ = stdout_tx.send(sink.buf);
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut sink = CappedSink { buf: Vec::new(), cap: max_output };
        let _ = std::io::copy(&mut child_stderr, &mut sink);
        let _ = stderr_tx.send(sink.buf);
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // 超时强杀整棵进程树：孙进程随树/组终止，管道写端全部
                    // 关闭，读取线程收 EOF 自然收尾
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(format!(
                        "命令 {program} 执行超过 {timeout:?} 超时，已强制终止"
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                kill_process_tree(&mut child);
                let _ = child.wait();
                return Err(format!("等待命令 {program} 退出失败: {e}"));
            }
        }
    };

    // 子进程正常退出但孙进程仍握管道写端时，读者等不到 EOF：限时等待，
    // 超宽限放弃 join——读者线程随管道最终关闭自行结束（detach），不阻塞返回
    let stdout = stdout_rx.recv_timeout(READER_DRAIN_GRACE).unwrap_or_default();
    let stderr = stderr_rx.recv_timeout(READER_DRAIN_GRACE).unwrap_or_default();
    drop(stdout_reader);
    drop(stderr_reader);

    Ok(SysExecResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code().unwrap_or(-1),
    })
}

/// 外部命令流式执行结果块：逐块透传 stdout 文本，最后一块 done=true。
/// 与 [`SysFetchChunk`] 同构——插件侧统一「逐块 + 终态」流式语义。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SysExecChunk {
    pub text: String,
    pub done: bool,
    pub exit_code: Option<i32>,
}

/// 流式执行外部命令：stdout 按**完整行**逐块经 `on_chunk` 回调（NDJSON 流
/// 工具如 codebuddy `--output-format stream-json` 的逐事件输出），进程退出
/// 后发 done=true 终态块。stderr 仍一次性捕获（诊断用，不入流）。
///
/// 与 [`exec_blocking`] 的差异：stdout 不等进程退出才返回，而是读到一行
/// 推一行——用于「边跑边上屏」的流式 AI CLI 场景。行缓冲以 `\n` 切分，
/// 末行无换行符在 EOF 时补推。超时/截断/杀进程树与 exec_with_limits 同口径。
pub fn exec_streaming_blocking<F>(
    program: &str,
    args: &[String],
    workdir: Option<&str>,
    on_chunk: F,
) -> Result<SysExecResult, String>
where
    F: FnMut(SysExecChunk) + Send + 'static,
{
    exec_streaming_with_limits(program, args, workdir, EXEC_TIMEOUT, EXEC_MAX_OUTPUT, on_chunk)
}

fn exec_streaming_with_limits<F>(
    program: &str,
    args: &[String],
    workdir: Option<&str>,
    timeout: Duration,
    max_output: usize,
    mut on_chunk: F,
) -> Result<SysExecResult, String>
where
    F: FnMut(SysExecChunk) + Send + 'static,
{
    #[cfg(target_os = "windows")]
    let program = resolve_program(program);

    let mut cmd = Command::new(&program);
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    #[cfg(target_os = "windows")]
    cmd.env("PATH", merged_path_env());

    if let Some(dir) = workdir {
        if !dir.is_empty() {
            cmd.current_dir(dir);
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动命令 {program} 失败: {e}"))?;

    let mut child_stdout = child.stdout.take().expect("stdout 已 piped");
    let mut child_stderr = child.stderr.take().expect("stderr 已 piped");

    // stdout 行缓冲读取线程：每读满一行（遇 \n）经 channel 推一块；EOF 补推
    // 末行残留。累计上限 max_output 截断（防 OOM），截断后仍排空管道防阻塞。
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ()>>();
    let stdout_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut line: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut buf = [0u8; 4096];
        loop {
            match child_stdout.read(&mut buf) {
                Ok(0) => {
                    if !line.is_empty() {
                        let _ = stdout_tx.send(Ok(line.clone()));
                    }
                    break;
                }
                Ok(n) => {
                    for &b in &buf[..n] {
                        total += 1;
                        if b == b'\n' {
                            let text = if total <= max_output { line.clone() } else { Vec::new() };
                            let _ = stdout_tx.send(Ok(text));
                            line.clear();
                        } else {
                            line.push(b);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let _ = stdout_tx.send(Err(())); // EOF 信号
    });

    // stderr 一次性捕获（诊断用）
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let stderr_reader = std::thread::spawn(move || {
        let mut sink = CappedSink { buf: Vec::new(), cap: max_output };
        let _ = std::io::copy(&mut child_stderr, &mut sink);
        let _ = stderr_tx.send(sink.buf);
    });

    // 主循环：收 stdout 行块并回调，直到进程退出且 EOF
    let deadline = std::time::Instant::now() + timeout;
    let mut stdout_eof = false;
    let mut status_opt: Option<std::process::ExitStatus> = None;
    loop {
        // 收一块 stdout（带短超时以便周期性检查退出/超时）
        match stdout_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(Ok(line)) => {
                let text = String::from_utf8_lossy(&line).into_owned();
                if !text.is_empty() {
                    on_chunk(SysExecChunk { text, done: false, exit_code: None });
                }
            }
            Ok(Err(())) => stdout_eof = true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => stdout_eof = true,
        }
        if status_opt.is_none() {
            match child.try_wait() {
                Ok(Some(s)) => status_opt = Some(s),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        kill_process_tree(&mut child);
                        let _ = child.wait();
                        let _ = stdout_reader.join();
                        let _ = stderr_reader.join();
                        return Err(format!("命令 {program} 执行超过 {timeout:?} 超时，已强制终止"));
                    }
                }
                Err(e) => {
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    return Err(format!("等待命令 {program} 退出失败: {e}"));
                }
            }
        }
        if status_opt.is_some() && stdout_eof {
            break;
        }
    }

    let status = status_opt.expect("退出状态已在循环内取得");
    let _ = stdout_reader.join();
    let stderr = stderr_rx.recv_timeout(READER_DRAIN_GRACE).unwrap_or_default();
    drop(stderr_reader);

    let exit_code = status.code().unwrap_or(-1);
    on_chunk(SysExecChunk { text: String::new(), done: true, exit_code: Some(exit_code) });

    Ok(SysExecResult {
        stdout: String::new(), // 流式模式全文经 on_chunk 推送，不回填
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code,
    })
}

/// 强杀子进程及其整棵进程树。孙进程可能继承 stdout/stderr 管道写端，
/// 只杀直接子进程（`Child::kill`）读者线程会永远等不到 EOF。
#[cfg(windows)]
fn kill_process_tree(child: &mut std::process::Child) {
    // taskkill /T /F 杀整棵进程树，零额外依赖；失败回退直接杀子进程
    let pid = child.id().to_string();
    let ok = Command::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = child.kill();
    }
}

/// Unix 变体：子进程 spawn 时已设为进程组组长（`process_group(0)`），
/// 负 pid 向整组发 SIGKILL，孙进程随组终止。
#[cfg(unix)]
fn kill_process_tree(child: &mut std::process::Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    // 兜底：子进程若已退出/未入组，确保直接子进程被回收
    let _ = child.kill();
}

#[cfg(not(any(windows, unix)))]
fn kill_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// 发起 HTTP 请求（async，透传 reqwest；代理由环境变量自行感知）。
///
/// 响应体上限 FETCH_MAX_BODY：先查 Content-Length，再按流式读取累计字节数
/// 截停（60s 超时挡不住慢速无限流）；超限返回错误。
pub async fn fetch(
    url: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
) -> Result<SysFetchResult, String> {
    fetch_bounded(url, method, headers, body, FETCH_MAX_BODY).await
}

async fn fetch_bounded(
    url: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
    max_body: usize,
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

    let mut response = req.send().await.map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status().as_u16();
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // 响应体上限：Content-Length 声明超限直接拒绝（免读流）
    if let Some(len) = response.content_length() {
        if len > max_body as u64 {
            return Err(format!(
                "响应体声明长度 {len} 字节，超过上限 {max_body} 字节"
            ));
        }
    }
    // 流式读取并按累计字节数截停（无 Content-Length 或声明不实的兜底）
    let mut buf = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?
    {
        if buf.len() + chunk.len() > max_body {
            return Err(format!("响应体超过大小上限 {max_body} 字节，已中止读取"));
        }
        buf.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&buf).into_owned();

    Ok(SysFetchResult {
        status,
        headers: resp_headers,
        body,
    })
}

/// 发起 HTTP 流式请求（async）。每个响应体文本块到达时调用 `chunk_cb`，
/// 最后一块 done=true 携带 status/headers 标记完成。
pub async fn fetch_stream<F>(
    url: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
    chunk_cb: F,
) -> Result<(), String>
where
    F: Fn(SysFetchChunk) + Send + 'static,
{
    fetch_stream_bounded(url, method, headers, body, FETCH_MAX_BODY, chunk_cb).await
}

/// 流式读取的空闲超时（两次 chunk 到达之间的最大间隔）。区别于非流式 fetch 的
/// 全程 FETCH_TIMEOUT——长流式（AI SSE 等）总时长可能远超 60s，全程超时会误伤；
/// 空闲超时只惩罚「卡住不再出数据」的慢流，正常慢速但持续出数据的流不受限。
/// 超限后不报错（由调用方按流结束语义处理），避免无限悬挂。
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 从字节缓冲切出「完整 UTF-8 前缀」，返回其字符串并把已切部分移出缓冲。
///
/// 流式 chunk 可能在多字节字符中间截断（如中文 3 字节跨块），逐块
/// `from_utf8_lossy` 会把未完成序列替换为 U+FFFD 乱码且持久化入档。此处按字节
/// 缓冲，`std::str::from_utf8` 的 `valid_up_to()` 恰好返回「最后一个完整字符的
/// 结束下标」——末尾不完整序列时截到其前，完整尾部留到下次与新块拼接后输出，
/// 从而跨块不断字（评审 Z4）。
fn drain_complete_utf8(buf: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(buf) {
        Ok(_) => buf.len(),
        Err(e) => e.valid_up_to(),
    };
    let text = String::from_utf8_lossy(&buf[..valid]).into_owned();
    buf.drain(..valid);
    text
}

async fn fetch_stream_bounded<F>(
    url: &str,
    method: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    body: Option<&str>,
    max_body: usize,
    chunk_cb: F,
) -> Result<(), String>
where
    F: Fn(SysFetchChunk) + Send + 'static,
{
    // 流式路径不设 reqwest 全程 timeout（长流式总时长可能远超 60s）；
    // 空闲超时在下方逐块读取处用 tokio::time::timeout 包裹实现。
    let client = reqwest::Client::builder()
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

    let mut response = req.send().await.map_err(|e| format!("HTTP 请求失败: {e}"))?;

    let status = response.status().as_u16();
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // Content-Length 上限检查
    if let Some(len) = response.content_length() {
        if len > max_body as u64 {
            return Err(format!(
                "响应体声明长度 {len} 字节，超过上限 {max_body} 字节"
            ));
        }
    }

    let mut total = 0usize;
    // 字节级缓冲：跨块拼完整 UTF-8 再切（Z4）
    let mut pending = Vec::new();
    loop {
        // 空闲超时包裹单次 chunk 读取：持续出数据的慢流不受限，卡住则中断
        let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, response.chunk())
            .await
            .map_err(|_| format!("响应流空闲超过 {STREAM_IDLE_TIMEOUT:?} 无数据，已中断"))?
            .map_err(|e| format!("读取响应体失败: {e}"))?;
        match next {
            Some(chunk) => {
                total += chunk.len();
                if total > max_body {
                    return Err(format!("响应体超过大小上限 {max_body} 字节，已中止读取"));
                }
                pending.extend_from_slice(&chunk);
                let text = drain_complete_utf8(&mut pending);
                if !text.is_empty() {
                    // 中间块不携带 headers（协议瘦身，仅 done 块带，评审 F6）
                    chunk_cb(SysFetchChunk {
                        text,
                        done: false,
                        status,
                        headers: HashMap::new(),
                    });
                }
            }
            None => {
                // 连接关闭即 EOF：冲刷尾部残存字节（若仍有不完整 UTF-8，lossy 兜底）
                let tail = drain_complete_utf8(&mut pending);
                if !tail.is_empty() {
                    chunk_cb(SysFetchChunk {
                        text: tail,
                        done: false,
                        status,
                        headers: HashMap::new(),
                    });
                }
                break;
            }
        }
    }

    // 发送完成标记：携带 status 与完整 headers
    chunk_cb(SysFetchChunk {
        text: String::new(),
        done: true,
        status,
        headers: resp_headers,
    });

    Ok(())
}

// ------------------------------------------------------------------
// 测试：exec 超时/输出上限、fetch 响应体上限（本机 std::net 微型 HTTP 服务）
// ------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;

    /// 以子进程形态重入本测试二进制并永久挂起（配合 --ignored 精确过滤）。
    #[test]
    #[ignore]
    fn hang_forever_helper() {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    /// 以子进程形态重入并向 stdout 持续输出（总量约 256KB，超出测试用小上限）。
    #[test]
    #[ignore]
    fn spew_stdout_helper() {
        let chunk = "x".repeat(4096);
        for _ in 0..64 {
            println!("{chunk}");
        }
    }

    fn self_exe() -> String {
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn run_helper(name: &str) -> Vec<String> {
        vec![
            format!("sys::tests::{name}"),
            "--ignored".into(),
            "--exact".into(),
            "--nocapture".into(),
        ]
    }

    #[test]
    fn exec_timeout_kills_hanging_child() {
        let start = Instant::now();
        let err = exec_with_limits(
            &self_exe(),
            &run_helper("hang_forever_helper"),
            None,
            Duration::from_millis(500),
            1024,
        )
        .err()
        .expect("挂起的子进程应报超时错误");
        assert!(err.contains("超时"), "应报超时错误，实际: {err}");
        // 强杀后应立即返回，而不是等满默认 60s 上限
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "超时强杀耗时异常: {:?}",
            start.elapsed()
        );
    }

    /// 以子进程形态重入：派生一个继承本进程 stdout 的挂起孙进程后自己
    /// 挂起。孙进程握住通向宿主的管道写端——只杀直接子进程时宿主读者
    /// 线程永远等不到 EOF。
    #[test]
    #[ignore]
    fn spawn_hanging_grandchild_helper() {
        let _grandchild = std::process::Command::new(self_exe())
            .args(run_helper("hang_forever_helper"))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    #[test]
    fn exec_timeout_kills_whole_process_tree() {
        let start = Instant::now();
        let err = exec_with_limits(
            &self_exe(),
            &run_helper("spawn_hanging_grandchild_helper"),
            None,
            Duration::from_millis(500),
            1024,
        )
        .err()
        .expect("挂起的进程树应报超时错误");
        assert!(err.contains("超时"), "应报超时错误，实际: {err}");
        // 孙进程持管道写端：若只杀直接子进程，reader join 将永久阻塞
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "进程树强杀耗时异常: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn exec_output_is_capped() {
        let cap = 16 * 1024;
        let r = exec_with_limits(
            &self_exe(),
            &run_helper("spew_stdout_helper"),
            None,
            Duration::from_secs(30),
            cap,
        )
        .unwrap();
        // 截断而非报错：子进程正常退出（管道持续排空，未阻塞）
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.stdout.len(), cap, "stdout 应截断至上限");
    }

    /// 以子进程形态重入：派生一个持管道 10s 的孙进程后立即正常退出
    /// （孙进程寿命 > READER_DRAIN_GRACE，覆盖正常退出路径的 EOF 等待）。
    #[test]
    #[ignore]
    fn spawn_grandchild_then_exit_helper() {
        let _grandchild = std::process::Command::new(self_exe())
            .args(run_helper("hold_pipe_briefly_helper"))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
    }

    /// 以孙进程形态重入：持管道 10s 后退出（有限寿命，不留残余进程）。
    #[test]
    #[ignore]
    fn hold_pipe_briefly_helper() {
        std::thread::sleep(Duration::from_secs(10));
    }

    #[test]
    fn exec_normal_exit_with_pipe_holding_grandchild_does_not_hang() {
        let start = Instant::now();
        let r = exec_with_limits(
            &self_exe(),
            &run_helper("spawn_grandchild_then_exit_helper"),
            None,
            Duration::from_secs(30),
            1024,
        )
        .expect("子进程正常退出应返回结果");
        assert_eq!(r.exit_code, 0);
        // 宽限 2s 后放弃等待读者，而非等孙进程 10s 退出
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "正常退出路径被孙进程拖住: {:?}",
            start.elapsed()
        );
    }

    /// 起一个一次性微型 HTTP 服务：写入给定响应头与 body 后关闭连接。
    fn spawn_http_server(head: &str, body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let head = head.to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_rejects_oversized_content_length() {
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
            Vec::new(),
        );
        let err = fetch_bounded(&url, None, None, None, 4096)
            .await
            .err()
            .expect("声明超限的 Content-Length 应报错");
        assert!(err.contains("上限"), "应报超限错误，实际: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_rejects_oversized_stream_without_content_length() {
        // 无 Content-Length（连接关闭即 EOF）：流式读取按累计字节截停
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
            vec![b'y'; 100 * 1024],
        );
        let err = fetch_bounded(&url, None, None, None, 4096)
            .await
            .err()
            .expect("累计超限的响应流应报错");
        assert!(err.contains("上限"), "应报超限错误，实际: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_small_body_passes() {
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
            b"hello".to_vec(),
        );
        let r = fetch_bounded(&url, None, None, None, 4096).await.unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "hello");
    }

    // ------------------------------------------------------------------
    // fetch_stream：分块回调 / done 终止 / UTF-8 跨块不断字 / 字节截停
    // ------------------------------------------------------------------

    /// 流式分块 server：写入响应头后分多次写 body，每次间延迟 delay_ms，
    /// 用于制造多个 chunk（chunk 边界由 TCP 段划分，非严格逐次）。
    fn spawn_stream_server(chunks: Vec<Vec<u8>>, delay_ms: u64) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                );
                for c in chunks {
                    let _ = stream.write_all(&c);
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
            }
        });
        format!("http://{addr}")
    }

    /// 收集 fetch_stream 回调产生的全部块
    async fn collect_stream(
        url: &str,
        max_body: usize,
    ) -> Result<Vec<SysFetchChunk>, String> {
        use std::sync::{Arc, Mutex};
        let chunks = Arc::new(Mutex::new(Vec::new()));
        {
            let chunks = Arc::clone(&chunks);
            fetch_stream_bounded(url, None, None, None, max_body, move |chunk| {
                chunks.lock().unwrap().push(chunk)
            })
            .await?;
        }
        // 闭包内的 Arc 引用已随 fetch_stream 结束而 drop，此处应可唯一取出；
        // Mutex 无并发持锁，PoisonError 也仅返回原锁内容
        let inner = match Arc::try_unwrap(chunks) {
            Ok(mutex) => mutex.into_inner().unwrap_or_else(|poison| poison.into_inner()),
            Err(_) => unreachable!("collect_stream 内不应残留 Arc 引用"),
        };
        Ok(inner)
    }

    #[test]
    fn drain_complete_utf8_keeps_cross_block_multibyte_char() {
        // 中文「你」= E4 BD A0，3 字节。模拟分块截断在多字节字符中间：
        // 首次只到第 1 字节，再次补 2 字节。跨块必须拼出完整字符，无 U+FFFD。
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0xE4]);
        assert_eq!(drain_complete_utf8(&mut buf), "", "不完整头部不应输出");
        assert_eq!(buf, vec![0xE4], "不完整头部应保留待拼接");
        buf.extend_from_slice(&[0xBD, 0xA0]);
        assert_eq!(drain_complete_utf8(&mut buf), "你", "跨块应拼出完整字符");
        assert!(buf.is_empty(), "已切分后缓冲应清空");
    }

    #[test]
    fn drain_complete_utf8_handles_ascii_and_tail() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"hello ");
        buf.extend_from_slice(&[0xE4, 0xBD]); // "你" 的前 2 字节
        assert_eq!(drain_complete_utf8(&mut buf), "hello ", "ASCII 前缀应输出");
        assert_eq!(buf, vec![0xE4, 0xBD], "不完整尾部应保留");
        buf.extend_from_slice(&[0xA0]);
        assert_eq!(drain_complete_utf8(&mut buf), "你");
        assert!(buf.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_stream_delivers_chunks_and_done() {
        let url = spawn_stream_server(
            vec![b"Hello".to_vec(), b", Spark".to_vec()],
            5,
        );
        let chunks = collect_stream(&url, 4096).await.expect("流式请求应成功");
        assert!(
            chunks.len() >= 3,
            "应有至少两个中间块 + 一个 done 块，实际 {}",
            chunks.len()
        );
        // 所有中间块 done=false，最后一块 done=true
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.done, i == chunks.len() - 1, "仅最后一块 done=true");
        }
        let body: String = chunks
            .iter()
            .filter(|c| !c.done)
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(body, "Hello, Spark");
        // 中间块不携带 headers（F6 协议瘦身），仅 done 块携带
        for c in chunks.iter().take(chunks.len() - 1) {
            assert!(c.headers.is_empty(), "中间块 headers 应为空");
        }
        assert!(!chunks.last().unwrap().headers.is_empty(), "done 块应携带 headers");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_stream_keeps_chinese_across_chunks() {
        // 分块输出完整中文（多次 write 制造多 chunk），累计不应出现 U+FFFD
        let url = spawn_stream_server(
            vec![b"\xe4\xbd".to_vec(), b"\xa0\xe5\xa5".to_vec(), b"\xbd".to_vec()],
            5,
        );
        let chunks = collect_stream(&url, 4096).await.expect("流式请求应成功");
        let body: String = chunks
            .iter()
            .filter(|c| !c.done)
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(body, "你好", "跨块中文不应出现乱码");
        assert!(!body.contains('\u{FFFD}'), "不应出现 U+FFFD 乱码");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_stream_rejects_oversized_body() {
        let url = spawn_stream_server(vec![vec![b'y'; 100 * 1024]], 2);
        let err = collect_stream(&url, 4096)
            .await
            .err()
            .expect("累计超限应报错");
        assert!(err.contains("上限"), "应报超限错误，实际: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_stream_content_length_rejects_oversized() {
        let url = spawn_http_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n",
            Vec::new(),
        );
        let err = collect_stream(&url, 4096).await.err().expect("声明超限应报错");
        assert!(err.contains("上限"), "应报超限错误，实际: {err}");
    }
}
