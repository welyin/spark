//! 安装 / 启停用例。
//!
//! 解耦（plugin_decoupling.md §4）：目录驱动 `install()` 已退役——插件只能经
//! 仓库锚定（install_from_repo）/ .spkg 侧载进入；`upgrade()` 复用仓库锚定安装。
//! 此文件覆盖 upgrade 的前置校验（须已安装 / 仅仓库地址可升级）与仓库锚定升级链路，
//! 以及仍基于已安装状态的启停能力（用侧载播种）。

use std::collections::BTreeMap;

use base64::Engine;

use super::super::repo::RepoFetcher;
use super::*;

const REPO_ID: &str = "github.com/acme/todo";
const DECL_URL: &str = "https://github.com/acme/todo/releases/latest/download/spark-plugin.json";
const MANIFEST_URL: &str =
    "https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-manifest.json";
const PACKAGE_URL: &str =
    "https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-0.2.0.spkg";

/// mock 抓取器（同 repo.rs 测试惯例）：缺键 = 404；包体与文本同源。
struct MapFetcher {
    map: BTreeMap<String, String>,
}

impl RepoFetcher for MapFetcher {
    fn fetch_text(&self, url: &str, _max_bytes: u64) -> Result<Option<String>, String> {
        Ok(self.map.get(url).cloned())
    }

    fn fetch_bytes(&self, url: &str, _max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
        Ok(self.map.get(url).map(|t| t.clone().into_bytes()))
    }
}

fn repo_declaration_text() -> String {
    serde_json::json!({
        "id": REPO_ID,
        "name": "待办清单",
        "icon": "",
        "summary": "仓库锚定测试插件",
        "category": "business",
        "version": "0.2.0",
        "releaseAssetPattern": "spark-plugin-todo-<version>.spkg",
        "permissions": [],
        "mirrors": [],
        "sdkVersion": "1.0.0"
    })
    .to_string()
}

fn repo_package_text() -> (String, String, u64) {
    let payload = serde_json::json!({
        "pluginId": REPO_ID,
        "domain": format!("plugin:{REPO_ID}"),
        "version": "0.2.0",
        "files": [{"path": "manifest.json", "sha256": "00", "size": 1, "contentBase64": "AA=="}]
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
    (
        text.clone(),
        hex::encode(sha2::Sha256::digest(text.as_bytes())),
        text.len() as u64,
    )
}

fn repo_manifest_text(digest: &str, size: u64) -> String {
    serde_json::json!({
        "pluginId": REPO_ID,
        "domain": format!("plugin:{REPO_ID}"),
        "version": "0.2.0",
        "assets": [{"kind": "package", "fileName": "spark-plugin-todo-0.2.0.spkg", "url": PACKAGE_URL, "sha256": digest, "size": size}]
    })
    .to_string()
}

fn repo_fetcher() -> MapFetcher {
    let (package_text, digest, size) = repo_package_text();
    MapFetcher {
        map: [
            (DECL_URL.to_string(), repo_declaration_text()),
            (MANIFEST_URL.to_string(), repo_manifest_text(&digest, size)),
            (PACKAGE_URL.to_string(), package_text),
        ]
        .into_iter()
        .collect(),
    }
}

/// 侧载播种一个已安装插件（trust = sideloaded，短名 id）。
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
fn upgrade_requires_installed_plugin() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.upgrade(REPO_ID).unwrap_err(),
        format!("Plugin is not installed: {REPO_ID}")
    );
}

#[test]
fn upgrade_rejects_non_repo_anchored_plugin() {
    // 侧载/短名插件（todo-local）无声明源，升级路径本就不存在 → 明确报错
    let fixture = Fixture::new();
    let mut service = fixture.service();
    seed_sideloaded(&fixture, &mut service);
    assert_eq!(
        service.upgrade("todo-local").unwrap_err(),
        "Only repo-anchored plugins can be upgraded: todo-local"
    );
    // 且不破坏已装状态
    assert!(service.state.installed.contains_key("todo-local"));
}

#[test]
fn upgrade_repo_plugin_reinstalls_and_marks_upgraded() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    let fetcher = repo_fetcher();

    // 先经仓库锚定安装
    service.install_from_repo_with(&fetcher, REPO_ID).unwrap();
    assert_eq!(
        service.update_probes[REPO_ID].reason,
        "installed",
        "首次安装 probe reason = installed"
    );

    // upgrade 复用同一仓库锚定源重拉覆盖；probe reason 切为 upgraded
    let upgraded = service.upgrade_with(&fetcher, REPO_ID).unwrap();
    assert_eq!(upgraded.plugin_id, REPO_ID);
    assert_eq!(upgraded.version, "0.2.0");
    assert!(service.state.installed.contains_key(REPO_ID));
    assert_eq!(
        service.update_probes[REPO_ID].reason,
        "upgraded",
        "upgrade 后 probe reason = upgraded"
    );
}

#[test]
fn set_enabled_roundtrip_on_installed_plugin() {
    let fixture = Fixture::new();
    let mut service = fixture.service();

    // 未安装不能启停
    assert_eq!(
        service.set_enabled("todo-local", false).unwrap_err(),
        "Plugin is not installed: todo-local"
    );

    seed_sideloaded(&fixture, &mut service);
    let disabled = service.set_enabled("todo-local", false).unwrap();
    assert!(!disabled.enabled);
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(!reloaded.state.installed["todo-local"].enabled);
}
