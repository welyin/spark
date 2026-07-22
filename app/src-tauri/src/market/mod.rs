//! 插件市场服务（Rust 移植自 TS desktop/src/main/plugin-market/service.ts，488 行原版）。
//!
//! 语义对齐要点：
//! - 清单解析优先级：本地 dist-market 发布目录优先（update-manifest.json + .sig 齐备），
//!   否则回退目录条目声明的远端 URL；`file://` 与 `/` 开头按本地文件读，
//!   `http://` 一律拒绝，其余按 https 下载（reqwest blocking + native-tls 系统信任库）。
//! - 安装 = 验签（Ed25519 detached，见 trust.rs）→ 下载/复制 .spkg → 校验
//!   sha256/size → 落状态（grantedPermissions = 基础 ∪ 声明∩高级，见 permissions.rs）。
//! - 启动对账（reconcile）：本地 bundle 验签通过 → 标记已安装；
//!   插件源码目录（code/plugins/<id>/ 含 manifest.ts/js）→ 标记 bundled-dev-source。
//! - 状态文件：<app_data_dir>/plugin-market-state.json（TS PersistedPluginState 同构）；
//!   更新探测（updateProbes）与 TS 一致仅驻留内存，不持久化。
//!
//! 与 TS 的有意差异（语义等价）：错误文案不含 JS `String(error)` 的 `Error: ` 前缀；
//! sha256 整文件读入计算（包体小，TS 为流式）。

pub mod catalog;
pub mod permissions;
pub mod semver;
pub mod trust;
pub mod types;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::Digest;

use catalog::{find_catalog_item, list_plugin_catalog, PluginCatalogItem};
use permissions::{basic_permissions, normalize_declared_permissions, resolve_granted_permissions};
use types::{
    InstalledPluginState, PersistedPluginState, PluginAsset, PluginMarketItem,
    PluginReleaseManifest, PluginUpdateProbe,
};

const PLUGIN_STATE_FILE: &str = "plugin-market-state.json";

/// 市场服务路径配置（注入式，测试用临时目录直造）。
#[derive(Clone, Debug)]
pub struct MarketPaths {
    /// 状态文件：<app_data_dir>/plugin-market-state.json
    pub state_file: PathBuf,
    /// 已安装包落盘根目录：<app_data_dir>/plugins（包在 <root>/<id>/packages/）
    pub packages_root: PathBuf,
    /// 本地发布目录候选根（各 root/<pluginId>/ 下找 update-manifest.json/.sig）
    pub local_release_roots: Vec<PathBuf>,
    /// 插件源码目录候选根（各 root/<pluginId>/ 下找 manifest.ts/js）
    pub local_source_roots: Vec<PathBuf>,
}

/// 词法归一化路径（折叠 `.`/`..`，TS path.normalize 同款；不做 symlink 解析）。
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl MarketPaths {
    /// 生产默认：状态/包目录在 app_data_dir 下；本地发布目录与源码目录按
    /// 编译期 crate 位置（code/app/src-tauri）与运行时 cwd 双候选
    /// （对齐 TS 的 appPath/cwd 候选语义；打包安装后这些目录不存在即自动走远端 URL）。
    pub fn for_app(app_data_dir: &Path) -> Self {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cwd = std::env::current_dir().unwrap_or_default();
        let release_roots = [
            manifest_dir.join("../dist-market/plugins"),
            cwd.join("dist-market/plugins"),
        ];
        let source_roots = [manifest_dir.join("../../plugins"), cwd.join("../plugins")];
        let dedupe = |dirs: &[PathBuf; 2]| {
            let mut unique: Vec<PathBuf> = Vec::new();
            for dir in dirs {
                let normalized = normalize_path(dir);
                if !unique.contains(&normalized) {
                    unique.push(normalized);
                }
            }
            unique
        };
        Self {
            state_file: app_data_dir.join(PLUGIN_STATE_FILE),
            packages_root: app_data_dir.join("plugins"),
            local_release_roots: dedupe(&release_roots),
            local_source_roots: dedupe(&source_roots),
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn to_file_url(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

/// TS `normalizeFileUrl`：file:// 原样；/ 开头补 file://；其余（https）原样。
fn normalize_file_url(url: &str) -> String {
    if url.starts_with("file://") {
        url.to_string()
    } else if let Some(path) = url.strip_prefix('/') {
        format!("file:///{path}")
    } else {
        url.to_string()
    }
}

/// TS `fetchTextSmart`：file:// 与 / 读本地文件；http:// 拒绝；其余 https GET。
fn fetch_text_smart(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return fs::read_to_string(path).map_err(|e| format!("{e}"));
    }
    if url.starts_with('/') {
        return fs::read_to_string(url).map_err(|e| format!("{e}"));
    }
    if url.starts_with("http://") {
        return Err("Insecure plugin manifest URL is not allowed".to_string());
    }
    let response = reqwest::blocking::get(url).map_err(|e| format!("Request failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(format!("Request failed: {url}, status={status}"));
    }
    response
        .text()
        .map_err(|e| format!("Request failed: {url}: {e}"))
}

/// TS `downloadFile`：https 下载到目标路径（status >= 400 报错）。
fn download_file(url: &str, destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
    }
    let mut response = reqwest::blocking::get(url).map_err(|e| format!("Download failed: {url}: {e}"))?;
    let status = response.status();
    if status.as_u16() >= 400 {
        return Err(format!("Download failed: {url}, status={status}"));
    }
    let mut file = fs::File::create(destination).map_err(|e| format!("{e}"))?;
    response
        .copy_to(&mut file)
        .map_err(|e| format!("Download failed: {url}: {e}"))?;
    Ok(())
}

fn compute_file_sha256(path: &Path) -> Result<String, String> {
    let content = fs::read(path).map_err(|e| format!("{e}"))?;
    Ok(hex::encode(sha2::Sha256::digest(content)))
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path).map(|m| m.len()).map_err(|e| format!("{e}"))
}

/// TS `readJsonFile`：读取失败/解析失败一律回退默认值。
fn read_state_file(path: &Path) -> PersistedPluginState {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// TS `writeJsonFile`：mkdir -p + JSON 两空格缩进。
fn write_state_file(path: &Path, state: &PersistedPluginState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|e| format!("{e}"))?;
    fs::write(path, text).map_err(|e| format!("{e}"))
}

/// 插件市场服务（TS `PluginMarketService`）。
pub struct PluginMarketService {
    paths: MarketPaths,
    /// 信任公钥（PEM；启动时由 trust::get_plugin_trust_config 解析注入）。
    trust_keys: Vec<String>,
    state: PersistedPluginState,
    update_probes: BTreeMap<String, PluginUpdateProbe>,
}

impl PluginMarketService {
    pub fn new(paths: MarketPaths, trust_keys: Vec<String>) -> Self {
        Self {
            paths,
            trust_keys,
            state: PersistedPluginState::default(),
            update_probes: BTreeMap::new(),
        }
    }

    /// TS `initialize`：读状态 → 回填授权 → 启动对账。
    pub fn initialize(&mut self) -> Result<(), String> {
        self.state = read_state_file(&self.paths.state_file);
        self.backfill_granted_permissions()?;
        self.reconcile_bundled_installed_state()?;
        Ok(())
    }

    /// 兼容旧版安装状态：缺失 grantedPermissions 时按目录声明回填（TS 同名）。
    fn backfill_granted_permissions(&mut self) -> Result<(), String> {
        let mut changed = false;
        for (plugin_id, installed) in self.state.installed.iter_mut() {
            if !installed.granted_permissions.is_empty() {
                continue;
            }
            let granted = match list_plugin_catalog().into_iter().find(|c| c.id == *plugin_id) {
                Some(item) => resolve_granted_permissions(&item.permissions),
                None => basic_permissions(),
            };
            installed.granted_permissions = granted;
            changed = true;
        }
        if changed {
            self.persist()?;
        }
        Ok(())
    }

    /// TS `resolveDeclaredPermissions`：清单声明（规范化）优先，缺省用目录声明。
    fn resolve_declared_permissions(
        item: &PluginCatalogItem,
        manifest: Option<&PluginReleaseManifest>,
    ) -> Vec<String> {
        let declared = match manifest.and_then(|m| m.permissions.as_ref()) {
            Some(raw) => normalize_declared_permissions(raw),
            None => item.permissions.clone(),
        };
        resolve_granted_permissions(&declared)
    }

    fn persist(&self) -> Result<(), String> {
        write_state_file(&self.paths.state_file, &self.state)
    }

    /// 本地发布目录（root/<pluginId>/ 下 update-manifest.json + .sig 齐备）。
    fn resolve_bundled_manifest_paths(&self, plugin_id: &str) -> Option<(PathBuf, PathBuf, PathBuf)> {
        for root in &self.paths.local_release_roots {
            let local_dir = root.join(plugin_id);
            let manifest_path = local_dir.join("update-manifest.json");
            let signature_path = local_dir.join("update-manifest.sig");
            if manifest_path.is_file() && signature_path.is_file() {
                return Some((manifest_path, signature_path, local_dir));
            }
        }
        None
    }

    /// 插件源码目录（root/<pluginId>/ 下含 manifest.ts 或 manifest.js）。
    fn resolve_bundled_source_plugin_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        for root in &self.paths.local_source_roots {
            let dir = root.join(plugin_id);
            if dir.join("manifest.ts").is_file() || dir.join("manifest.js").is_file() {
                return Some(dir);
            }
        }
        None
    }

    /// TS `resolveManifestEndpoints`：本地 bundle 优先，否则目录声明的远端 URL。
    fn resolve_manifest_endpoints(&self, item: &PluginCatalogItem) -> (String, String) {
        if let Some((manifest_path, signature_path, _)) = self.resolve_bundled_manifest_paths(&item.id)
        {
            return (to_file_url(&manifest_path), to_file_url(&signature_path));
        }
        (
            normalize_file_url(&item.package.update_manifest_url),
            normalize_file_url(&item.package.signature_url),
        )
    }

    /// TS `buildDevSourceInstalledState`：开发态源码直挂（installedAt 恒 0，不落盘）。
    fn build_dev_source_installed_state(&self, item: &PluginCatalogItem) -> Option<InstalledPluginState> {
        let source_dir = self.resolve_bundled_source_plugin_dir(&item.id)?;
        Some(InstalledPluginState {
            plugin_id: item.id.clone(),
            version: item.version.clone(),
            package_path: source_dir.to_string_lossy().to_string(),
            sha256: "bundled-dev-source".to_string(),
            size: 0,
            installed_at: 0,
            enabled: true,
            granted_permissions: resolve_granted_permissions(&item.permissions),
        })
    }

    /// TS `reconcileBundledInstalledState`：本地 bundle 验签通过标记已安装；
    /// 其次源码目录标记 bundled-dev-source。坏 bundle 静默跳过，保留显式安装路径。
    fn reconcile_bundled_installed_state(&mut self) -> Result<(), String> {
        let mut changed = false;

        for item in list_plugin_catalog() {
            if self.state.installed.contains_key(&item.id) {
                continue;
            }

            if let Some((manifest_path, signature_path, local_dir)) =
                self.resolve_bundled_manifest_paths(&item.id)
            {
                // TS：整段 try/catch 静默忽略坏 bundle
                let mut attempt = || -> Result<(), String> {
                    let manifest_text = fs::read_to_string(&manifest_path).map_err(|e| format!("{e}"))?;
                    let signature_text = fs::read_to_string(&signature_path)
                        .map_err(|e| format!("{e}"))?
                        .trim()
                        .to_string();
                    if !trust::verify_manifest_signature(&manifest_text, &signature_text, &self.trust_keys)
                    {
                        return Err("signature verification failed".to_string());
                    }
                    let manifest: PluginReleaseManifest =
                        serde_json::from_str(&manifest_text).map_err(|e| format!("{e}"))?;
                    if manifest.plugin_id != item.id || manifest.domain != item.domain {
                        return Err("manifest id/domain mismatch".to_string());
                    }
                    let asset = manifest
                        .package_asset()
                        .ok_or_else(|| "no package asset".to_string())?;
                    let package_path = local_dir.join(&asset.file_name);
                    if !package_path.is_file() {
                        return Err("package file missing".to_string());
                    }
                    let digest = compute_file_sha256(&package_path)?;
                    let size = file_size(&package_path)?;
                    if digest != asset.sha256 || size != asset.size {
                        return Err("package digest/size mismatch".to_string());
                    }
                    let granted = Self::resolve_declared_permissions(&item, Some(&manifest));
                    self.state.installed.insert(
                        item.id.clone(),
                        InstalledPluginState {
                            plugin_id: item.id.clone(),
                            version: manifest.version.clone(),
                            package_path: package_path.to_string_lossy().to_string(),
                            sha256: digest,
                            size,
                            installed_at: now_millis(),
                            enabled: true,
                            granted_permissions: granted,
                        },
                    );
                    self.update_probes.insert(
                        item.id.clone(),
                        PluginUpdateProbe {
                            plugin_id: item.id.clone(),
                            checked_at: now_millis(),
                            latest_version: Some(manifest.version.clone()),
                            update_available: false,
                            reason: "bundled".to_string(),
                        },
                    );
                    Ok(())
                };
                if attempt().is_ok() {
                    changed = true;
                }
            }

            if self.state.installed.contains_key(&item.id) {
                continue;
            }

            let Some(source_dir) = self.resolve_bundled_source_plugin_dir(&item.id) else {
                continue;
            };
            self.state.installed.insert(
                item.id.clone(),
                InstalledPluginState {
                    plugin_id: item.id.clone(),
                    version: item.version.clone(),
                    package_path: source_dir.to_string_lossy().to_string(),
                    sha256: "bundled-dev-source".to_string(),
                    size: 0,
                    installed_at: now_millis(),
                    enabled: true,
                    granted_permissions: resolve_granted_permissions(&item.permissions),
                },
            );
            self.update_probes.insert(
                item.id.clone(),
                PluginUpdateProbe {
                    plugin_id: item.id.clone(),
                    checked_at: now_millis(),
                    latest_version: Some(item.version.clone()),
                    update_available: false,
                    reason: "bundled-dev-source".to_string(),
                },
            );
            changed = true;
        }

        if changed {
            self.persist()?;
        }
        Ok(())
    }

    /// TS `loadVerifiedManifest`：取清单+签名 → 验签 → 解析 → id/domain 匹配。
    fn load_verified_manifest(&self, item: &PluginCatalogItem) -> Result<PluginReleaseManifest, String> {
        let (manifest_url, signature_url) = self.resolve_manifest_endpoints(item);
        let manifest_text = fetch_text_smart(&manifest_url)?;
        let signature_text = fetch_text_smart(&signature_url)?.trim().to_string();

        if !trust::verify_manifest_signature(&manifest_text, &signature_text, &self.trust_keys) {
            return Err(format!(
                "Plugin manifest signature verification failed: {}",
                item.id
            ));
        }

        let manifest: PluginReleaseManifest =
            serde_json::from_str(&manifest_text).map_err(|e| format!("{e}"))?;
        if manifest.plugin_id != item.id {
            return Err(format!(
                "Plugin manifest id mismatch: expected {}, got {}",
                item.id, manifest.plugin_id
            ));
        }
        if manifest.domain != item.domain {
            return Err(format!(
                "Plugin manifest domain mismatch: expected {}, got {}",
                item.domain, manifest.domain
            ));
        }
        Ok(manifest)
    }

    /// TS `downloadAndVerifyAsset`：落 <packages_root>/<id>/packages/<fileName>，
    /// file:// 复制、https 下载，随后校验 sha256 与 size。
    fn download_and_verify_asset(
        &self,
        asset: &PluginAsset,
        plugin_id: &str,
    ) -> Result<(PathBuf, String, u64), String> {
        let plugin_dir = self.paths.packages_root.join(plugin_id).join("packages");
        fs::create_dir_all(&plugin_dir).map_err(|e| format!("{e}"))?;
        let file_path = plugin_dir.join(&asset.file_name);

        let url = normalize_file_url(&asset.url);
        if let Some(source) = url.strip_prefix("file://") {
            fs::copy(source, &file_path).map_err(|e| format!("{e}"))?;
        } else {
            download_file(&url, &file_path)?;
        }

        let digest = compute_file_sha256(&file_path)?;
        if digest != asset.sha256 {
            return Err(format!("Plugin package sha256 mismatch for {plugin_id}"));
        }
        let actual_size = file_size(&file_path)?;
        if actual_size != asset.size {
            return Err(format!("Plugin package size mismatch for {plugin_id}"));
        }
        Ok((file_path, digest, actual_size))
    }

    /// TS `checkForUpdates` 单项 try 块：验签取清单 → 与已装版本比较。
    fn probe_one(&self, item: &PluginCatalogItem) -> Result<PluginUpdateProbe, String> {
        let manifest = self.load_verified_manifest(item)?;
        let current = self.state.installed.get(&item.id).map(|s| s.version.clone());
        let update_available = match &current {
            Some(version) => semver::compare_semver(&manifest.version, version)? > 0,
            None => false,
        };
        Ok(PluginUpdateProbe {
            plugin_id: item.id.clone(),
            checked_at: now_millis(),
            latest_version: Some(manifest.version),
            update_available,
            reason: if update_available {
                "new-version-available"
            } else {
                "up-to-date"
            }
            .to_string(),
        })
    }

    /// TS `checkForUpdates`：逐目录项探测；单项失败不中断（reason = check-failed）。
    pub fn check_for_updates(&mut self, plugin_id: Option<&str>) -> Result<Vec<PluginUpdateProbe>, String> {
        let catalog = list_plugin_catalog();
        let targets: Vec<_> = match plugin_id {
            Some(id) => {
                let found: Vec<_> = catalog.into_iter().filter(|item| item.id == id).collect();
                if found.is_empty() {
                    return Err(format!("Plugin not found: {id}"));
                }
                found
            }
            None => catalog,
        };

        let mut probes = Vec::new();
        for item in targets {
            // 单项全流程（取清单/验签/版本比较）任一步失败都落入 check-failed
            // 探测（TS try/catch 同款），不中断其他插件。
            let probe = match self.probe_one(&item) {
                Ok(probe) => probe,
                Err(error) => PluginUpdateProbe {
                    plugin_id: item.id.clone(),
                    checked_at: now_millis(),
                    latest_version: None,
                    update_available: false,
                    reason: format!("check-failed: {error}"),
                },
            };
            self.update_probes.insert(item.id.clone(), probe.clone());
            probes.push(probe);
        }
        Ok(probes)
    }

    /// TS `install`：验签 → 下载/复制 → 校验 → 落状态（enabled = true）。
    pub fn install(&mut self, plugin_id: &str) -> Result<InstalledPluginState, String> {
        let item = find_catalog_item(plugin_id)?;
        let manifest = self.load_verified_manifest(&item)?;
        let asset = manifest
            .package_asset()
            .ok_or_else(|| format!("No package asset found for plugin {plugin_id}"))?;
        // 借用检查：asset 属于 manifest，先克隆再进 &mut self 路径
        let asset = asset.clone();

        let (file_path, digest, size) = self.download_and_verify_asset(&asset, plugin_id)?;
        let installed_state = InstalledPluginState {
            plugin_id: plugin_id.to_string(),
            version: manifest.version.clone(),
            package_path: file_path.to_string_lossy().to_string(),
            sha256: digest,
            size,
            installed_at: now_millis(),
            enabled: true,
            granted_permissions: Self::resolve_declared_permissions(&item, Some(&manifest)),
        };

        self.state
            .installed
            .insert(plugin_id.to_string(), installed_state.clone());
        self.update_probes.insert(
            plugin_id.to_string(),
            PluginUpdateProbe {
                plugin_id: plugin_id.to_string(),
                checked_at: now_millis(),
                latest_version: Some(manifest.version),
                update_available: false,
                reason: "installed".to_string(),
            },
        );
        self.persist()?;
        Ok(installed_state)
    }

    /// TS `upgrade`：须已安装（不含 dev-source 兜底）；余同 install，探测 reason = upgraded。
    pub fn upgrade(&mut self, plugin_id: &str) -> Result<InstalledPluginState, String> {
        if !self.state.installed.contains_key(plugin_id) {
            return Err(format!("Plugin is not installed: {plugin_id}"));
        }
        let upgraded = self.install(plugin_id)?;
        self.update_probes.insert(
            plugin_id.to_string(),
            PluginUpdateProbe {
                plugin_id: plugin_id.to_string(),
                checked_at: now_millis(),
                latest_version: Some(upgraded.version.clone()),
                update_available: false,
                reason: "upgraded".to_string(),
            },
        );
        self.persist()?;
        Ok(upgraded)
    }

    /// TS `setEnabled`。
    pub fn set_enabled(&mut self, plugin_id: &str, enabled: bool) -> Result<InstalledPluginState, String> {
        let Some(installed) = self.state.installed.get_mut(plugin_id) else {
            return Err(format!("Plugin is not installed: {plugin_id}"));
        };
        installed.enabled = enabled;
        let installed = installed.clone();
        self.persist()?;
        Ok(installed)
    }

    /// TS `listMarket`：目录 + 安装态 + 探测聚合；未安装时 dev-source 兜底展示。
    pub fn list_market(&self) -> Vec<PluginMarketItem> {
        list_plugin_catalog()
            .into_iter()
            .map(|item| {
                let installed = self
                    .state
                    .installed
                    .get(&item.id)
                    .cloned()
                    .or_else(|| self.build_dev_source_installed_state(&item));
                let probe = self.update_probes.get(&item.id);
                PluginMarketItem {
                    installed: installed.is_some(),
                    enabled: installed.as_ref().is_some_and(|s| s.enabled),
                    installed_version: installed.as_ref().map(|s| s.version.clone()),
                    latest_version: probe.and_then(|p| p.latest_version.clone()),
                    update_available: probe.is_some_and(|p| p.update_available),
                    last_checked_at: probe.map(|p| p.checked_at),
                    last_check_reason: probe
                        .map(|p| p.reason.clone())
                        .unwrap_or_else(|| "not-checked".to_string()),
                    catalog: item,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::trust::tests::{sign_text, test_keypair};

    /// 测试夹具：临时目录 + 服务实例 + 签名密钥。
    struct Fixture {
        _tmp: tempfile::TempDir,
        release_root: PathBuf,
        source_root: PathBuf,
        packages_root: PathBuf,
        state_file: PathBuf,
        pem: String,
        signing_key: ed25519_dalek::SigningKey,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let (pem, signing_key) = test_keypair(42);
            Self {
                release_root: tmp.path().join("dist-market/plugins"),
                source_root: tmp.path().join("plugins-src"),
                packages_root: tmp.path().join("data/plugins"),
                state_file: tmp.path().join("data/plugin-market-state.json"),
                pem,
                signing_key,
                _tmp: tmp,
            }
        }

        fn service(&self) -> PluginMarketService {
            PluginMarketService::new(
                MarketPaths {
                    state_file: self.state_file.clone(),
                    packages_root: self.packages_root.clone(),
                    local_release_roots: vec![self.release_root.clone()],
                    local_source_roots: vec![self.source_root.clone()],
                },
                vec![self.pem.clone()],
            )
        }

        fn release_dir(&self) -> PathBuf {
            self.release_root.join("weibo-core")
        }
    }

    struct ReleaseOpts {
        version: String,
        plugin_id: String,
        domain: String,
        permissions: Option<Vec<String>>,
        tamper_sha256: bool,
        tamper_size: bool,
        bad_signature: bool,
    }

    impl Default for ReleaseOpts {
        fn default() -> Self {
            Self {
                version: "0.1.0".to_string(),
                plugin_id: "weibo-core".to_string(),
                domain: "plugin:weibo-core".to_string(),
                permissions: None,
                tamper_sha256: false,
                tamper_size: false,
                bad_signature: false,
            }
        }
    }

    /// 在 release_root 下造一份与打包脚本同构的发布产物（spkg + 清单 + 签名）。
    fn write_release(fixture: &Fixture, opts: &ReleaseOpts) {
        let dir = fixture.release_dir();
        fs::create_dir_all(&dir).unwrap();

        let file_name = format!("spark-plugin-weibo-core-{}.spkg", opts.version);
        let package_payload = serde_json::json!({
            "pluginId": opts.plugin_id,
            "domain": opts.domain,
            "version": opts.version,
            "files": [{
                "path": "manifest.ts",
                "sha256": "00",
                "size": 1,
                "contentBase64": "AA=="
            }]
        });
        let package_text = format!("{}\n", serde_json::to_string_pretty(&package_payload).unwrap());
        fs::write(dir.join(&file_name), &package_text).unwrap();
        let digest = if opts.tamper_sha256 {
            "ff".repeat(32)
        } else {
            hex::encode(sha2::Sha256::digest(package_text.as_bytes()))
        };
        let size = if opts.tamper_size {
            package_text.len() as u64 + 1
        } else {
            package_text.len() as u64
        };

        let mut manifest = serde_json::json!({
            "pluginId": opts.plugin_id,
            "domain": opts.domain,
            "manifestVersion": 1,
            "version": opts.version,
            "releaseTime": "2026-07-22T00:00:00.000Z",
            "assets": [{
                "kind": "package",
                "fileName": file_name,
                "url": format!("file://{}", dir.join(&file_name).to_string_lossy()),
                "sha256": digest,
                "size": size
            }]
        });
        if let Some(permissions) = &opts.permissions {
            manifest["permissions"] = serde_json::json!(permissions);
        }
        let manifest_text = format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap());
        fs::write(dir.join("update-manifest.json"), &manifest_text).unwrap();

        let signature = if opts.bad_signature {
            let (_, other_key) = test_keypair(7);
            sign_text(&other_key, &manifest_text)
        } else {
            sign_text(&fixture.signing_key, &manifest_text)
        };
        fs::write(dir.join("update-manifest.sig"), format!("{signature}\n")).unwrap();
    }

    fn write_dev_source(fixture: &Fixture) {
        let dir = fixture.source_root.join("weibo-core");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.ts"), "export {};\n").unwrap();
    }

    // --------------------------------------------------------------
    // 启动对账
    // --------------------------------------------------------------

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

    // --------------------------------------------------------------
    // 安装 / 升级 / 启停
    // --------------------------------------------------------------

    #[test]
    fn install_from_local_release_copies_package_and_persists() {
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts::default());
        // 不调 initialize：显式 install 路径（reconcile 已在其他用例覆盖，
        // 若先 initialize，本地 bundle 会被对账直接标记安装）
        let mut service = fixture.service();
        assert!(!service.state.installed.contains_key("weibo-core"));

        let installed = service.install("weibo-core").unwrap();
        assert_eq!(installed.version, "0.1.0");
        assert!(installed.enabled);
        // 包被复制到 packages_root/<id>/packages/
        let copied = fixture
            .packages_root
            .join("weibo-core/packages/spark-plugin-weibo-core-0.1.0.spkg");
        assert_eq!(installed.package_path, copied.to_string_lossy());
        assert!(copied.is_file());
        assert_eq!(service.update_probes["weibo-core"].reason, "installed");

        // 新实例从状态文件恢复（持久化语义）；reconcile 跳过已安装条目
        let mut reloaded = fixture.service();
        reloaded.initialize().unwrap();
        assert!(reloaded.state.installed.contains_key("weibo-core"));
        assert!(reloaded.list_market()[0].installed);
    }

    #[test]
    fn install_normalizes_manifest_permissions() {
        let fixture = Fixture::new();
        write_release(
            &fixture,
            &ReleaseOpts {
                permissions: Some(vec![
                    "org:sync".to_string(),
                    "bogus".to_string(),
                    "identity:sign".to_string(),
                    "org:sync".to_string(),
                ]),
                ..Default::default()
            },
        );
        let mut service = fixture.service();
        service.initialize().unwrap();
        let installed = service.install("weibo-core").unwrap();
        assert_eq!(
            installed.granted_permissions,
            vec![
                "storage:read",
                "storage:write",
                "org:read",
                "proof:verify",
                "org:sync",
                "identity:sign"
            ]
        );
    }

    #[test]
    fn install_rejects_signature_id_domain_and_digest_problems() {
        // 坏签名
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts { bad_signature: true, ..Default::default() });
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("weibo-core").unwrap_err(),
            "Plugin manifest signature verification failed: weibo-core"
        );

        // id 不匹配
        let fixture = Fixture::new();
        write_release(
            &fixture,
            &ReleaseOpts { plugin_id: "evil".to_string(), ..Default::default() },
        );
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("weibo-core").unwrap_err(),
            "Plugin manifest id mismatch: expected weibo-core, got evil"
        );

        // domain 不匹配
        let fixture = Fixture::new();
        write_release(
            &fixture,
            &ReleaseOpts { domain: "plugin:evil".to_string(), ..Default::default() },
        );
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("weibo-core").unwrap_err(),
            "Plugin manifest domain mismatch: expected plugin:weibo-core, got plugin:evil"
        );

        // sha256 不匹配
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts { tamper_sha256: true, ..Default::default() });
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("weibo-core").unwrap_err(),
            "Plugin package sha256 mismatch for weibo-core"
        );
        assert!(!service.state.installed.contains_key("weibo-core"));

        // size 不匹配
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts { tamper_size: true, ..Default::default() });
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("weibo-core").unwrap_err(),
            "Plugin package size mismatch for weibo-core"
        );

        // 未收录插件
        let fixture = Fixture::new();
        let mut service = fixture.service();
        service.initialize().unwrap();
        assert_eq!(
            service.install("nope").unwrap_err(),
            "Plugin not found: nope"
        );
    }

    #[test]
    fn set_enabled_roundtrip_and_upgrade_flow() {
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts::default());
        // 不调 initialize：先验证"未安装不能启停/升级"，再走显式 install
        let mut service = fixture.service();

        // 未安装不能启停/升级
        assert_eq!(
            service.set_enabled("weibo-core", false).unwrap_err(),
            "Plugin is not installed: weibo-core"
        );
        assert_eq!(
            service.upgrade("weibo-core").unwrap_err(),
            "Plugin is not installed: weibo-core"
        );

        service.install("weibo-core").unwrap();
        let disabled = service.set_enabled("weibo-core", false).unwrap();
        assert!(!disabled.enabled);
        let mut reloaded = fixture.service();
        reloaded.initialize().unwrap();
        assert!(!reloaded.state.installed["weibo-core"].enabled);
        assert!(!reloaded.list_market()[0].enabled);

        // 发布 0.2.0 后升级
        write_release(&fixture, &ReleaseOpts { version: "0.2.0".to_string(), ..Default::default() });
        let probes = reloaded.check_for_updates(Some("weibo-core")).unwrap();
        assert!(probes[0].update_available);
        assert_eq!(probes[0].reason, "new-version-available");
        assert_eq!(probes[0].latest_version.as_deref(), Some("0.2.0"));

        let upgraded = reloaded.upgrade("weibo-core").unwrap();
        assert_eq!(upgraded.version, "0.2.0");
        assert_eq!(reloaded.update_probes["weibo-core"].reason, "upgraded");
        assert!(fixture
            .packages_root
            .join("weibo-core/packages/spark-plugin-weibo-core-0.2.0.spkg")
            .is_file());
    }

    // --------------------------------------------------------------
    // 检查更新
    // --------------------------------------------------------------

    #[test]
    fn check_updates_version_compare_and_failure_reasons() {
        // 已装 0.1.0，远端同版 → up-to-date
        let fixture = Fixture::new();
        write_release(&fixture, &ReleaseOpts::default());
        let mut service = fixture.service();
        service.initialize().unwrap(); // reconcile 已安装 0.1.0
        let probes = service.check_for_updates(None).unwrap();
        assert_eq!(probes.len(), 1);
        assert!(!probes[0].update_available);
        assert_eq!(probes[0].reason, "up-to-date");

        // 未知插件 → Plugin not found
        assert_eq!(
            service.check_for_updates(Some("nope")).unwrap_err(),
            "Plugin not found: nope"
        );

        // 清单缺失 → check-failed（不中断、latestVersion 置空）
        let fixture = Fixture::new();
        let mut service = fixture.service();
        service.initialize().unwrap();
        let probes = service.check_for_updates(None).unwrap();
        assert!(!probes[0].update_available);
        assert!(probes[0].latest_version.is_none());
        assert!(probes[0].reason.starts_with("check-failed:"));
        // 失败原因进入列表展示
        assert!(service.list_market()[0].last_check_reason.starts_with("check-failed:"));
    }

    #[test]
    fn http_manifest_url_is_rejected() {
        assert_eq!(
            fetch_text_smart("http://example.com/update-manifest.json").unwrap_err(),
            "Insecure plugin manifest URL is not allowed"
        );
    }

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
        assert!(item["package"]["updateManifestUrl"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/"));

        // InstalledPluginState / PluginUpdateProbe 键名
        let state = serde_json::to_value(InstalledPluginState {
            plugin_id: "weibo-core".to_string(),
            version: "0.1.0".to_string(),
            package_path: "/tmp/x".to_string(),
            sha256: "aa".to_string(),
            size: 1,
            installed_at: 2,
            enabled: true,
            granted_permissions: vec!["org:sync".to_string()],
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
        let probe = serde_json::to_value(PluginUpdateProbe {
            plugin_id: "weibo-core".to_string(),
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

    // --------------------------------------------------------------
    // 真实产物端到端（opt-in，默认跳过）
    // --------------------------------------------------------------

    /// 验证打包脚本（code/plugins/scripts/build-weibo-package.mjs）的真实产物
    /// 经 file:// 全链路（验签 → 复制 → sha256/size 校验 → 落状态）可安装。
    ///
    /// 运行：
    ///   SPARK_MARKET_E2E_RELEASE_DIR=<repo>/code/desktop/dist-market/plugins \
    ///   SPARK_MARKET_E2E_PUBLIC_KEY_PEM="$(cat <repo>/code/desktop/dist-market/plugins/weibo-core/update-manifest.pub.pem)" \
    ///   cargo test e2e_real_release_artifacts
    #[test]
    fn e2e_real_release_artifacts() {
        let (Ok(release_root), Ok(pem)) = (
            std::env::var("SPARK_MARKET_E2E_RELEASE_DIR"),
            std::env::var("SPARK_MARKET_E2E_PUBLIC_KEY_PEM"),
        ) else {
            eprintln!("skip e2e_real_release_artifacts: SPARK_MARKET_E2E_* env not set");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let paths = MarketPaths {
            state_file: tmp.path().join("data/plugin-market-state.json"),
            packages_root: tmp.path().join("data/plugins"),
            local_release_roots: vec![PathBuf::from(release_root)],
            // 不挂源码目录：隔离验证"已签名 bundle"路径，不混 bundled-dev-source
            local_source_roots: vec![],
        };

        // 反：内置默认公钥（与本机 .secrets 签名钥不对应）必须拒装该产物
        let mut untrusted = PluginMarketService::new(
            paths.clone(),
            trust::DEFAULT_PLUGIN_PUBLIC_KEYS_PEM
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        untrusted.initialize().unwrap();
        assert!(!untrusted.list_market()[0].installed);
        assert_eq!(
            untrusted.install("weibo-core").unwrap_err(),
            "Plugin manifest signature verification failed: weibo-core"
        );

        // 正：产物自带公钥（env 覆盖场景同款）→ reconcile 直接标记已安装
        let mut service = PluginMarketService::new(paths.clone(), vec![pem]);
        service.initialize().unwrap();
        let items = service.list_market();
        assert!(items[0].installed, "real bundle should reconcile to installed");
        assert_eq!(items[0].last_check_reason, "bundled");

        // 显式 install（file:// 复制到 packages_root）+ .spkg 内部一致性：
        // 逐文件校验 contentBase64 解码后的 sha256/size 与清单一致
        let installed = service.install("weibo-core").unwrap();
        assert!(Path::new(&installed.package_path).is_file());
        let spkg: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&installed.package_path).unwrap(),
        )
        .unwrap();
        assert_eq!(spkg["pluginId"], "weibo-core");
        assert_eq!(spkg["domain"], "plugin:weibo-core");
        for file in spkg["files"].as_array().unwrap() {
            use base64::Engine as _;
            let content = base64::engine::general_purpose::STANDARD
                .decode(file["contentBase64"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                hex::encode(sha2::Sha256::digest(&content)),
                file["sha256"].as_str().unwrap(),
                "spkg file {} sha256 mismatch",
                file["path"]
            );
            assert_eq!(content.len() as u64, file["size"].as_u64().unwrap());
        }

        // 升级流：同版本 check → up-to-date；状态文件已落盘
        let probes = service.check_for_updates(Some("weibo-core")).unwrap();
        assert_eq!(probes[0].reason, "up-to-date");
        assert!(paths.state_file.is_file());
    }
}
