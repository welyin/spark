//! 卸载用例：删记录 + 删包文件（限 app_data 插件目录内）；
//! dev-source / 目录外 packagePath 只删记录不动文件；非法 id 拒绝。

use super::*;

#[test]
fn uninstall_removes_record_probe_and_package_files() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.install("spark-example").unwrap();
    let package = fixture
        .packages_root
        .join("spark-example/packages/spark-plugin-spark-example-0.1.0.spkg");
    assert!(package.is_file());

    service.uninstall("spark-example").unwrap();
    assert!(!service.state.installed.contains_key("spark-example"));
    assert!(!service.update_probes.contains_key("spark-example"));
    // 包文件与插件目录整体清理
    assert!(!package.exists());
    assert!(!fixture.packages_root.join("spark-example").exists());

    // 持久化语义：状态文件里记录已移除（不经 initialize：对账会把本地
    // bundle 重新登记为已安装，属既有 reconcile 语义，与卸载解耦断言）
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.installed.contains_key("spark-example"));
}

#[test]
fn uninstall_dev_source_removes_record_only() {
    let fixture = Fixture::new();
    write_dev_source(&fixture);
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(service.state.installed["spark-example"].sha256, "bundled-dev-source");

    service.uninstall("spark-example").unwrap();
    assert!(!service.state.installed.contains_key("spark-example"));
    assert!(!service.update_probes.contains_key("spark-example"));
    // 源码目录是开发者的代码，绝不可动（reconcile 下次启动会重新登记）
    assert!(fixture.source_root.join("spark-example/manifest.ts").is_file());
}

#[test]
fn uninstall_rejects_invalid_id_and_missing_plugin() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.install("spark-example").unwrap();

    // 非法 id：段校验与 sideload/repo 同规则（拒空段/穿越段/非法字符）
    for bad in ["", "../evil", "spark-example/..", "UPPER", "a b"] {
        assert_eq!(
            service.uninstall(bad).unwrap_err(),
            format!("Plugin id invalid: {bad}")
        );
    }
    // 负向用例不影响既有记录与文件
    assert!(service.state.installed.contains_key("spark-example"));

    // 未安装
    assert_eq!(
        service.uninstall("nope").unwrap_err(),
        "Plugin is not installed: nope"
    );
}

#[test]
fn uninstall_keeps_package_path_outside_plugins_dir() {
    let fixture = Fixture::new();
    // 构造 packagePath 指向 app_data 插件目录外的记录（如本地发布目录产物）
    let outside_dir = fixture.release_dir();
    fs::create_dir_all(&outside_dir).unwrap();
    let outside_pkg = outside_dir.join("keep.spkg");
    fs::write(&outside_pkg, b"keep").unwrap();

    let mut service = fixture.service();
    service.state.installed.insert(
        "spark-example".to_string(),
        InstalledPluginState {
            plugin_id: "spark-example".to_string(),
            version: "0.1.0".to_string(),
            package_path: outside_pkg.to_string_lossy().to_string(),
            sha256: "00".repeat(32),
            size: 4,
            installed_at: 1,
            enabled: true,
            granted_permissions: vec![],
            trust: None,
        },
    );
    service.persist().unwrap();

    service.uninstall("spark-example").unwrap();
    assert!(!service.state.installed.contains_key("spark-example"));
    // 目录外文件一律不动，仅移除记录
    assert!(outside_pkg.is_file());
}
