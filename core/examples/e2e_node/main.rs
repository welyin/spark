//! e2e_node：内核级端到端测试驱动节点。
//!
//! 进程模型 = stdio JSON 行协议（每行一个 JSON 对象）：
//! - 启动参数：`--data-dir <path>`（必选）；
//! - 启动完成后 stdout 打一行 `{"ready":true}`；
//! - stdin 每行一个请求 `{"id":n,"cmd":"...", ...参数}`，响应一行
//!   `{"id":n,"ok":true,"data":...}` 或 `{"id":n,"ok":false,"error":"..."}`；
//! - p2p 事件异步吐出 `{"event":"<Kind>","data":...}`（Kind 为 P2pEvent 变体名）。
//!
//! Kernel API 为同步门面（内部 block_on），故主线程直接持有 Kernel 顺序处理
//! 命令；stdin 读取与事件接收各起一个线程，经 mpsc 汇入主循环。

mod contact;
mod dispatch;
mod identity;
mod message;
mod org;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use serde_json::{Value, json};
use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::p2p::{P2pConfig, P2pEvent};
use spark_core::p2p::node::system_now_ms;

/// 测试节点统一口令（脚本可经 `password` 参数覆盖）。
pub const DEFAULT_PASSWORD: &str = "e2e-password-123";

/// 主循环输入：stdin 行 / p2p 事件 / stdin 关闭。
enum Input {
    Line(String),
    Event(P2pEvent),
    StdinEof,
}

/// 单行输出（加锁防交错）。
pub fn print_line(value: &Value) {
    use std::io::Write;
    let text = serde_json::to_string(value).expect("e2e output serializable");
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{text}");
    let _ = lock.flush();
}

/// p2p 事件 → `{"event": kind, "data": ...}` 行（KeepaliveTick 太吵，过滤）。
fn event_line(event: &P2pEvent) -> Option<Value> {
    if matches!(event, P2pEvent::KeepaliveTick(_)) {
        return None;
    }
    let value = serde_json::to_value(event).expect("P2pEvent serializable");
    let kind = value.get("kind").cloned().unwrap_or(Value::Null);
    let mut line = json!({"event": kind});
    if let Some(data) = value.get("data") {
        line["data"] = data.clone();
    }
    Some(line)
}

fn parse_args() -> (PathBuf, u16) {
    let args: Vec<String> = std::env::args().collect();
    let mut data_dir: Option<PathBuf> = None;
    let mut preferred_port: u16 = 0;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                if let Some(value) = args.get(i + 1) {
                    data_dir = Some(PathBuf::from(value));
                    i += 1;
                }
            }
            // 首选监听端口（驱动脚本按节点钉死：p2p 重启后端口不变，
            // 对端已存寻址不失效；0 = OS 分配）
            "--preferred-port" => {
                if let Some(value) = args.get(i + 1) {
                    preferred_port = value.parse().unwrap_or(0);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    (
        data_dir.expect("usage: e2e_node --data-dir <path> [--preferred-port <port>]"),
        preferred_port,
    )
}

/// 单机测试网络配置：TCP only、无 mdns/upnp。
/// keepalive 1s：驱动组织反熵收敛（成员变更/资料更新经快照同步传播，
/// 与生产壳层的周期 tick 同机制）；tick 事件不吐出（太吵）。
fn test_p2p_config(preferred_port: u16) -> P2pConfig {
    P2pConfig {
        app_version: "0.0.0-e2e".to_string(),
        preferred_port: Some(preferred_port),
        port_scan: false,
        enable_tcp: true,
        enable_ws: false,
        enable_ipv6: false,
        enable_mdns: false,
        enable_upnp: false,
        keepalive_interval: Some(std::time::Duration::from_secs(1)),
        dht_mode: spark_core::p2p::DhtMode::Server,
        plugin_announce_pow_bits: None,
        plugin_announce_relay_tenure_ms: None,
        dht_republish_ticks: None,
        enable_relay_server: true,
        now_fn: Arc::new(system_now_ms),
    }
}

fn main() {
    let (data_dir, preferred_port) = parse_args();
    let config = KernelConfig {
        data_dir,
        app_version: "0.0.0-e2e".to_string(),
        p2p: Some(test_p2p_config(preferred_port)),
    };
    let mut kernel = Kernel::init(config).expect("kernel init");

    let (tx, rx) = mpsc::channel::<Input>();

    // stdin 读取线程：逐行转发；EOF 通知主循环关停
    let stdin_tx = tx.clone();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(text) => {
                    if stdin_tx.send(Input::Line(text)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = stdin_tx.send(Input::StdinEof);
    });

    // p2p 事件线程：broadcast 接收端阻塞读（同步上下文可用 blocking_recv）
    let mut events = kernel.subscribe_p2p_events();
    std::thread::spawn(move || loop {
        match events.blocking_recv() {
            Ok(event) => {
                if tx.send(Input::Event(event)).is_err() {
                    return;
                }
            }
            // 滞后丢弃继续；频道关闭即退出
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    });

    print_line(&json!({"ready": true}));

    loop {
        match rx.recv() {
            Ok(Input::Line(text)) => {
                if text.trim().is_empty() {
                    continue;
                }
                if dispatch::handle_line(&text, &mut kernel) {
                    break;
                }
            }
            Ok(Input::Event(event)) => {
                if let Some(line) = event_line(&event) {
                    print_line(&line);
                }
            }
            Ok(Input::StdinEof) | Err(_) => break,
        }
    }

    let _ = kernel.shutdown();
}
