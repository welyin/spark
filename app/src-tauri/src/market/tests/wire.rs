//! 命令出参线形用例：对齐旧 preload.ts 声明（拍平、camelCase、无嵌套包裹）。

use super::*;

/// 命令出参线形对齐旧 preload.ts：catalog 字段拍平、camelCase、无嵌套包裹。
#[test]
fn wire_shapes_match_preload_declarations() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let value = serde_json::to_value(service.list_market()).unwrap();
    let item = &value[0];
    for key in [
        "id",
        "domain",
        "name",
        "description",
        "category",
        "version",
        "views",
        "permissions",
        "package",
        "installed",
        "enabled",
        "installedVersion",
        "latestVersion",
        "updateAvailable",
        "lastCheckedAt",
        "lastCheckReason",
    ] {
        assert!(item.get(key).is_some(), "PluginMarketItem missing key {key}");
    }
    assert!(item.get("catalog").is_none(), "catalog 应拍平而非嵌套");
    assert_eq!(item["lastCheckReason"], "not-checked");
    // supportedSpaces：Some 出现（spark-example 目录声明 ["org"]）
    assert_eq!(item["supportedSpaces"], serde_json::json!(["org"]));
    assert!(item["package"]["updateManifestUrl"]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/"));

    // InstalledPluginState / PluginUpdateProbe 键名
    let state = serde_json::to_value(InstalledPluginState {
        plugin_id: "spark-example".to_string(),
        version: "0.1.0".to_string(),
        package_path: "/tmp/x".to_string(),
        sha256: "aa".to_string(),
        size: 1,
        installed_at: 2,
        enabled: true,
        granted_permissions: vec!["org:sync".to_string()],
        trust: None,
        supported_spaces: None,
    })
    .unwrap();
    for key in [
        "pluginId",
        "version",
        "packagePath",
        "sha256",
        "size",
        "installedAt",
        "enabled",
        "grantedPermissions",
    ] {
        assert!(state.get(key).is_some(), "InstalledPluginState missing key {key}");
    }
    // supportedSpaces：None 省略（缺省 = 未声明，按 ["org"] 口径）
    assert!(state.get("supportedSpaces").is_none(), "supportedSpaces None 应省略");
    // supportedSpaces：Some 出现
    let state_with_spaces = serde_json::to_value(InstalledPluginState {
        plugin_id: "spark-example".to_string(),
        version: "0.1.0".to_string(),
        package_path: "/tmp/x".to_string(),
        sha256: "aa".to_string(),
        size: 1,
        installed_at: 2,
        enabled: true,
        granted_permissions: vec![],
        trust: None,
        supported_spaces: Some(vec!["personal".to_string()]),
    })
    .unwrap();
    assert_eq!(
        state_with_spaces["supportedSpaces"],
        serde_json::json!(["personal"])
    );
    let probe = serde_json::to_value(PluginUpdateProbe {
        plugin_id: "spark-example".to_string(),
        checked_at: 1,
        latest_version: None,
        update_available: false,
        reason: "up-to-date".to_string(),
    })
    .unwrap();
    for key in ["pluginId", "checkedAt", "latestVersion", "updateAvailable", "reason"] {
        assert!(probe.get(key).is_some(), "PluginUpdateProbe missing key {key}");
    }
}
