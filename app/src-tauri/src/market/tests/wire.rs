//! 命令出参线形用例：对齐旧 preload.ts 声明（拍平、camelCase、无嵌套包裹）。

use base64::Engine;

use super::*;

/// 命令出参线形对齐旧 preload.ts：catalog 字段拍平、camelCase、无嵌套包裹。
#[test]
fn wire_shapes_match_preload_declarations() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    // 解耦后无内置目录：侧载播种一个已装插件以产出市场条目
    let spkg = fixture.release_root.join("seed/todo-local.spkg");
    fs::create_dir_all(spkg.parent().unwrap()).unwrap();
    let manifest_json = br#"{"id":"todo-local","name":"LocalTodo","supportedSpaces":["personal","org"]}"#;
    let text = serde_json::json!({
        "pluginId": "todo-local",
        "domain": "plugin:todo-local",
        "version": "1.0.0",
        "files": [
            {
                "path": "manifest.json",
                "sha256": hex::encode(sha2::Sha256::digest(manifest_json)),
                "size": manifest_json.len(),
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(manifest_json)
            },
            {
                "path": "views/main.js",
                "sha256": hex::encode(sha2::Sha256::digest(b"hello")),
                "size": 5,
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(b"hello")
            }
        ]
    })
    .to_string();
    fs::write(&spkg, &text).unwrap();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();

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
    // 侧载导入会写入 reason="installed" 的本地占位探测
    assert_eq!(item["lastCheckReason"], "installed");
    // 侧载插件无仓库声明缓存：supportedSpaces 回落安装时落库的解析值
    assert_eq!(item["supportedSpaces"], serde_json::json!(["personal", "org"]));
    // 信任级进入数据模型：侧载 = L0；requires 未声明时省略（None skip）
    assert_eq!(item["trustLevel"], serde_json::json!("L0"));
    assert!(item.get("requires").is_none(), "requires None 应省略");
    // 无声明缓存 → 派生的清单 URL 为空（合成条目不携带远端清单地址）
    assert_eq!(item["package"]["updateManifestUrl"], serde_json::json!(""));

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
        requires: None,
        window: None,
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
        requires: None,
        window: None,
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
