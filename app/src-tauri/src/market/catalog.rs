//! 插件市场内置目录（解耦后为空）。
//!
//! 生产壳层零内置插件知识（plugin_decoupling.md §4.1）：插件只能经「仓库锚定
//! 安装 / 广播索引探索 / .spkg 侧载」进入，目录完全动态发现。`list_plugin_catalog()`
//! 恒返回空；`find_catalog_item()` 恒报「未收录」。类型仍保留：repo.rs 的
//! `synthesize_catalog_entry` 与 updates.rs 合成条目仍需用 `PluginCatalogItem`。

use serde::{Deserialize, Serialize};

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
/// 插件运行时前提（与 SDK PluginRequires 对齐；壳层安装/启用校验用）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRequires {
    /// 需要的系统能力子集（permissions 的超集校验；如 system:exec / network:fetch）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// 明确限定平台（缺省 = 全平台）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// 移动端只读豁免：声明后移动端可安装但禁用写能力（默认 false）
    #[serde(default)]
    pub mobile_readonly: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogItem {
    pub id: String,
    pub domain: String,
    pub name: String,
    pub description: String,
    /// 展示分类：'ai-assistant' | 'social' | 'tool' | 'game' | 'foundation'
    /// （字符串对齐 TS，未用枚举以免破坏线形）
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
    /// 运行时前提（平台/能力约束；None = 无约束，全平台可装）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<PluginRequires>,
    pub package: PluginCatalogPackage,
}

/// 当前壳层平台（requires.platforms 校验口径）：Android/iOS 为 "mobile"，
/// 其余（Windows/macOS/Linux）为 "desktop"（spaces-and-plugins §4）。
pub fn current_platform() -> &'static str {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        "mobile"
    } else {
        "desktop"
    }
}

/// requires.platforms 归一化（侧载包内 manifest 宽进口径）：只保留
/// desktop/mobile、按首次出现去重；非法值丢弃。
pub fn normalize_requires_platforms(raw: &[String]) -> Vec<String> {
    let mut platforms: Vec<String> = Vec::new();
    for value in raw {
        if (value == "desktop" || value == "mobile") && !platforms.contains(value) {
            platforms.push(value.clone());
        }
    }
    platforms
}

/// 平台约束强制点（安装/更新/侧载共用）：空清单 = 无约束全平台可装；
/// 否则当前平台必须在声明列表中，不在则拒绝（spaces-and-plugins §4）。
pub(crate) fn ensure_platform_supported(plugin_id: &str, platforms: &[String]) -> Result<(), String> {
    ensure_platform_supported_on(plugin_id, platforms, current_platform())
}

/// 与 [`ensure_platform_supported`] 同逻辑，平台作参数注入（单测确定性强）。
fn ensure_platform_supported_on(plugin_id: &str, platforms: &[String], platform: &str) -> Result<(), String> {
    if platforms.is_empty() || platforms.iter().any(|p| p == platform) {
        Ok(())
    } else {
        Err(format!(
            "Plugin platform unsupported: {plugin_id} requires platforms [{}], current platform is {platform}",
            platforms.join(", ")
        ))
    }
}

/// 内置目录（解耦后恒空：生产壳层零内置插件知识）。
/// 每次调用返回新 Vec（保持旧「深拷贝」语义，方便调用方持有）。
pub fn list_plugin_catalog() -> Vec<PluginCatalogItem> {
    vec![]
}

/// 按 id 查目录条目（TS `findCatalogItem` 的错误文案对齐）。
/// 解耦后目录恒空 → 恒报「未收录」。
pub fn find_catalog_item(plugin_id: &str) -> Result<PluginCatalogItem, String> {
    Err(format!("Plugin not found: {plugin_id}"))
}

// ------------------------------------------------------------------
// 单测：平台归一化 / 平台约束校验矩阵
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_requires_platforms_matrix() {
        // 非法值丢弃、合法值保留、去重保序
        assert_eq!(
            normalize_requires_platforms(&[
                "mobile".to_string(),
                "watch".to_string(),
                "desktop".to_string(),
                "mobile".to_string(),
            ]),
            vec!["mobile".to_string(), "desktop".to_string()]
        );
        assert!(normalize_requires_platforms(&[]).is_empty());
        assert!(normalize_requires_platforms(&["Desktop".to_string()]).is_empty());
    }

    #[test]
    fn ensure_platform_supported_matrix() {
        // 空清单 = 无约束
        assert!(ensure_platform_supported_on("p", &[], "desktop").is_ok());
        assert!(ensure_platform_supported_on("p", &[], "mobile").is_ok());
        // 命中当前平台
        assert!(
            ensure_platform_supported_on("p", &["desktop".to_string(), "mobile".to_string()], "mobile")
                .is_ok()
        );
        // 不匹配 → 拒绝，错误文案含声明列表与当前平台
        assert_eq!(
            ensure_platform_supported_on("p", &["mobile".to_string()], "desktop").unwrap_err(),
            "Plugin platform unsupported: p requires platforms [mobile], current platform is desktop"
        );
    }

    #[test]
    fn current_platform_matches_compile_target() {
        let expected = if cfg!(any(target_os = "android", target_os = "ios")) {
            "mobile"
        } else {
            "desktop"
        };
        assert_eq!(current_platform(), expected);
    }
}
