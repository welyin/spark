//! 卸载用例：删记录 + 删包文件（限 app_data 插件目录内）；
//! 目录外 packagePath 只删记录不动文件；非法 id 拒绝；
//! 卸载墓碑（uninstalled）持久化。
//!
//! 解耦（plugin_decoupling.md §4.3）：已移除 dev-source 登记分支（源码目录不再
//! 标记 bundled-dev-source）；reconcile 已删除，故「墓碑阻止对账复活」用例随移除。

use base64::Engine;

use super::*;

/// 侧载播种一个已安装插件（trust = sideloaded）。
fn seed_sideloaded(fixture: &Fixture, service: &mut PluginMarketService) {
    let spkg = fixture.release_root.join("seed/todo-local.spkg");
    fs::create_dir_all(spkg.parent().unwrap()).unwrap();
    let text = serde_json::json!({
        "pluginId": "todo-local",
        "domain": "plugin:todo-local",
        "version": "1.0.0",
        "files": [{
            "path": "views/main.js",
            "sha256": hex::encode(sha2::Sha256::digest(b"hello")),
            "size": 5,
            "contentBase64": base64::engine::general_purpose::STANDARD.encode(b"hello")
        }]
    })
    .to_string();
    fs::write(&spkg, &text).unwrap();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();
}

#[test]
fn uninstall_removes_record_probe_and_package_files() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    seed_sideloaded(&fixture, &mut service);
    let package = fixture
        .packages_root
        .join("todo-local/packages/todo-local.spkg");
    assert!(package.is_file());

    service.uninstall("todo-local").unwrap();
    assert!(!service.state.installed.contains_key("todo-local"));
    assert!(!service.update_probes.contains_key("todo-local"));
    // 包文件与插件目录整体清理
    assert!(!package.exists());
    assert!(!fixture.packages_root.join("todo-local").exists());

    // 持久化语义：状态文件里记录已移除、墓碑已写入
    let persisted = read_state_file(&fixture.state_file);
    assert!(!persisted.installed.contains_key("todo-local"));
    assert!(persisted.uninstalled.contains("todo-local"));

    // 二次卸载：记录已不在，返回 not installed
    assert_eq!(
        service.uninstall("todo-local").unwrap_err(),
        "Plugin is not installed: todo-local"
    );
}

#[test]
fn uninstall_rejects_invalid_id_and_missing_plugin() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    seed_sideloaded(&fixture, &mut service);

    // 非法 id：段校验与 sideload/repo 同规则（拒空段/穿越段/非法字符/Windows 保留名）
    for bad in ["", "../evil", "todo-local/..", "UPPER", "a b", "con", "github.com/nul/x"] {
        assert_eq!(
            service.uninstall(bad).unwrap_err(),
            format!("Plugin id invalid: {bad}")
        );
    }
    // 负向用例不影响既有记录与文件
    assert!(service.state.installed.contains_key("todo-local"));

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
        "todo-local".to_string(),
        InstalledPluginState {
            plugin_id: "todo-local".to_string(),
            version: "0.1.0".to_string(),
            package_path: outside_pkg.to_string_lossy().to_string(),
            sha256: "00".repeat(32),
            size: 4,
            installed_at: 1,
            enabled: true,
            granted_permissions: vec![],
            trust: None,
            supported_spaces: None,
            requires: None,
        },
    );
    service.persist().unwrap();

    service.uninstall("todo-local").unwrap();
    assert!(!service.state.installed.contains_key("todo-local"));
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

    let packages_link = fixture.packages_root.join("todo-local/packages");
    fs::create_dir_all(packages_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside_dir, &packages_link).unwrap();
    let escaped_path = packages_link.join("keep.spkg");

    let mut service = fixture.service();
    service.state.installed.insert(
        "todo-local".to_string(),
        InstalledPluginState {
            plugin_id: "todo-local".to_string(),
            version: "0.1.0".to_string(),
            package_path: escaped_path.to_string_lossy().to_string(),
            sha256: "00".repeat(32),
            size: 4,
            installed_at: 1,
            enabled: true,
            granted_permissions: vec![],
            trust: None,
            supported_spaces: None,
            requires: None,
        },
    );
    service.persist().unwrap();

    // 校验失败只删记录不动文件：逃逸目标保留
    service.uninstall("todo-local").unwrap();
    assert!(!service.state.installed.contains_key("todo-local"));
    assert!(outside_pkg.is_file());
}
