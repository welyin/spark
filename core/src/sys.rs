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
}
