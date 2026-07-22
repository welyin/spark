//! 插件市场内置目录（vendored 自 TS desktop/src/main/plugins/catalog.ts，
//! 与 code/plugins/weibo-core/manifest.ts 的 package 字段保持一致）。
//!
//! 本期为静态 vendored：远端目录服务未排期；新增插件 = 在此追加条目 +
//! 打包脚本/发布 workflow 跟进（见 code/plugins/README.md）。

use serde::Serialize;

/// 目录条目的包元数据（TS `PluginCatalogItem.package`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogPackage {
    pub update_manifest_url: String,
    pub signature_url: String,
    pub package_name: String,
    pub install_command: String,
}

/// 目录条目（TS `PluginCatalogItem`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogItem {
    pub id: String,
    pub domain: String,
    pub name: String,
    pub description: String,
    /// 'foundation' | 'business'（字符串对齐 TS，未用枚举以免破坏线形）
    pub category: String,
    pub version: String,
    pub views: Vec<String>,
    /// 插件声明的权限清单（基础权限无需声明，安装时向用户展示并授权）
    pub permissions: Vec<String>,
    pub package: PluginCatalogPackage,
}

/// 内置目录（TS `CATALOG`；listPluginCatalog 同款深拷贝语义 → 每次返回新 Vec）。
pub fn list_plugin_catalog() -> Vec<PluginCatalogItem> {
    vec![PluginCatalogItem {
        id: "weibo-core".to_string(),
        domain: "plugin:weibo-core".to_string(),
        name: "组织微博基础插件".to_string(),
        description: "单主管理员发帖，组织成员评论/回复，基于插件域独立数据同步。".to_string(),
        category: "foundation".to_string(),
        version: "0.1.0".to_string(),
        views: vec!["default".to_string()],
        permissions: vec!["org:sync".to_string()],
        package: PluginCatalogPackage {
            update_manifest_url:
                "https://github.com/welyin/spark/releases/latest/download/spark-plugin-weibo-core-manifest.json"
                    .to_string(),
            signature_url:
                "https://github.com/welyin/spark/releases/latest/download/spark-plugin-weibo-core-manifest.sig"
                    .to_string(),
            package_name: "spark-plugin-weibo-core-0.1.0.spkg".to_string(),
            install_command: "spark-plugin install spark-plugin-weibo-core-0.1.0.spkg".to_string(),
        },
    }]
}

/// 按 id 查目录条目（TS `findCatalogItem` 的错误文案对齐）。
pub fn find_catalog_item(plugin_id: &str) -> Result<PluginCatalogItem, String> {
    list_plugin_catalog()
        .into_iter()
        .find(|item| item.id == plugin_id)
        .ok_or_else(|| format!("Plugin not found: {plugin_id}"))
}
