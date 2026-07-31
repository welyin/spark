//! 卸载用例：删记录 + 删包文件（限 app_data 插件目录内）；
//! dev-source / 目录外 packagePath 只删记录不动文件；非法 id 拒绝；
//! 卸载墓碑（uninstalled）阻止对账复活，显式安装清除墓碑。

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

    // 持久化语义：状态文件里记录已移除、墓碑已写入
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.installed.contains_key("spark-example"));
    assert!(persisted.uninstalled.contains("spark-example"));

    // 二次卸载：记录已不在，返回 not installed
    assert_eq!(
        service.uninstall("spark-example").unwrap_err(),
        "Plugin is not installed: spark-example"
    );
}

#[test]
fn uninstall_tombstone_blocks_reconcile_and_install_clears_it() {
    // 卸载 → 重新 initialize：墓碑阻止对账把本地 bundle 重新登记（不复活）
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert!(service.state.installed.contains_key("spark-example"));

    service.uninstall("spark-example").unwrap();
    assert!(service.state.uninstalled.contains("spark-example"));

    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(!reloaded.state.installed.contains_key("spark-example"));
    assert!(!reloaded.update_probes.contains_key("spark-example"));
    assert!(!reloaded.list_market()[0].installed);
    // 墓碑持久化（新实例从状态文件读得）
    assert!(reloaded.state.uninstalled.contains("spark-example"));

    // 显式安装清除墓碑：再 initialize 正常登记（同 bundle 对账口径）
    reloaded.install("spark-example").unwrap();
    assert!(!reloaded.state.uninstalled.contains("spark-example"));
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.uninstalled.contains("spark-example"));

    let mut reloaded2 = fixture.service();
    reloaded2.initialize().unwrap();
    assert!(reloaded2.state.installed.contains_key("spark-example"));
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
    // 源码目录是开发者的代码，绝不可动
    assert!(fixture.source_root.join("spark-example/manifest.ts").is_file());

    // 墓碑阻止 dev-source 登记分支复活（重新 initialize 不再出现）
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(!reloaded.state.installed.contains_key("spark-example"));
}

#[test]
fn uninstall_rejects_invalid_id_and_missing_plugin() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.install("spark-example").unwrap();

    // 非法 id：段校验与 sideload/repo 同规则（拒空段/穿越段/非法字符/Windows 保留名）
    for bad in ["", "../evil", "spark-example/..", "UPPER", "a b", "con", "github.com/nul/x"] {
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
            supported_spaces: None,
        },
    );
    service.persist().unwrap();

    service.uninstall("spark-example").unwrap();
    assert!(!service.state.installed.contains_key("spark-example"));
    // 目录外文件一律不动，仅移除记录
    assert!(outside_pkg.is_file());
}

#[test]
// Windows 创建目录符号链接需管理员/开发者模式权限（CI 与开发机通常没有），
// 逃逸构造只在 Unix 下稳定可行；校验逻辑本身平台无关
#[cfg(unix)]
fn uninstall_refuses_symlink_escape() {
    let fixture = Fixture::new();
    // 构造逃逸：packages_root/<id>/packages 为指向目录外的符号链接，
    // packagePath 词法上仍在插件目录内——canonicalize 校验必须拦截
    let outside_dir = fixture.release_dir();
    fs::create_dir_all(&outside_dir).unwrap();
    let outside_pkg = outside_dir.join("keep.spkg");
    fs::write(&outside_pkg, b"keep").unwrap();

    let packages_link = fixture.packages_root.join("spark-example/packages");
    fs::create_dir_all(packages_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside_dir, &packages_link).unwrap();
    let escaped_path = packages_link.join("keep.spkg");

    let mut service = fixture.service();
    service.state.installed.insert(
        "spark-example".to_string(),
        InstalledPluginState {
            plugin_id: "spark-example".to_string(),
            version: "0.1.0".to_string(),
            package_path: escaped_path.to_string_lossy().to_string(),
            sha256: "00".repeat(32),
            size: 4,
            installed_at: 1,
            enabled: true,
            granted_permissions: vec![],
            trust: None,
            supported_spaces: None,
        },
    );
    service.persist().unwrap();

    // 校验失败只删记录不动文件：逃逸目标保留
    service.uninstall("spark-example").unwrap();
    assert!(!service.state.installed.contains_key("spark-example"));
    assert!(outside_pkg.is_file());
}
