//! 更新探测与市场列表聚合：逐目录项验签取清单、与已装版本比较；
//! 单项失败不中断（reason = check-failed）；探测结果仅驻留内存，不持久化。

use super::catalog::{PluginCatalogItem, list_plugin_catalog};
use super::sources::now_millis;
use super::types::{PluginMarketItem, PluginUpdateProbe};
use super::{PluginMarketService, semver};

impl PluginMarketService {
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
