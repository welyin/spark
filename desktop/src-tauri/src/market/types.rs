//! 插件市场数据类型（对齐 TS desktop/src/main/plugin-market/types.ts + preload.ts 线形）。
//!
//! 跨越命令边界的出参一律 camelCase（`#[serde(rename_all = "camelCase")]`），
//! 与 TS preload 声明的字段名一致，适配层零加工透传。

use serde::{Deserialize, Serialize};

use super::catalog::PluginCatalogItem;

/// 更新清单中的包资产条目（TS `PluginAsset`）。
#[derive(Clone, Debug, Deserialize)]
pub struct PluginAsset {
    pub kind: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// 插件更新清单（TS `PluginReleaseManifest`）。
///
/// `manifestVersion` / `releaseTime` 旧服务也不消费（类型里有、逻辑未校验），
/// 此处同样不声明（serde 忽略未知字段），避免死字段。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginReleaseManifest {
    pub plugin_id: String,
    pub domain: String,
    pub version: String,
    /// 插件声明的权限清单（可选；缺省时按内置目录声明处理）
    pub permissions: Option<Vec<String>>,
    pub assets: Vec<PluginAsset>,
}

impl PluginReleaseManifest {
    /// 第一个 `kind == "package"` 资产（TS 同款 find 语义）。
    pub fn package_asset(&self) -> Option<&PluginAsset> {
        self.assets.iter().find(|asset| asset.kind == "package")
    }
}

/// 已安装插件状态（TS `InstalledPluginState`；持久化 + 命令出参共用）。
///
/// `granted_permissions` 带 `#[serde(default)]`：兼容旧版状态文件缺字段，
/// 空清单由 initialize 的 backfill 按目录声明回填（基础权限恒授予，
/// 正常安装不会产生真空清单，故"空 = 缺失"判定无损）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPluginState {
    pub plugin_id: String,
    pub version: String,
    pub package_path: String,
    pub sha256: String,
    pub size: u64,
    pub installed_at: u64,
    pub enabled: bool,
    /// 安装时授权并持久化的权限清单（基础 ∪ 声明∩高级）
    #[serde(default)]
    pub granted_permissions: Vec<String>,
}

/// 更新探测结果（TS `PluginUpdateProbe`；仅内存，不持久化）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateProbe {
    pub plugin_id: String,
    pub checked_at: u64,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub reason: String,
}

/// 市场列表条目（TS `PluginMarketItem` = 目录条目 + 安装/探测聚合）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketItem {
    #[serde(flatten)]
    pub catalog: PluginCatalogItem,
    pub installed: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub last_checked_at: Option<u64>,
    pub last_check_reason: String,
}

/// 持久化状态文件形状（TS `PersistedPluginState`：plugin-market-state.json）。
///
/// BTreeMap：落盘键序确定（TS 侧 Record 无序语义，此处仅求稳定可读）。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPluginState {
    #[serde(default)]
    pub installed: std::collections::BTreeMap<String, InstalledPluginState>,
}
