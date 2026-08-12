//! 更新探测与来源安全（http:// 拒读）用例。
//!
//! 解耦（plugin_decoupling.md §4.5）：`check_for_updates` 从已安装状态反查。
//! 侧载插件（无仓库声明缓存）探测时清单 URL 为空 → check-failed（不中断）；
//! 未安装插件单项查询报「未安装」。

use base64::Engine;

use super::*;

#[test]
fn check_updates_targets_installed_state_only() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    service.initialize().unwrap();

    // 无任何已装插件：check(None) 返回空，check(Some(id)) 报未安装
    assert!(service.check_for_updates(None).unwrap().is_empty());
    assert_eq!(
        service.check_for_updates(Some("nope")).unwrap_err(),
        "Plugin is not installed: nope"
    );

    // 侧载播种一个已装插件
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

    // 侧载插件无仓库声明缓存 → 清单 URL 空，探测落 check-failed（latest 置空）
    let probes = service.check_for_updates(None).unwrap();
    let probe = probes
        .iter()
        .find(|p| p.plugin_id == "todo-local")
        .expect("todo-local probe");
    assert!(!probe.update_available);
    assert!(probe.latest_version.is_none());
    assert!(probe.reason.starts_with("check-failed:"));

    // 失败原因进入列表展示
    let item = service
        .list_market()
        .into_iter()
        .find(|item| item.catalog.id == "todo-local")
        .expect("todo-local market item");
    assert!(item.last_check_reason.starts_with("check-failed:"));
}

#[test]
fn http_manifest_url_is_rejected() {
    assert_eq!(
        fetch_text_smart("http://example.com/update-manifest.json").unwrap_err(),
        "Insecure plugin manifest URL is not allowed"
    );
}
