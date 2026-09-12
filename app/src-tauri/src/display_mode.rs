//! 主窗口显示模式（系统设置「显示模式」）：只保留两种形态——
//! **窗口模式（最大化锁定）** 与 **全屏**，不允许还原成可拖拉的小窗。
//!
//! - 窗口模式：`set_fullscreen(false)` + `maximize()`，并由 lib.rs 的
//!   `RunEvent::WindowEvent::Resized` 监听兜底锁定——任何脱离最大化的通路
//!   （双击标题栏还原、Windows 从最大化拖标题栏还原、Win+方向键还原/贴靠）
//!   都会触发 Resized，监听发现「非最小化 && 非最大化 && 非全屏」立即重新
//!   `maximize()`（重新最大化会再触发一次 Resized，守卫条件不再满足，无循环）；
//! - 全屏：`set_fullscreen(true)`；同一监听在全屏模式下发现被 OS 层退出全屏
//!   （如 macOS 绿灯 / Ctrl+Cmd+F）时重新拉起，保证只存在两种模式。
//!
//! 持久化：`<data_dir>/spark-display-mode.json`，形如 `{"mode":"windowed"}`；
//! 口径与 proxy.rs 一致（缺失/损坏按默认 windowed）。仅桌面端有窗口语义，
//! 移动端恒 windowed（命令保留、设置项不渲染）。

use std::path::Path;
use std::sync::Mutex;

/// 显示模式配置文件名（data_dir 下）。
const DISPLAY_MODE_FILE_NAME: &str = "spark-display-mode.json";

/// 窗口标签（tauri.conf.json 主窗口）。
pub(crate) const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DisplayMode {
    /// 窗口模式＝最大化锁定
    Windowed,
    /// 全屏
    Fullscreen,
}

impl Default for DisplayMode {
    fn default() -> Self {
        Self::Windowed
    }
}

impl DisplayMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Windowed => "windowed",
            Self::Fullscreen => "fullscreen",
        }
    }
}

/// 当前显示模式（setup 载入持久化值后 manage；命令层读写，事件锁定只读）。
pub(crate) struct DisplayModeState(pub Mutex<DisplayMode>);

/// 入参解析：仅接受 windowed / fullscreen。
pub(crate) fn parse_mode(input: &str) -> Result<DisplayMode, String> {
    match input.trim() {
        "windowed" => Ok(DisplayMode::Windowed),
        "fullscreen" => Ok(DisplayMode::Fullscreen),
        other => Err(format!("显示模式无效：{other}（应为 windowed / fullscreen）")),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DisplayModeFile {
    mode: Option<String>,
}

/// 读持久化模式；文件缺失、损坏或内容非法一律按默认 windowed（不阻断启动）。
pub(crate) fn load_display_mode(data_dir: &Path) -> DisplayMode {
    let Ok(content) = std::fs::read_to_string(data_dir.join(DISPLAY_MODE_FILE_NAME)) else {
        return DisplayMode::default();
    };
    let Ok(file) = serde_json::from_str::<DisplayModeFile>(&content) else {
        return DisplayMode::default();
    };
    file.mode
        .as_deref()
        .and_then(|raw| parse_mode(raw).ok())
        .unwrap_or_default()
}

pub(crate) fn save_display_mode(data_dir: &Path, mode: DisplayMode) -> Result<(), String> {
    let file = DisplayModeFile {
        mode: Some(mode.as_str().to_string()),
    };
    let content = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(data_dir.join(DISPLAY_MODE_FILE_NAME), content).map_err(|e| e.to_string())
}

/// 应用显示模式到窗口（桌面端）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn apply_display_mode(window: &tauri::WebviewWindow, mode: DisplayMode) {
    match mode {
        DisplayMode::Fullscreen => {
            let _ = window.set_fullscreen(true);
        }
        DisplayMode::Windowed => {
            let _ = window.set_fullscreen(false);
            let _ = window.maximize();
        }
    }
}

/// 窗口事件锁定（桌面端）：窗口模式下脱离最大化且非最小化 → 重新最大化；
/// 全屏模式下被 OS 退出全屏 → 重新拉起全屏。返回是否触发了矫正（供测试/日志）。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn enforce_display_mode(window: &tauri::WebviewWindow, mode: DisplayMode) -> bool {
    match mode {
        DisplayMode::Windowed => {
            let minimized = window.is_minimized().unwrap_or(false);
            let maximized = window.is_maximized().unwrap_or(true);
            let fullscreen = window.is_fullscreen().unwrap_or(false);
            // 最小化是合法形态（窗口模式仍允许最小化到任务栏），不矫正；
            // 全屏过渡帧（切模式途中）不矫正，由 apply 目标态收敛。
            if !minimized && !maximized && !fullscreen {
                let _ = window.maximize();
                return true;
            }
            false
        }
        DisplayMode::Fullscreen => {
            if !window.is_fullscreen().unwrap_or(true) {
                let _ = window.set_fullscreen(true);
                return true;
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_accepts_two_modes() {
        assert_eq!(parse_mode("windowed"), Ok(DisplayMode::Windowed));
        assert_eq!(parse_mode("fullscreen"), Ok(DisplayMode::Fullscreen));
        assert_eq!(parse_mode(" fullscreen "), Ok(DisplayMode::Fullscreen));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(parse_mode("").is_err());
        assert!(parse_mode("maximized").is_err());
        assert!(parse_mode("Windowed").is_err());
    }

    #[test]
    fn load_missing_or_broken_file_defaults_windowed() {
        let dir = std::env::temp_dir().join(format!("spark-dm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_display_mode(&dir), DisplayMode::Windowed);
        let mut f = std::fs::File::create(dir.join(DISPLAY_MODE_FILE_NAME)).unwrap();
        f.write_all(b"not-json").unwrap();
        drop(f);
        assert_eq!(load_display_mode(&dir), DisplayMode::Windowed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!("spark-dm-test-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save_display_mode(&dir, DisplayMode::Fullscreen).unwrap();
        assert_eq!(load_display_mode(&dir), DisplayMode::Fullscreen);
        save_display_mode(&dir, DisplayMode::Windowed).unwrap();
        assert_eq!(load_display_mode(&dir), DisplayMode::Windowed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_unknown_persisted_value() {
        let dir = std::env::temp_dir().join(format!("spark-dm-test-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(DISPLAY_MODE_FILE_NAME), r#"{"mode":"mini"}"#).unwrap();
        assert_eq!(load_display_mode(&dir), DisplayMode::Windowed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
