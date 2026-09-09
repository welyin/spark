//! 桌面数据目录布局（identity.md §4.3，A7）。
//!
//! - 新布局：OS 用户目录下 `spark`（Windows `%LOCALAPPDATA%\spark`，
//!   macOS `~/Library/Application Support/spark`，Linux `~/.local/share/spark`）。
//!   一个 OS 账号即一个逻辑设备（各自实例、各自数据目录）；隔离强度 =
//!   OS 文件权限——本机静态纵深防御，**不是加密替代品**（UI/文档如实标注）。
//! - 旧布局：Tauri `app_data_dir`（带应用标识符，如 `%APPDATA%\com.spark.desktop`）。
//! - 启动一次性迁移：旧布局有数据且新布局未启用 → 移动 → 写布局标记 +
//!   原位置留迁移说明；移动失败回退旧布局（由调用方提示）。
//! - 移动端：OS 应用沙箱天然满足，不走本模块。

use std::path::{Path, PathBuf};

/// 新布局标记文件（位于新数据根内）：已迁移/已启用新布局。
pub const LAYOUT_MARKER: &str = ".spark-layout-v2";
/// 旧位置遗留的迁移说明文件名。
pub const MIGRATION_NOTE: &str = "MIGRATED.txt";

/// 平台新布局根目录（桌面；移动端沙箱天然满足，调用方不走此路径）。
pub fn platform_spark_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("spark"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join("Library")
                .join("Application Support")
                .join("spark")
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local").join("share").join("spark"))
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(target_os = "macos"))
    )))]
    {
        None
    }
}

/// 迁移判定结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// 新布局已就位（此前已迁移或新装）。
    AlreadyNew,
    /// 旧布局无数据（新装/已迁移过），直接启用新布局。
    NoLegacyData,
    /// 旧布局数据已移动到新布局（含标记与迁移说明）。
    Moved,
    /// 移动失败，回退旧布局（错误描述供提示）。
    Failed(String),
}

/// 旧布局是否持有数据（存在、非空、且不是只剩迁移说明）。
fn legacy_has_data(legacy: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(legacy) else {
        return false;
    };
    rd.flatten()
        .any(|entry| entry.file_name() != MIGRATION_NOTE)
}

/// 移动目录整体（同卷 rename，跨卷回退 copy+remove）。
fn move_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_recursive(src, dst)?;
    std::fs::remove_dir_all(src)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn write_marker(new: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(new)?;
    std::fs::write(
        new.join(LAYOUT_MARKER),
        "spark layout v2（OS 用户目录；隔离强度 = OS 文件权限，不是加密替代品）\n",
    )
}

/// 一次性迁移：旧布局数据 → 新布局。返回（生效目录, 结果）。
///
/// 幂等：成功后新布局含 [`LAYOUT_MARKER`]、旧位置只留 [`MIGRATION_NOTE`]
/// 说明文件，后续启动走 `AlreadyNew` 分支不再移动。
pub fn migrate_once(legacy: &Path, new: &Path) -> (PathBuf, MigrateOutcome) {
    if new.join(LAYOUT_MARKER).exists() {
        return (new.to_path_buf(), MigrateOutcome::AlreadyNew);
    }
    if new.exists() {
        // 新路径已存在但未打标：若旧布局仍有数据 = 此前迁移中断（两边都有
        // 数据，自动合并风险高）→ 回退旧布局并如实上报，待人工处理。
        if legacy_has_data(legacy) {
            return (
                legacy.to_path_buf(),
                MigrateOutcome::Failed(
                    "检测到新旧两处数据目录均有内容（疑似迁移中断），已回退旧目录；请人工合并后删除旧目录".to_string(),
                ),
            );
        }
        return match write_marker(new) {
            Ok(()) => (new.to_path_buf(), MigrateOutcome::AlreadyNew),
            Err(e) => (
                legacy.to_path_buf(),
                MigrateOutcome::Failed(format!("写入布局标记失败: {e}")),
            ),
        };
    }
    if !legacy_has_data(legacy) {
        return match std::fs::create_dir_all(new).and_then(|()| write_marker(new)) {
            Ok(()) => (new.to_path_buf(), MigrateOutcome::NoLegacyData),
            Err(e) => (
                legacy.to_path_buf(),
                MigrateOutcome::Failed(format!("创建新布局目录失败: {e}")),
            ),
        };
    }
    if let Err(e) = move_dir(legacy, new) {
        return (
            legacy.to_path_buf(),
            MigrateOutcome::Failed(format!(
                "数据目录迁移失败（{} → {}）: {e}",
                legacy.display(),
                new.display()
            )),
        );
    }
    if let Err(e) = write_marker(new) {
        // 数据已移动成功，不回退；但标记缺失会导致下轮重判，如实上报。
        return (
            new.to_path_buf(),
            MigrateOutcome::Failed(format!("写入布局标记失败: {e}")),
        );
    }
    // 原位置留迁移说明（§4.3「移动 + 原位置留迁移说明」）。
    let _ = std::fs::create_dir_all(legacy);
    let _ = std::fs::write(
        legacy.join(MIGRATION_NOTE),
        format!(
            "Spark 数据目录已迁移至新布局：{}\n此目录仅保留本说明，可安全删除。\n",
            new.display()
        ),
    );
    (new.to_path_buf(), MigrateOutcome::Moved)
}

/// 桌面数据目录解析（纯函数，测试注入路径）：优先新布局，必要时一次性迁移。
pub fn resolve_desktop(legacy: &Path, new: &Path) -> (PathBuf, MigrateOutcome) {
    if new == legacy {
        return (legacy.to_path_buf(), MigrateOutcome::AlreadyNew);
    }
    migrate_once(legacy, new)
}

/// 进程级解析缓存：setup 与 plugin:// 协议回调共用，保证同一进程只判定
/// 一次（失败不每请求重试移动）。
static RESOLVED: std::sync::OnceLock<(PathBuf, MigrateOutcome)> = std::sync::OnceLock::new();

/// 桌面数据目录解析（带缓存）：`legacy` = Tauri `app_data_dir`（旧布局）。
/// 平台目录不可解析时维持旧布局。
pub fn resolve_desktop_cached(legacy: &Path) -> &'static (PathBuf, MigrateOutcome) {
    RESOLVED.get_or_init(|| match platform_spark_dir() {
        Some(new) => resolve_desktop(legacy, &new),
        None => (legacy.to_path_buf(), MigrateOutcome::AlreadyNew),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// 新装：旧布局无数据 → 启用新布局并打标。
    #[test]
    fn fresh_install_uses_new_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let new = tmp.path().join("new");
        let (dir, outcome) = resolve_desktop(&legacy, &new);
        assert_eq!(outcome, MigrateOutcome::NoLegacyData);
        assert_eq!(dir, new);
        assert!(new.join(LAYOUT_MARKER).exists(), "新布局打标");
    }

    /// 存量迁移：旧布局有数据 → 整体移动 + 新布局打标 + 原位置留说明。
    #[test]
    fn legacy_data_is_moved_with_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let new = tmp.path().join("new");
        write_file(&legacy.join("data"), "spark.sqlite", "db-bytes");
        write_file(&legacy, "root-abc.json", "identity-file");

        let (dir, outcome) = resolve_desktop(&legacy, &new);
        assert_eq!(outcome, MigrateOutcome::Moved);
        assert_eq!(dir, new);
        assert_eq!(
            std::fs::read_to_string(new.join("data").join("spark.sqlite")).unwrap(),
            "db-bytes",
            "内容完整移动"
        );
        assert!(new.join("root-abc.json").exists());
        assert!(new.join(LAYOUT_MARKER).exists(), "写布局标记");
        // 原位置只剩迁移说明。
        let note = std::fs::read_to_string(legacy.join(MIGRATION_NOTE)).unwrap();
        assert!(note.contains(&new.display().to_string()), "说明指向新位置");
        assert!(
            !legacy_has_data(&legacy),
            "旧位置只剩说明文件（下轮不再触发迁移）"
        );

        // 幂等：再次解析 → AlreadyNew，不再移动。
        let (dir2, outcome2) = resolve_desktop(&legacy, &new);
        assert_eq!(outcome2, MigrateOutcome::AlreadyNew);
        assert_eq!(dir2, new);
    }

    /// 迁移中断形态（新目录存在但未打标、旧目录仍有数据）→ 回退旧布局 + Failed。
    #[test]
    fn interrupted_migration_falls_back_to_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let new = tmp.path().join("new");
        write_file(&legacy, "root-abc.json", "identity-file");
        write_file(&new, "partial.tmp", "half-moved");

        let (dir, outcome) = resolve_desktop(&legacy, &new);
        assert_eq!(dir, legacy, "回退旧布局");
        assert!(
            matches!(outcome, MigrateOutcome::Failed(ref msg) if msg.contains("迁移中断")),
            "回退并给出可读原因: {outcome:?}"
        );
        assert!(legacy.join("root-abc.json").exists(), "旧数据未动");
    }

    /// 新布局已打标 → 直接用新，旧位置内容不再关心。
    #[test]
    fn marked_new_layout_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let new = tmp.path().join("new");
        write_file(&new, LAYOUT_MARKER, "v2");

        let (dir, outcome) = resolve_desktop(&legacy, &new);
        assert_eq!(outcome, MigrateOutcome::AlreadyNew);
        assert_eq!(dir, new);
    }

    /// 平台新布局解析：桌面平台可得且以 spark 结尾。
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn platform_dir_ends_with_spark() {
        let dir = platform_spark_dir().expect("桌面平台可解析");
        assert_eq!(dir.file_name().unwrap(), "spark");
    }
}
