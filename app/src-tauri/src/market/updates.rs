//! 更新探测与市场列表聚合：从已安装状态反查，逐插件验签取清单、与已装版本
//! 比较；单项失败不中断（reason = check-failed）；探测结果仅驻留内存，不持久化。
//!
//! 解耦（plugin_decoupling.md §4.5）：`check_for_updates` 不再查内置目录
//! （`list_plugin_catalog` 已空），改为从 `state.installed` 反查——每个已装插件
//! 经 `repo::synthesize_catalog_entry` 合成目录条目（声明文件缓存 + 派生的远端
//! 清单 URL），走与仓库锚定一致的更新探测。
//!
//! 性能：`check_for_updates` 会对每个插件拉取远端 manifest+签名并验签
//! （reqwest blocking，connect 5s / 总超时 30s/请求）。两点优化：
//! - **并行探测**：插件互不依赖，改用线程作用域并发拉取，缩短总耗时；
//! - **TTL 缓存**：TTL 内已探测的插件直接复用内存结果（`update_probes`），
//!   不再重复触网（同 repo_decl_cache 的 10 分钟 TTL 先例）。

use super::catalog::PluginCatalogItem;
use super::sources::now_millis;
use super::types::{PluginMarketItem, PluginUpdateProbe};
use super::{PluginMarketService, semver};

/// 更新探测结果的内存 TTL（10 分钟；对齐 repo.rs 声明文件缓存先例）。
/// TTL 内进入应用页复用 `update_probes` 缓存，不重复拉远端 manifest。
const UPDATE_PROBE_TTL_MS: u64 = 10 * 60 * 1000;

/// 本地占位 probe reason（安装时写入，非真实远端探测结果，永不参与缓存命中）。
fn is_placeholder_probe(reason: &str) -> bool {
    matches!(reason, "bundled" | "bundled-dev-source" | "installed")
}

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

    /// TS `checkForUpdates`：从已安装状态反查，逐插件探测；单项失败不中断
    /// （reason = check-failed）。
    ///
    /// `plugin_id` 指定单项时：未安装返回 "Plugin is not installed"。不指定则
    /// 遍历全部已装插件。优化：TTL 内已探测的插件直接复用缓存不触网；需要探测
    /// 的插件**并行**拉取远端清单（reqwest blocking 同步调用，多插件并发缩短
    /// 总耗时）。返回顺序对齐已安装顺序。
    pub fn check_for_updates(&mut self, plugin_id: Option<&str>) -> Result<Vec<PluginUpdateProbe>, String> {
        // 从已安装状态反查目标（不再走内置目录 find_catalog_item）
        let installed_ids: Vec<String> = self.state.installed.keys().cloned().collect();
        let targets: Vec<PluginCatalogItem> = match plugin_id {
            Some(id) => {
                let installed = self
                    .state
                    .installed
                    .get(id)
                    .ok_or_else(|| format!("Plugin is not installed: {id}"))?;
                vec![super::repo::synthesize_catalog_entry(self, installed)]
            }
            None => installed_ids
                .into_iter()
                .filter_map(|id| {
                    let installed = self.state.installed.get(&id)?;
                    Some(super::repo::synthesize_catalog_entry(self, installed))
                })
                .collect(),
        };

        let now = now_millis();
        // 1) 筛出命中 TTL 缓存（无需触网）与待探测的插件。
        //    本地占位 probe（reason=bundled/bundled-dev-source/installed）非真实
        //    远端探测结果——即使 checked_at 很新也不参与缓存，必须真实拉一次远端
        //    清单（否则"可更新"永远不生效）。
        let cached_ids: Vec<String> = targets
            .iter()
            .filter(|item| {
                self.update_probes
                    .get(&item.id)
                    .is_some_and(|p| {
                        !is_placeholder_probe(&p.reason)
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

        // 3) 按已安装顺序组装返回（缓存命中 + 本次新探测混合后重排）。
        Ok(self
            .state
            .installed
            .keys()
            .filter_map(|id| self.update_probes.get(id).cloned())
            .collect())
    }

    /// TS `listMarket`：已安装插件（合成条目）+ 探测聚合。
    /// 解耦后无内置目录条目；市场列表 = 已装插件（经声明文件缓存合成，见
    /// `repo::synthesize_catalog_entry`）+ 广播索引探索条目。
    pub fn list_market(&self) -> Vec<PluginMarketItem> {
        let mut items: Vec<PluginMarketItem> = Vec::new();
        for installed in self.state.installed.values() {
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
                // 信任级（L0/L1/L2）由落库 trust 派生，进入市场条目数据模型
                trust_level: Some(super::trust::trust_level_of(installed.trust.as_deref()).to_string()),
            });
        }
        items
    }
}
