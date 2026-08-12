//! 启动回填用例。
//!
//! 解耦（plugin_decoupling.md §4.3）：启动不再对账内置 bundle / 源码目录
//! （reconcile_bundled_installed_state 已删除）——已安装状态只由 .spkg 落盘文件 +
//! plugin-market-state.json 决定。此文件保留「兼容旧版安装态」回填用例
//! （grantedPermissions / supportedSpaces）与旧状态文件反序列化回归。

use super::*;

#[test]
fn backfill_fills_missing_granted_permissions() {
    let fixture = Fixture::new();
    // 手写一份缺 grantedPermissions 字段的旧版状态
    let legacy = serde_json::json!({
        "installed": {
            "spark-example": {
                "pluginId": "spark-example",
                "version": "0.1.0",
                "packagePath": "/tmp/x.spkg",
                "sha256": "aa",
                "size": 1,
                "installedAt": 1,
                "enabled": true
            }
        }
    });
    fs::create_dir_all(fixture.state_file.parent().unwrap()).unwrap();
    fs::write(&fixture.state_file, legacy.to_string()).unwrap();

    let mut service = fixture.service();
    service.initialize().unwrap();
    let installed = service.state.installed.get("spark-example").unwrap();
    // 解耦后无内置目录回填分支：统一按基础权限兜底
    assert_eq!(installed.granted_permissions, super::permissions::basic_permissions());
    // 回填已落盘
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.installed["spark-example"].granted_permissions.is_empty());
}

#[test]
fn legacy_state_without_supported_spaces_deserializes() {
    // 旧版状态文件回归（spaces-and-plugins §4）：supportedSpaces 字段后落地，
    // 旧记录缺该字段——serde default → None（按未声明 ["org"] 口径），不得反序列化失败
    let fixture = Fixture::new();
    let legacy = serde_json::json!({
        "installed": {
            "todo-local": {
                "pluginId": "todo-local",
                "version": "1.0.0",
                "packagePath": "/tmp/x.spkg",
                "sha256": "aa",
                "size": 1,
                "installedAt": 1,
                "enabled": true,
                "grantedPermissions": ["storage:read"],
                "trust": "sideloaded"
            }
        }
    });
    fs::create_dir_all(fixture.state_file.parent().unwrap()).unwrap();
    fs::write(&fixture.state_file, legacy.to_string()).unwrap();

    let persisted = read_state_file(&fixture.state_file);
    assert_eq!(persisted.installed["todo-local"].supported_spaces, None);
    assert_eq!(persisted.installed["todo-local"].trust.as_deref(), Some("sideloaded"));
}

#[test]
fn initialize_preserves_only_persisted_installed_state() {
    // 解耦后 initialize 不再扫描源码树/本地发布目录：无落盘状态时市场为空，
    // 不凭空出现任何插件。
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    write_dev_source(&fixture);
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert!(service.state.installed.is_empty());
    assert!(service.list_market().is_empty());
}
