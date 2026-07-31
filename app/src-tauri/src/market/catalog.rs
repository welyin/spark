//! 插件市场内置目录（vendored 自 TS desktop/src/main/plugins/catalog.ts，
//! 与 code/plugins/spark-example/manifest.json 的 package 字段保持一致）。
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
    /// 插件支持的空间类型（"personal" / "org"；与插件 manifest.json 的
    /// supportedSpaces 一致；None = 未声明，前端按 ["org"] 处理，
    /// 设计 spaces-and-plugins §4）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_spaces: Option<Vec<String>>,
    pub package: PluginCatalogPackage,
}

/// 内置目录（TS `CATALOG`；listPluginCatalog 同款深拷贝语义 → 每次返回新 Vec）。
pub fn list_plugin_catalog() -> Vec<PluginCatalogItem> {
    vec![PluginCatalogItem {
        id: "spark-example".to_string(),
        domain: "plugin:spark-example".to_string(),
        name: "示例插件".to_string(),
        description: "插件体系参考实现：管理员发帖（域签名防抵赖）发应用会话卡片通知，成员评论/回复。"
            .to_string(),
        category: "foundation".to_string(),
        version: "0.1.1".to_string(),
        views: vec!["default".to_string(), "post-card".to_string()],
        permissions: vec![
            "storage:read".to_string(),
            "storage:write".to_string(),
            "org:read".to_string(),
            "org:sync".to_string(),
            "message:app".to_string(),
            "identity:sign".to_string(),
        ],
        // 与 code/plugins/spark-example/manifest.json 的 supportedSpaces 保持一致
        supported_spaces: Some(vec!["org".to_string()]),
        package: PluginCatalogPackage {
            update_manifest_url:
                "https://github.com/welyin/spark/releases/latest/download/spark-plugin-spark-example-manifest.json"
                    .to_string(),
            signature_url:
                "https://github.com/welyin/spark/releases/latest/download/spark-plugin-spark-example-manifest.sig"
                    .to_string(),
            package_name: "spark-plugin-spark-example-0.1.1.spkg".to_string(),
            install_command: "spark-plugin install spark-plugin-spark-example-0.1.1.spkg".to_string(),
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
