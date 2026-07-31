//! 市场服务主体：状态装载、grantedPermissions 回填、启动对账
//! （本地 bundle 验签通过标记已安装；插件源码目录标记 bundled-dev-source）。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use super::catalog::{PluginCatalogItem, list_plugin_catalog};
use super::permissions::{basic_permissions, normalize_declared_permissions, resolve_granted_permissions};
use super::repo::CachedRepoDeclaration;
use super::sources::{compute_file_sha256, file_size, normalize_file_url, now_millis, to_file_url};
use super::state::{read_state_file, write_state_file};
use super::types::{
    InstalledPluginState, PersistedPluginState, PluginReleaseManifest, PluginUpdateProbe,
};
use super::{MarketPaths, trust};

/// 插件市场服务（TS `PluginMarketService`）。
pub struct PluginMarketService {
    pub(crate) paths: MarketPaths,
    /// 信任公钥（PEM；启动时由 trust::get_plugin_trust_config 解析注入）。
    pub(crate) trust_keys: Vec<String>,
    pub(crate) state: PersistedPluginState,
    pub(crate) update_probes: BTreeMap<String, PluginUpdateProbe>,
    /// 仓库声明文件内存缓存（TTL 10 分钟；sled 持久化见 repo.rs）
    pub(crate) repo_decl_cache: BTreeMap<String, CachedRepoDeclaration>,
}

impl PluginMarketService {
    pub fn new(paths: MarketPaths, trust_keys: Vec<String>) -> Self {
        Self {
            paths,
            trust_keys,
            state: PersistedPluginState::default(),
            update_probes: BTreeMap::new(),
            repo_decl_cache: BTreeMap::new(),
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
    pub(crate) fn resolve_declared_permissions(
        item: &PluginCatalogItem,
        manifest: Option<&PluginReleaseManifest>,
    ) -> Vec<String> {
        let declared = match manifest.and_then(|m| m.permissions.as_ref()) {
            Some(raw) => normalize_declared_permissions(raw),
            None => item.permissions.clone(),
        };
        resolve_granted_permissions(&declared)
    }

    pub(crate) fn persist(&self) -> Result<(), String> {
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
    pub(crate) fn resolve_manifest_endpoints(&self, item: &PluginCatalogItem) -> (String, String) {
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
    pub(crate) fn build_dev_source_installed_state(&self, item: &PluginCatalogItem) -> Option<InstalledPluginState> {
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
            trust: None,
            supported_spaces: item.supported_spaces.clone(),
        })
    }

    /// TS `reconcileBundledInstalledState`：本地 bundle 验签通过标记已安装；
    /// 其次源码目录标记 bundled-dev-source。坏 bundle 静默跳过，保留显式安装路径。
    fn reconcile_bundled_installed_state(&mut self) -> Result<(), String> {
        let mut changed = false;

        for item in list_plugin_catalog() {
            // 卸载墓碑：显式卸载过的插件不再由对账复活（bundled 与
            // bundled-dev-source 两个登记分支统一跳过；显式安装会清除墓碑）
            if self.state.uninstalled.contains(&item.id) {
                continue;
            }
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
                            trust: None,
                            supported_spaces: item.supported_spaces.clone(),
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
                    trust: None,
                    supported_spaces: item.supported_spaces.clone(),
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

        // 清理化石记录：bundled-dev-source 记录对应的插件已不在目录（如插件更名）
        // 或其源码路径已失效——保留会让插件源服务 404、市场出现无目录孤儿。
        // 仅针对 sha256 == "bundled-dev-source" 的记录；显式安装/repo 安装不受影响。
        let catalog_ids: std::collections::BTreeSet<String> =
            list_plugin_catalog().iter().map(|item| item.id.clone()).collect();
        let stale_ids: Vec<String> = self
            .state
            .installed
            .values()
            .filter(|record| {
                record.sha256 == "bundled-dev-source"
                    && (!catalog_ids.contains(&record.plugin_id)
                        || !PathBuf::from(&record.package_path).is_dir())
            })
            .map(|record| record.plugin_id.clone())
            .collect();
        for id in stale_ids {
            self.state.installed.remove(&id);
            self.update_probes.remove(&id);
            changed = true;
        }

        if changed {
            self.persist()?;
        }
        Ok(())
    }
}
