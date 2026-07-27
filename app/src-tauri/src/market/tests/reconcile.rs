//! 启动对账与 grantedPermissions 回填用例。

use super::*;

#[test]
fn reconcile_marks_verified_bundle_installed() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.initialize().unwrap();

    let installed = service.state.installed.get("weibo-core").unwrap();
    assert_eq!(installed.version, "0.1.0");
    assert!(installed.size > 0);
    assert!(installed.enabled);
    assert_eq!(
        installed.granted_permissions,
        vec!["storage:read", "storage:write", "org:read", "proof:verify", "org:sync"]
    );
    assert!(installed.package_path.contains("dist-market"));
    assert_eq!(service.update_probes["weibo-core"].reason, "bundled");

    // 状态已持久化
    let persisted = read_state_file(&fixture.state_file);
    assert!(persisted.installed.contains_key("weibo-core"));

    let items = service.list_market();
    assert_eq!(items.len(), 1);
    assert!(items[0].installed);
    assert_eq!(items[0].installed_version.as_deref(), Some("0.1.0"));
    assert_eq!(items[0].last_check_reason, "bundled");
}

#[test]
fn reconcile_skips_bad_signature_and_digest() {
    // 坏签名 → 不安装
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { bad_signature: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert!(!service.state.installed.contains_key("weibo-core"));
    assert!(!service.list_market()[0].installed);

    // sha256 对不上 → 不安装
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { tamper_sha256: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert!(!service.state.installed.contains_key("weibo-core"));
}

#[test]
fn reconcile_marks_dev_source_and_bundle_wins() {
    // 仅源码目录 → bundled-dev-source
    let fixture = Fixture::new();
    write_dev_source(&fixture);
    let mut service = fixture.service();
    service.initialize().unwrap();
    let installed = service.state.installed.get("weibo-core").unwrap();
    assert_eq!(installed.sha256, "bundled-dev-source");
    assert_eq!(installed.size, 0);
    assert_eq!(service.update_probes["weibo-core"].reason, "bundled-dev-source");

    // bundle + 源码同时存在 → bundle 优先
    let fixture = Fixture::new();
    write_dev_source(&fixture);
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.initialize().unwrap();
    let installed = service.state.installed.get("weibo-core").unwrap();
    assert_ne!(installed.sha256, "bundled-dev-source");
    assert_eq!(service.update_probes["weibo-core"].reason, "bundled");
}

#[test]
fn backfill_fills_missing_granted_permissions() {
    let fixture = Fixture::new();
    // 手写一份缺 grantedPermissions 字段的旧版状态
    let legacy = serde_json::json!({
        "installed": {
            "weibo-core": {
                "pluginId": "weibo-core",
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
    let installed = service.state.installed.get("weibo-core").unwrap();
    assert_eq!(
        installed.granted_permissions,
        vec!["storage:read", "storage:write", "org:read", "proof:verify", "org:sync"]
    );
    // 回填已落盘
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.installed["weibo-core"].granted_permissions.is_empty());
}
