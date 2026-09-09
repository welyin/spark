//! 市场服务主体：状态装载、grantedPermissions 回填、旧侧载 supportedSpaces/requires 回填。
//! （解耦 plugin_decoupling.md §4.3：启动不再对账内置 bundle / 源码目录——
//! 已安装状态只由 .spkg 落盘文件 + plugin-market-state.json 决定。）

use std::collections::BTreeMap;
use std::fs;

use super::catalog::PluginCatalogItem;
use super::permissions::basic_permissions;
use super::repo::CachedRepoDeclaration;
use super::sources::normalize_file_url;
use super::state::{read_state_file, write_state_file};
use super::types::{PersistedPluginState, PluginUpdateProbe};
use super::MarketPaths;

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

    /// TS `initialize`：读状态 → 回填授权 → 旧侧载 supportedSpaces/requires 回填。
    /// 解耦后不再做内置目录对账（reconcile_bundled_installed_state 已删除）。
    pub fn initialize(&mut self) -> Result<(), String> {
        self.state = read_state_file(&self.paths.state_file);
        self.backfill_granted_permissions()?;
        self.backfill_sideload_manifest_fields()?;
        Ok(())
    }

    /// 兼容旧版安装状态：缺失 grantedPermissions 时回填基础权限（TS 同名）。
    /// 解耦后无内置目录回填分支（目录已空），统一按 `basic_permissions()` 兜底。
    fn backfill_granted_permissions(&mut self) -> Result<(), String> {
        let mut changed = false;
        for installed in self.state.installed.values_mut() {
            if !installed.granted_permissions.is_empty() {
                continue;
            }
            installed.granted_permissions = basic_permissions();
            changed = true;
        }
        if changed {
            self.persist()?;
        }
        Ok(())
    }

    /// 旧侧载安装态 supportedSpaces/requires/window 回填（spaces-and-plugins §4 与
    /// 规格 §2.1 requires/window 字段的历史数据迁移）：这些字段均后于侧载链路落地，
    /// 旧记录缺值（serde default → None）；对 trust == "sideloaded" 且字段为 None 的
    /// 记录重解析落盘 .spkg 包内 manifest.json 回填。包丢失/解析失败/包内未声明均保持
    /// None（按未声明口径处理），不阻断启动；幂等，包内未声明时每次启动重读一次本地
    /// 文件，代价可忽略故不引入额外迁移标记。
    fn backfill_sideload_manifest_fields(&mut self) -> Result<(), String> {
        let mut changed = false;
        for installed in self.state.installed.values_mut() {
            if installed.trust.as_deref() != Some("sideloaded")
                || (installed.supported_spaces.is_some()
                    && installed.requires.is_some()
                    && installed.window.is_some())
            {
                continue;
            }
            let Ok(bytes) = fs::read(&installed.package_path) else {
                continue;
            };
            let Ok(container) = super::sideload::parse_container(&bytes) else {
                continue;
            };
            // 容器 id 与记录一致才回填（防状态文件指向无关包）
            if container.plugin_id != installed.plugin_id {
                continue;
            }
            let inner = super::sideload::read_inner_manifest(&container);
            if installed.supported_spaces.is_none() {
                let spaces = inner
                    .as_ref()
                    .and_then(|m| m.supported_spaces.clone());
                if let Some(spaces) = super::sideload::normalize_supported_spaces(spaces) {
                    installed.supported_spaces = Some(spaces);
                    changed = true;
                }
            }
            if installed.requires.is_none() {
                let requires = inner.as_ref().and_then(|m| m.requires.clone());
                if let Some(requires) = requires.and_then(super::sideload::normalize_requires) {
                    installed.requires = Some(requires);
                    changed = true;
                }
            }
            if installed.window.is_none() {
                let window = super::catalog::normalize_window(inner.and_then(|m| m.window));
                if let Some(window) = window {
                    installed.window = Some(window);
                    changed = true;
                }
            }
        }
        if changed {
            self.persist()?;
        }
        Ok(())
    }

    pub(crate) fn persist(&self) -> Result<(), String> {
        write_state_file(&self.paths.state_file, &self.state)
    }

    /// TS `resolveManifestEndpoints`：统一取目录条目声明的远端 URL。
    /// 解耦（plugin_decoupling.md §4.3/§4.5）：不再优先读本地 dist-market 发布目录
    /// （本地旁路已删）——更新探测与仓库锚定安装同源，走
    /// `synthesize_catalog_entry` 派生的远端清单 URL，仓库锚定一致。
    pub(crate) fn resolve_manifest_endpoints(&self, item: &PluginCatalogItem) -> (String, String) {
        (
            normalize_file_url(&item.package.update_manifest_url),
            normalize_file_url(&item.package.signature_url),
        )
    }
}
