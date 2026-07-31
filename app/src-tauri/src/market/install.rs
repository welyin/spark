//! 安装链路：验签取清单（Ed25519 detached，见 trust.rs）→ file:// 复制 /
//! https 下载 .spkg → 校验 sha256/size → 落状态；含升级与启停。

use std::fs;
use std::path::PathBuf;

use super::catalog::{PluginCatalogItem, find_catalog_item};
use super::sources::{
    compute_file_sha256, download_file, fetch_text_smart, file_size, normalize_file_url, now_millis,
};
use super::types::{InstalledPluginState, PluginAsset, PluginReleaseManifest, PluginUpdateProbe};
use super::{PluginMarketService, trust};

impl PluginMarketService {
    /// TS `loadVerifiedManifest`：取清单+签名 → 验签 → 解析 → id/domain 匹配。
    pub(crate) fn load_verified_manifest(&self, item: &PluginCatalogItem) -> Result<PluginReleaseManifest, String> {
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
    pub(crate) fn download_and_verify_asset(
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
            trust: None,
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
}
