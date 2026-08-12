//! 更新探测与市场列表聚合：逐目录项验签取清单、与已装版本比较；
//! 单项失败不中断（reason = check-failed）；探测结果仅驻留内存，不持久化。
//!
//! 性能（应用页首次加载慢）：`check_for_updates` 会对每个插件拉取远端
//! manifest+签名并验签（reqwest blocking，connect 5s / 总超时 30s/请求）。
//! 原实现串行逐插件探测且结果每次冷启动都重建，网络差时进入应用页卡很久。
//! 现已做两点优化：
//! - **并行探测**：插件互不依赖，改用线程作用域并发拉取，缩短总耗时；
//! - **TTL 缓存**：TTL 内已探测的插件直接复用内存结果（`update_probes`），
//!   不再重复触网（同 repo_decl_cache 的 10 分钟 TTL 先例）。

use super::catalog::{PluginCatalogItem, list_plugin_catalog};
use super::sources::now_millis;
use super::types::{PluginMarketItem, PluginUpdateProbe};
use super::{PluginMarketService, semver};

/// 更新探测结果的内存 TTL（10 分钟；对齐 repo.rs 声明文件缓存先例）。
/// TTL 内进入应用页复用 `update_probes` 缓存，不重复拉远端 manifest。
const UPDATE_PROBE_TTL_MS: u64 = 10 * 60 * 1000;

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
    ///
    /// 优化：TTL 内已探测的插件直接复用缓存不触网；需要探测的插件**并行**拉取
    /// 远端清单（reqwest blocking 同步调用，多插件并发缩短总耗时）。返回顺序
    /// 对齐目录顺序（目录 + 缓存命中混合后按目录序重排）。
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

        let now = now_millis();
        // 1) 筛出命中 TTL 缓存（无需触网）与待探测的插件。
        //    注意：启动对账写入的占位 probe（reason=bundled / bundled-dev-source）
        //    是本地标记、非真实远端探测结果——即使 checked_at 很新也不参与缓存，
        //    必须真实拉一次远端清单（否则"可更新"永远不生效）。
        let cached_ids: Vec<String> = targets
            .iter()
            .filter(|item| {
                self.update_probes
                    .get(&item.id)
                    .is_some_and(|p| {
                        p.reason != "bundled"
                            && p.reason != "bundled-dev-source"
                            && now.saturating_sub(p.checked_at) < UPDATE_PROBE_TTL_MS
                    })
            })
            .map(|item| item.id.clone())
            .collect();
        let to_probe: Vec<PluginCatalogItem> = targets
            .into_iter()
            .filter(|item| !cached_ids.contains(&item.id))
            .collect();

        // 2) 并行探测待处理插件（线程作用域；reqwest blocking 同步调用，
        //    插件互不依赖可并发。单项全流程失败落 check-failed，不中断其他）。
        if !to_probe.is_empty() {
            // 只读借用：并行线程共用 &self 跑 probe_one（PluginMarketService: Sync）。
            let this: &PluginMarketService = self;
            let fresh: Vec<PluginUpdateProbe> = std::thread::scope(|s| {
                let handles: Vec<_> = to_probe
                    .iter()
                    .map(|item| {
                        s.spawn(move || match this.probe_one(item) {
                            Ok(probe) => probe,
                            Err(error) => PluginUpdateProbe {
                                plugin_id: item.id.clone(),
                                checked_at: now_millis(),
                                latest_version: None,
                                update_available: false,
                                reason: format!("check-failed: {error}"),
                            },
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        // join 失败（探测线程 panic）不中断整体：落 check-failed 占位
                        h.join().unwrap_or_else(|_| PluginUpdateProbe {
                            plugin_id: String::new(),
                            checked_at: now_millis(),
                            latest_version: None,
                            update_available: false,
                            reason: "check-failed: probe task panicked".to_string(),
                        })
                    })
                    .collect()
            });
            for probe in &fresh {
                self.update_probes.insert(probe.plugin_id.clone(), probe.clone());
            }
        }

        // 3) 按目录顺序组装返回（缓存命中 + 本次新探测混合后重排）。
        Ok(list_plugin_catalog()
            .into_iter()
            .filter_map(|item| self.update_probes.get(&item.id).cloned())
            .collect())
    }

    /// TS `listMarket`：目录 + 安装态 + 探测聚合；未安装时 dev-source 兜底展示。
    /// 扩展（plugin-dist 波次 1）：目录外的仓库锚定已装插件按缓存声明文件合成条目。
    pub fn list_market(&self) -> Vec<PluginMarketItem> {
        let mut items: Vec<PluginMarketItem> = list_plugin_catalog()
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
                    granted_permissions: installed
                        .map(|s| s.granted_permissions)
                        .unwrap_or_default(),
                    catalog: item,
                }
            })
            .collect();

        // 仓库锚定安装（无内置目录条目）的已装插件：名称/简介/分类取自声明文件缓存
        for installed in self.state.installed.values() {
            if items.iter().any(|item| item.catalog.id == installed.plugin_id) {
                continue;
            }
            let probe = self.update_probes.get(&installed.plugin_id);
            items.push(PluginMarketItem {
                catalog: super::repo::synthesize_catalog_entry(self, installed),
                installed: true,
                enabled: installed.enabled,
                installed_version: Some(installed.version.clone()),
                latest_version: probe.and_then(|p| p.latest_version.clone()),
                update_available: probe.is_some_and(|p| p.update_available),
                last_checked_at: probe.map(|p| p.checked_at),
                last_check_reason: probe
                    .map(|p| p.reason.clone())
                    .unwrap_or_else(|| "not-checked".to_string()),
                granted_permissions: installed.granted_permissions.clone(),
            });
        }
        items
    }
}
