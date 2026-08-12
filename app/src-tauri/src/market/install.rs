//! 安装链路：验签取清单（Ed25519 detached，见 trust.rs）→ 下载 .spkg →
//! 校验 sha256/size → 落状态；含升级与启停。
//!
//! 解耦（plugin_decoupling.md §4）：目录驱动 `install()` 已随目录一起退役，统一收敛到
//! 仓库锚定 `install_from_repo`（repo.rs）。本文件保留与安装/升级共用的工具
//! （清单验签 / 包体校验落盘）与 `upgrade()`（复用 install_from_repo）。

use std::fs;
use std::path::PathBuf;

use sha2::Digest as _;

use super::catalog::PluginCatalogItem;
use super::repo::{HttpRepoFetcher, RepoFetcher, RepoId};
use super::sources::{fetch_text_smart, now_millis};
use super::types::{InstalledPluginState, PluginAsset, PluginReleaseManifest, PluginUpdateProbe};
use super::{PluginMarketService, trust};

/// 清单资产 fileName 消毒（B1）：必须是单段文件名——拒绝绝对路径、`\`、
/// `/`、`..` 段与盘符（`C:`），防任意路径写盘与跨插件覆盖提权。
/// 与 plugin_src.rs / sideload.rs 的路径段校验同思路，但更严：不允许多段。
pub(crate) fn sanitize_asset_file_name(file_name: &str) -> Result<&str, String> {
    let valid = !file_name.is_empty()
        && file_name != "."
        && file_name != ".."
        && !file_name.contains('/')
        && !file_name.contains('\\')
        && !file_name.contains(':')
        && !std::path::Path::new(file_name).is_absolute();
    if valid {
        Ok(file_name)
    } else {
        Err(format!("Plugin asset file name invalid: {file_name}"))
    }
}

impl PluginMarketService {
    /// TS `loadVerifiedManifest`：取清单+签名 → 验签 → 解析 → id/domain 匹配。
    /// 供更新探测（updates.rs probe_one）与目录安装共用；目录安装已退役，仍保留供探测。
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

    /// 由已下载字节落包（repo.rs 仓库锚定链路：包体经抓取层有界读入内存）：
    /// fileName 消毒 → sha256/size 校验（不过不写盘）→ 写盘。
    pub(crate) fn save_verified_package_bytes(
        &self,
        asset: &PluginAsset,
        plugin_id: &str,
        bytes: &[u8],
    ) -> Result<(PathBuf, String, u64), String> {
        let file_name = sanitize_asset_file_name(&asset.file_name)?;
        let digest = hex::encode(sha2::Sha256::digest(bytes));
        if digest != asset.sha256 {
            return Err(format!("Plugin package sha256 mismatch for {plugin_id}"));
        }
        let size = bytes.len() as u64;
        if size != asset.size {
            return Err(format!("Plugin package size mismatch for {plugin_id}"));
        }
        let plugin_dir = self.paths.packages_root.join(plugin_id).join("packages");
        fs::create_dir_all(&plugin_dir).map_err(|e| format!("{e}"))?;
        let file_path = plugin_dir.join(file_name);
        fs::write(&file_path, bytes).map_err(|e| format!("{e}"))?;
        Ok((file_path, digest, size))
    }

    /// TS `upgrade`：须已安装；复用仓库锚定 `install_from_repo`（仓库规范化地址
    /// 才可升级——侧载/短名插件无声明源，其升级路径本就不存在）。
    ///
    /// 解耦（plugin_decoupling.md §4）：目录驱动 install 已退役，整个更新通路
    /// （check → upgrade）数据源统一为仓库锚定，闭环自洽。
    pub fn upgrade(&mut self, plugin_id: &str) -> Result<InstalledPluginState, String> {
        self.upgrade_with(&HttpRepoFetcher, plugin_id)
    }

    /// 供测试注入 fetcher 的 upgrade（镜像 install_from_repo / install_from_repo_with 惯例）。
    pub(crate) fn upgrade_with(
        &mut self,
        fetcher: &dyn RepoFetcher,
        plugin_id: &str,
    ) -> Result<InstalledPluginState, String> {
        if !self.state.installed.contains_key(plugin_id) {
            return Err(format!("Plugin is not installed: {plugin_id}"));
        }
        if RepoId::parse(plugin_id).is_err() {
            return Err(format!("Only repo-anchored plugins can be upgraded: {plugin_id}"));
        }
        let upgraded = self.install_from_repo_with(fetcher, plugin_id)?;
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
