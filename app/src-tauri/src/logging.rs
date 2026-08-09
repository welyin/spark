//! 全局 `log` 门面注册：Android 写 logcat（`__android_log_write`，tag
//! `spark-rust`），桌面写 stderr。filter 级别 Info 起步。
//!
//! 零新增专用依赖：`log` crate 已在依赖树（libp2p 传递依赖，锁定 0.4.x），
//! Android 的 `__android_log_write` 直接经 `libc`（Android 既有依赖）声明
//! extern 调 liblog.so，不引入 android_logger / tauri-plugin-log。
//!
//! 注册后内核依赖链（libp2p 等）经 `log` 门面的日志在 logcat/stderr 可见；
//! 内核自身的 `eprintln!` 走 stderr（Android 上不可见），改 `log` 统一做后续。

use log::{Level, LevelFilter, Log, Metadata, Record};

/// logcat 标签（`adb logcat -s spark-rust` 过滤）。
#[cfg(target_os = "android")]
const LOGCAT_TAG: &std::ffi::CStr = c"spark-rust";

struct ShellLogger;

impl Log for ShellLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} [{}] {}", record.level(), record.target(), record.args());
        #[cfg(target_os = "android")]
        android_log_write(record.level(), &line);
        #[cfg(not(target_os = "android"))]
        eprintln!("{line}");
    }

    fn flush(&self) {}
}

/// Android logcat 优先级（`android/log.h` 的 android_LogPriority）。
#[cfg(target_os = "android")]
fn android_log_write(level: Level, line: &str) {
    use std::os::raw::{c_char, c_int};

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    let prio = match level {
        Level::Error => 6, // ANDROID_LOG_ERROR
        Level::Warn => 5,  // ANDROID_LOG_WARN
        Level::Info => 4,  // ANDROID_LOG_INFO
        Level::Debug => 3, // ANDROID_LOG_DEBUG
        Level::Trace => 2, // ANDROID_LOG_VERBOSE
    };
    // 消息含 NUL 时截断（日志内容来自格式化串，正常不含内嵌 NUL）。
    let bytes = line.as_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let mut buf = bytes[..end].to_vec();
    buf.push(0);
    unsafe {
        __android_log_write(
            prio,
            LOGCAT_TAG.as_ptr(),
            buf.as_ptr() as *const c_char,
        );
    }
}

static LOGGER: ShellLogger = ShellLogger;

/// 注册全局 logger（幂等；重复注册静默忽略）。`run()` 入口最前调用。
pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Info);
}
