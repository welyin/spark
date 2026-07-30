//! 系统桥接命令（F4）：前端未读角标 → 系统徽标（dock/任务栏）。
//!
//! 桌面主目标 macOS 用 dock 角标（`set_badge_label`，仅 macOS 实现）；
//! 其余平台用 `set_badge_count`（Linux/iOS 支持；Tauri 文档注明 Windows
//! 需 overlay_icon、Android 不支持）。平台不支持或内部错误一律静默降级
//! （命令返回单位，错误不冒泡）——徽标是装饰性反馈，前端 fire-and-forget。

/// 未读数 → 徽标值：<=0 清除徽标（None），正数收敛到 1..=999（极端值不把
/// 大数字直接贴上 dock；前端 title 侧 >99 已显示「…」，徽标只是装饰性近似）。
fn badge_value(count: i64) -> Option<i64> {
    (count > 0).then_some(count.min(999))
}

#[tauri::command]
pub fn system_set_badge(window: tauri::Window, count: i64) {
    #[cfg(target_os = "macos")]
    let result = window.set_badge_label(badge_value(count).map(|n| n.to_string()));
    #[cfg(not(target_os = "macos"))]
    let result = window.set_badge_count(badge_value(count));
    let _ = result;
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
