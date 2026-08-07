//! 系统桥接命令（F4）：前端未读角标 → 系统徽标（dock/任务栏）。
//!
//! 桌面主目标 macOS 用 dock 角标（`set_badge_label`，仅 macOS 实现）；
//! 其余平台用 `set_badge_count`（Linux/iOS 支持；Tauri 文档注明 Windows
//! 需 overlay_icon、Android 不支持）。平台不支持或内部错误一律静默降级
//! （命令返回单位，错误不冒泡）——徽标是装饰性反馈，前端 fire-and-forget。

/// 未读数 → 徽标值：<=0 清除徽标（None），正数收敛到 1..=999（极端值不把
/// 大数字直接贴上 dock；前端 title 侧 >99 已显示「…」，徽标只是装饰性近似）。
#[cfg(not(target_os = "android"))]
fn badge_value(count: i64) -> Option<i64> {
    (count > 0).then_some(count.min(999))
}

#[tauri::command]
pub fn system_set_badge(window: tauri::Window, count: i64) {
    // Android 不支持徽标（Tauri 文档明示），相关方法在 Android target 不存在，
    // 编译期门控为 no-op；命令保留（前端 fire-and-forget 无需感知平台）。
    #[cfg(target_os = "android")]
    {
        let _ = (window, count);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window.set_badge_label(badge_value(count).map(|n| n.to_string()));
    }
    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    {
        let _ = window.set_badge_count(badge_value(count));
    }
}

// ------------------------------------------------------------------
// HTTP 代理设置：读写 <data_dir>/spark-proxy.json 并同步注入进程环境变量
// （持久化/校验/env 语义见 crate::proxy 模块注释）。
// ------------------------------------------------------------------

/// 读取当前代理配置：未设置（或文件损坏）返回 None。
#[tauri::command]
pub fn system_get_proxy(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    Ok(crate::proxy::load_proxy(&data_dir))
}

/// 退出应用（Android 前端改造：系统返回键在一级页时由前端调用）。
///
/// 背景：Android 原生层（AppPlugin）一旦存在 JS 返回键监听，按返回键只发事件、
/// 不再执行默认退出；`plugin:app|exit` 又不在 core:app 的 ACL 命令清单内（前端
/// 无法调用）。因此退出路径必须走应用自定义命令（自定义命令不受插件 ACL 限制）。
/// 桌面端不会触发该路径（无系统返回键事件），命令保留同理静默可用。
///
/// Android 语义为「退到后台保活」而非杀进程：我们是 P2P 应用，进程死了就掉线，
/// 对标微信按 Home 键的行为。经 tao ndk_glue 拿主 Activity 调 moveTaskToBack(true)。
/// 不用 app.exit(0)：它走 std::process::exit → __cxa_finalize → WebView GL 析构
/// 时 Adreno 锁已销毁的互斥锁，FORTIFY abort（已实测崩溃弹窗）。
#[tauri::command]
pub fn system_exit_app(app: tauri::AppHandle) {
    #[cfg(target_os = "android")]
    {
        let _ = app; // Android 路径经 JNI，不使用 AppHandle
        if !crate::android_activity::move_task_to_back() {
            eprintln!("[system] moveTaskToBack unavailable, killing process");
            unsafe { libc::raise(libc::SIGKILL) };
        }
    }
    #[cfg(not(target_os = "android"))]
    app.exit(0);
}

/// 设置代理：空串=关闭；否则须为 host:port（校验见 proxy::validate_proxy）。
/// 保存后立即更新环境变量——后续新建的 reqwest 客户端生效；市场 OnceLock
/// 客户端与 updater 客户端等已建立连接不追溯，需重启应用（前端已提示）。
#[tauri::command]
pub fn system_set_proxy(app: tauri::AppHandle, proxy: String) -> Result<(), String> {
    let validated = crate::proxy::validate_proxy(&proxy)?;
    let data_dir = crate::resolve_data_dir(&app).map_err(|e| e.to_string())?;
    crate::proxy::save_proxy(&data_dir, validated.as_deref())?;
    crate::proxy::apply_proxy_env(validated.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::badge_value;

    #[test]
    fn positive_count_maps_to_value() {
        assert_eq!(badge_value(1), Some(1));
        assert_eq!(badge_value(100), Some(100));
    }

    #[test]
    fn huge_count_clamps_to_cap() {
        assert_eq!(badge_value(999), Some(999));
        assert_eq!(badge_value(1000), Some(999));
        assert_eq!(badge_value(i64::MAX), Some(999));
    }

    #[test]
    fn non_positive_count_clears_badge() {
        assert_eq!(badge_value(0), None);
        assert_eq!(badge_value(-3), None);
    }
}
