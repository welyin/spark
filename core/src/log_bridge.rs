//! log crate 的最小 stderr logger 桥接。
//!
//! Spark 内核未初始化任何 logger，`log::info!` / `log::warn!` 等宏在无 sink
//! 时是 no-op，DM 投递等链路的诊断日志被静默丢弃。本模块把 `log` crate 接到
//! stderr（`eprintln!` 透传，线程安全、无堆分配递归风险），使 Android logcat
//! 与 macOS stdout 都能看到现有所有 `log::*` 输出。level 从 `RUST_LOG` env 读，
//! 缺省 Info（保证现有 `log::info!` 全量可见）。
//!
//! 使用：在 P2P 节点启动处调用 [`init_logger`]（内部 `std::sync::Once` 保证只
//! 初始化一次，重复调用安全，不 panic）。

use std::sync::Once;

/// 极简 logger：`eprintln!` 透传，不格式化、无分配（宏已渲染好最终文本）。
struct StderrLogger {
    max_level: log::LevelFilter,
}

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.max_level
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// 初始化 stderr logger。level 取自 `RUST_LOG`（支持 off/error/warn/info/
/// debug/trace，大小写不敏感；含 `module=level` 形式时取其中最高级别），未设置
/// 或无法解析时默认 Info。只初始化一次，后续调用为 no-op。
pub fn init_logger() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let max_level = std::env::var("RUST_LOG")
            .ok()
            .map(|v| parse_level(&v))
            .unwrap_or_else(|| Some(log::LevelFilter::Info))
            .unwrap_or(log::LevelFilter::Info);
        let logger = Box::new(StderrLogger { max_level });
        // 无其他 logger 已注册时安装；已注册则保持现有（不覆盖宿主 logger）。
        let _ = log::set_logger(Box::leak(logger));
        log::set_max_level(max_level);
    });
}

/// 解析 `RUST_LOG`。缺省 `module=level` 形式可能含多个段，取所有可解析段中
/// 最高的级别（等价于无模块过滤时的全局级别）。
fn parse_level(value: &str) -> Option<log::LevelFilter> {
    value
        .split(',')
        .filter_map(|seg| {
            let (_, lvl) = seg.split_once('=').unwrap_or(("", seg));
            parse_single(lvl.trim())
        })
        .max()
}

fn parse_single(level: &str) -> Option<log::LevelFilter> {
    match level.to_ascii_lowercase().as_str() {
        "off" => Some(log::LevelFilter::Off),
        "error" => Some(log::LevelFilter::Error),
        "warn" => Some(log::LevelFilter::Warn),
        "info" => Some(log::LevelFilter::Info),
        "debug" => Some(log::LevelFilter::Debug),
        "trace" => Some(log::LevelFilter::Trace),
        _ => None,
    }
}
