//! 仓库锚定安装链路用例（plugin-dist §4.2）：mock fetcher 供声明/清单/签名/包体，
//! 资产 URL 一律 https（远程清单拒 file://），不触网。

use std::collections::BTreeMap;

use super::super::repo::RepoFetcher;
use super::*;

/// mock 抓取器：缺键 = 404；值以 "@ERR" 开头 = 网络错误；包体与文本同源（内容为 JSON 文本）。
struct MapFetcher {
    map: BTreeMap<String, String>,
}

impl RepoFetcher for MapFetcher {
    fn fetch_text(&self, url: &str, _max_bytes: u64) -> Result<Option<String>, String> {
        match self.map.get(url) {
            Some(text) if text.starts_with("@ERR") => Err(text.clone()),
            Some(text) => Ok(Some(text.clone())),
            None => Ok(None),
        }
    }

    fn fetch_bytes(&self, url: &str, _max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
        match self.map.get(url) {
            Some(text) if text.starts_with("@ERR") => Err(text.clone()),
            Some(text) => Ok(Some(text.clone().into_bytes())),
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
const PACKAGE_URL: &str =
    "https://github.com/acme/todo/releases/download/v0.2.0/spark-plugin-todo-0.2.0.spkg";

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

/// 造 .spkg 容器文本，返回 (文本, sha256, size)。
fn repo_package_text(version: &str) -> (String, String, u64) {
    let payload = serde_json::json!({
        "pluginId": REPO_ID,
        "domain": format!("plugin:{REPO_ID}"),
        "version": version,
        "files": [{"path": "manifest.json", "sha256": "00", "size": 1, "contentBase64": "AA=="}]
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&payload).unwrap());
    (
        text.clone(),
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
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.2.0", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration.clone()),
        (DECL_PROXY_URL, declaration),
        (MANIFEST_URL, manifest.clone()),
        (MANIFEST_PROXY_URL, manifest),
        (PACKAGE_URL, package_text),
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
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.2.0", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let signature = sign_text(&fixture.signing_key, &manifest);
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest.clone()),
        (SIG_URL, signature),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    let installed = service.install_from_repo_with(&fetcher, REPO_ID).unwrap();
    assert_eq!(installed.trust.as_deref(), Some("signed"));

    // 有 sig 但验签失败 → 拒绝
    let fixture = Fixture::new();
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.2.0", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let (_, other_key) = test_keypair(7);
    let bad_signature = sign_text(&other_key, &manifest);
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest),
        (SIG_URL, bad_signature),
        (PACKAGE_URL, package_text),
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
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.9.9", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let declaration = repo_declaration_text("0.2.0");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, declaration),
        (MANIFEST_URL, manifest),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        "Repo plugin manifest version mismatch: expected 0.2.0, got 0.9.9"
    );

    // 无签名清单双源不一致 → 拒绝（规格 §3.4，E_MANIFEST_CROSS_MISMATCH 文案）
    let fixture = Fixture::new();
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest_a = repo_manifest_text("0.2.0", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg");
    let manifest_b = repo_manifest_text("0.2.0", PACKAGE_URL, &"11".repeat(32), size, "spark-plugin-todo-0.2.0.spkg");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, repo_declaration_text("0.2.0")),
        (MANIFEST_URL, manifest_a),
        (MANIFEST_PROXY_URL, manifest_b),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Repo plugin manifest cross-check mismatch: {REPO_ID}")
    );
}

/// 负路径（规格 §4.2/§5）：清单 id 不一致 / 包体 sha256 不符（无残留）/
/// 远程清单资产 URL 拒 file://。
#[test]
fn install_from_repo_negative_paths() {
    // 清单 pluginId ≠ 规范化 id → E_MANIFEST_ID_MISMATCH
    let fixture = Fixture::new();
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.2.0", PACKAGE_URL, &digest, size, "spark-plugin-todo-0.2.0.spkg")
        .replace(&format!("\"pluginId\":\"{REPO_ID}\""), "\"pluginId\":\"github.com/evil/todo\"");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, repo_declaration_text("0.2.0")),
        (MANIFEST_URL, manifest),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Repo plugin manifest id mismatch: expected {REPO_ID}, got github.com/evil/todo")
    );
    assert!(!service.state.installed.contains_key(REPO_ID));

    // 包体 sha256 不符 → 拒绝且无残留（不落 installed、包文件不写盘）
    let fixture = Fixture::new();
    let (package_text, _digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text("0.2.0", PACKAGE_URL, &"ff".repeat(32), size, "spark-plugin-todo-0.2.0.spkg");
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, repo_declaration_text("0.2.0")),
        (MANIFEST_URL, manifest),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Plugin package sha256 mismatch for {REPO_ID}")
    );
    assert!(!service.state.installed.contains_key(REPO_ID));
    assert!(!fixture.packages_root.join("github.com").exists());

    // 远程清单资产 URL 为 file:// → 拒绝（file:// 只属内置目录本地 bundle 链路）
    let fixture = Fixture::new();
    let (package_text, digest, size) = repo_package_text("0.2.0");
    let manifest = repo_manifest_text(
        "0.2.0",
        "file:///etc/passwd",
        &digest,
        size,
        "spark-plugin-todo-0.2.0.spkg",
    );
    let fetcher = fetcher_of(&[
        (DECL_RELEASE_URL, repo_declaration_text("0.2.0")),
        (MANIFEST_URL, manifest),
        (PACKAGE_URL, package_text),
    ]);
    let mut service = fixture.service();
    assert_eq!(
        service.install_from_repo_with(&fetcher, REPO_ID).unwrap_err(),
        format!("Repo plugin manifest invalid: {REPO_ID}: package asset url must be https")
    );
    assert!(!service.state.installed.contains_key(REPO_ID));
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
