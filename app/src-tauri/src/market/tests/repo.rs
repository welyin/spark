//! 仓库锚定安装链路用例（plugin-dist §4.2）：mock fetcher 供声明/清单/签名，
//! 包资产走 file://（复用现有下载校验链路），不触网。

use std::collections::BTreeMap;

use super::super::repo::RepoFetcher;
use super::*;

/// mock 抓取器：缺键 = 404；值以 "@ERR" 开头 = 网络错误。
struct MapFetcher {
    map: BTreeMap<String, String>,
}

impl RepoFetcher for MapFetcher {
    fn fetch_text(&self, url: &str) -> Result<Option<String>, String> {
        match self.map.get(url) {
            Some(text) if text.starts_with("@ERR") => Err(text.clone()),
            Some(text) => Ok(Some(text.clone())),
            None => Ok(None),
        }
    }
}

const REPO_ID: &str = "github.com/acme/todo";
const DECL_RELEASE_URL: &str = "https://github.com/acme/todo/releases/latest/download/spark-plugin.json";
const DECL_PROXY_URL: &str =
    "https://mirror.ghproxy.com/https://github.com/acme/todo/releases/latest/download/spark-plugin.json";
const MANIFEST_URL: &str =
    "https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-manifest.json";
const MANIFEST_PROXY_URL: &str =
    "https://mirror.ghproxy.com/https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-manifest.json";
const SIG_URL: &str = "https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-manifest.sig";

fn repo_declaration_text(version: &str) -> String {
    serde_json::json!({
        "id": REPO_ID,
        "name": "待办清单",
        "icon": "",
        "summary": "仓库锚定测试插件",
        "category": "business",
        "version": version,
        "releaseAssetPattern": "spark-plugin-todo-<version>.spkg",
        "permissions": ["org:sync"],
        "mirrors": [],
        "sdkVersion": "1.0.0"
    })
    .to_string()
}

/// 在临时目录造 .spkg，返回 (路径, sha256, size)。
fn write_repo_package(fixture: &Fixture, version: &str) -> (PathBuf, String, u64) {
    let dir = fixture.release_root.join("_repo-assets");
    fs::create_dir_all(&dir).unwrap();
    let file_name = format!("spark-plugin-todo-{version}.spkg");
    let payload = serde_json::json!({
        "pluginId": REPO_ID,
        "domain": format!("plugin:{REPO_ID}"),
        "version": version,
        "files": [{"path": "manifest.json", "sha256": "00", "size": 1, "contentBase64": "AA=="}]
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
    let path = dir.join(&file_name);
    fs::write(&path, &text).unwrap();
    (
        path,
        hex::encode(sha2::Sha256::digest(text.as_bytes())),
        text.len() as u64,
    )
}

fn repo_manifest_text(version: &str, asset_url: &str, sha256: &str, size: u64, file_name: &str) -> String {
    serde_json::json!({
        "pluginId": REPO_ID,
        "domain": format!("plugin:{REPO_ID}"),
        "version": version,
        "assets": [{"kind": "package", "fileName": file_name, "url": asset_url, "sha256": sha256, "size": size}]
    })
    .to_string()
}

fn fetcher_of(pairs: &[(&str, String)]) -> MapFetcher {
    MapFetcher {
        map: pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

#[test]
fn install_from_repo_unsigned_cross_checked() {
    let fixture = Fixture::new();
    let (package_path, digest, size) = write_repo_package(&fixture, "0.2.0");
    let asset_url = format!("file://{}", package_path.to_string_lossy());
    let manifest = repo_manifest_text("0.2.0", &asset_url, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration.clone()),
        (DECL_PROXY_URL, declaration),
        (MANIFEST_URL, manifest.clone()),
        (MANIFEST_PROXY_URL, manifest),
    ]);

    let mut service = fixture.service();
    let installed = service
        .install_from_repo_with(&fetcher, "GitHub.com/ACME/todo/")
        .unwrap();
    assert_eq!(installed.plugin_id, REPO_ID);
    assert_eq!(installed.version, "0.2.0");
    assert_eq!(installed.trust.as_deref(), Some("repo-anchored"));
    assert_eq!(
        installed.granted_permissions,
        vec!["storage:read", "storage:write", "org:read", "proof:verify", "org:sync"]
    );
    // 包落盘：packages_root/<id 段>/packages/<fileName>
    assert!(fixture
        .packages_root
        .join("github.com/acme/todo/packages/spark-plugin-todo-0.2.0.spkg")
        .is_file());

    // 市场列表合成条目（名称/简介/分类来自声明文件缓存）
    let items = service.list_market();
    let entry = items.iter().find(|i| i.catalog.id == REPO_ID).unwrap();
    assert_eq!(entry.catalog.name, "待办清单");
    assert_eq!(entry.catalog.category, "business");
    assert_eq!(entry.catalog.domain, format!("plugin:{REPO_ID}"));
    assert!(entry.installed && entry.enabled);

    // 持久化 + sled 声明缓存：新实例（空内存缓存）对账后列表仍含该插件
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    let items = reloaded.list_market();
    let entry = items.iter().find(|i| i.catalog.id == REPO_ID).unwrap();
    assert_eq!(entry.catalog.name, "待办清单");
    assert!(entry.installed);
}

#[test]
fn install_from_repo_signed_and_bad_signature() {
    // 有 sig 且验签通过 → trust = signed（免交叉：清单只放 origin 一处）
    let fixture = Fixture::new();
    let (package_path, digest, size) = write_repo_package(&fixture, "0.2.0");
    let asset_url = format!("file://{}", package_path.to_string_lossy());
    let manifest = repo_manifest_text("0.2.0", &asset_url, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let signature = sign_text(&fixture.signing_key, &manifest);
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest.clone()),
        (SIG_URL, signature),
    ]);
    let mut service = fixture.service();
    let installed = service.install_from_repo_with(&fetcher, REPO_ID).unwrap();
    assert_eq!(installed.trust.as_deref(), Some("signed"));

    // 有 sig 但验签失败 → 拒绝
    let fixture = Fixture::new();
    let (package_path, digest, size) = write_repo_package(&fixture, "0.2.0");
    let asset_url = format!("file://{}", package_path.to_string_lossy());
    let manifest = repo_manifest_text("0.2.0", &asset_url, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let (_, other_key) = test_keypair(7);
    let bad_signature = sign_text(&other_key, &manifest);
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest),
        (SIG_URL, bad_signature),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Repo plugin manifest signature verification failed: {REPO_ID}")
    );
    assert!(!service.state.installed.contains_key(REPO_ID));
}

#[test]
fn install_from_repo_rejects_cross_mismatch_and_version_mismatch() {
    // 声明文件双源不一致 → 拒绝（规格 §3.4）
    let fixture = Fixture::new();
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, repo_declaration_text("0.2.0")),
        (DECL_PROXY_URL, repo_declaration_text("0.9.9")),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Repo plugin declaration cross-check mismatch: {REPO_ID}")
    );

    // 清单 version 与声明不一致 → 拒绝（规格 §4.2-4）
    let fixture = Fixture::new();
    let (package_path, digest, size) = write_repo_package(&fixture, "0.2.0");
    let asset_url = format!("file://{}", package_path.to_string_lossy());
    let manifest = repo_manifest_text("0.9.9", &asset_url, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        "Repo plugin manifest version mismatch: expected 0.2.0, got 0.9.9"
    );
}

#[test]
fn resolve_repo_plugin_uses_memory_then_sled_cache() {
    let fixture = Fixture::new();
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[(DECL_RELEASE_URL, declaration)]);
    let mut service = fixture.service();
    let resolved = service.resolve_repo_plugin_with(&fetcher, REPO_ID).unwrap();
    assert_eq!(resolved.id, REPO_ID);
    assert_eq!(resolved.name, "待办清单");

    // 内存缓存：fetcher 全部 404 也能解析
    let empty = fetcher_of(&[]);
    assert!(service.resolve_repo_plugin_with(&empty, REPO_ID).is_ok());
    // sled 缓存：新实例（空内存）同样离线可解析
    let mut reloaded = fixture.service();
    let resolved = reloaded.resolve_repo_plugin_with(&empty, REPO_ID).unwrap();
    assert_eq!(resolved.version, "0.2.0");
}
