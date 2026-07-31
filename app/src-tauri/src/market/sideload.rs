//! .spkg 侧载导入（plugin_system.md「市场展示与排序 · 网络差降级」，阶段 C 波次 2b）。
//!
//! 两步命令：
//! - inspect：只读解析 .spkg（容器字段 + 包内 manifest.json 的名称/权限），
//!   计算整包 sha256/size，供前端确认对话框「显示包哈希供核对」；
//! - import：读一次字节（哈希复核/解析/落盘写同一份，消换文件窗口）→ 复核
//!   整包哈希（inspect 之后文件被替换即拒）→ 保留 id（system/内置目录）拒载 →
//!   覆盖更高信任安装需显式确认 → 逐文件 sha256/size 校验 → 写 packages 目录
//!   → 落状态（trust = "sideloaded"）。
//!
//! 信任口径：侧载绕过签名信任链与仓库锚定，哈希核对责任在用户（与发布者
//! 公布的哈希比对）；状态显式标记 trust = "sideloaded"，与 signed /
//! repo-anchored 区分。

use std::fs;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::catalog::list_plugin_catalog;
use super::permissions::{normalize_declared_permissions, resolve_granted_permissions};
use super::sources::{file_size, now_millis};
use super::types::{InstalledPluginState, PluginUpdateProbe};
use super::PluginMarketService;

/// .spkg 容器文件条目（与 code/plugins/scripts/build-example-package.mjs 产物同构）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpkgFileEntry {
    path: String,
    sha256: String,
    size: u64,
    content_base64: String,
}

/// .spkg 容器（pluginId/domain/version + 文件清单；未知字段忽略，前向兼容）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpkgContainer {
    pub(crate) plugin_id: String,
    domain: String,
    version: String,
    files: Vec<SpkgFileEntry>,
}

/// 包内 manifest.json 消费字段（名称/权限用于预览与授权；supportedSpaces 用于
/// 市场按空间过滤；其余字段忽略）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpkgInnerManifest {
    name: Option<String>,
    permissions: Option<Vec<String>>,
    pub(crate) supported_spaces: Option<Vec<String>>,
}

/// supportedSpaces 归一化（宽进）：只保留 personal/org，去重；空结果按未声明
/// 处理（None，前端按 ["org"] 过滤，spaces-and-plugins §4）。
/// pub(crate)：service.rs 旧侧载安装态回填复用同一归一化口径。
pub(crate) fn normalize_supported_spaces(raw: Option<Vec<String>>) -> Option<Vec<String>> {
    let mut spaces: Vec<String> = Vec::new();
    for space in raw? {
        if (space == "personal" || space == "org") && !spaces.contains(&space) {
            spaces.push(space);
        }
    }
    if spaces.is_empty() { None } else { Some(spaces) }
}

/// inspect 出参（camelCase，与命令层线形一致）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SideloadPreview {
    pub plugin_id: String,
    pub domain: String,
    pub version: String,
    pub name: String,
    /// 包内 manifest.json 声明的权限（已规范化；缺省空清单）
    pub permissions: Vec<String>,
    /// 包内 manifest.json 声明的支持空间（已规范化；缺省 = 未声明，按 ["org"] 口径）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_spaces: Option<Vec<String>>,
    /// 整包 sha256（前端展示供核对；import 复核）
    pub sha256: String,
    pub size: u64,
    pub file_name: String,
}

/// .spkg 容器大小上限 64 MiB（base64 膨胀后的 bundle 约数 MB，留余量防内存爆）。
const SPKG_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn sideload_invalid(reason: &str) -> String {
    format!("Sideload package invalid: {reason}")
}

/// 插件 id 段校验（与 repo.rs 段规则一致：小写字母数字与 `. _ -`，拒空段与
/// `.`/`..`）；repo 形态 id（含 `/` 段）同样合法，落盘逐段 join 无穿越。
/// 段另排除 Windows 保留设备名（con/nul/aux/prn/com1-9/lpt1-9，按 `.` 前词干
/// 判定——`con.txt` 同属保留）：段字符集已限小写，直接小写比较即可；
/// 保留名目录在 Windows 上不可创建，落盘会莫名失败。
/// pub(crate)：uninstall.rs 复用同一规则校验卸载目标 id。
pub(crate) fn plugin_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.split('/').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 100
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
                && !windows_reserved_segment(segment)
        })
}

/// Windows 保留设备名判定（按 `.` 前词干；段已限小写）。
fn windows_reserved_segment(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    if matches!(stem, "con" | "prn" | "aux" | "nul") {
        return true;
    }
    // com1-9 / lpt1-9
    for prefix in ["com", "lpt"] {
        if let Some(digit) = stem.strip_prefix(prefix) {
            if digit.len() == 1 && digit.chars().all(|c| ('1'..='9').contains(&c)) {
                return true;
            }
        }
    }
    false
}

/// 读取 .spkg 原始字节（扩展名/大小上限校验；import 全程复用同一份字节，
/// 消除"读哈希后文件被换"的 TOCTOU 窗口）。
fn read_container_bytes(path: &Path) -> Result<Vec<u8>, String> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("spkg") {
        return Err(sideload_invalid("not a .spkg file"));
    }
    if file_size(path)? > SPKG_MAX_BYTES {
        return Err(sideload_invalid("package exceeds 64 MiB"));
    }
    fs::read(path).map_err(|e| format!("{e}"))
}

/// 解析 .spkg 容器（JSON 线形/容器字段校验）。
/// pub(crate)：service.rs 旧侧载安装态回填复用同一解析。
pub(crate) fn parse_container(bytes: &[u8]) -> Result<SpkgContainer, String> {
    let container: SpkgContainer =
        serde_json::from_slice(bytes).map_err(|e| sideload_invalid(&format!("{e}")))?;
    if !plugin_id_valid(&container.plugin_id) {
        return Err(sideload_invalid("pluginId invalid"));
    }
    if container.version.is_empty() {
        return Err(sideload_invalid("version empty"));
    }
    if container.files.is_empty() {
        return Err(sideload_invalid("files empty"));
    }
    Ok(container)
}

/// 信任层级排序（signed > repo-anchored > sideloaded）：侧载覆盖更高信任
/// 安装属降级，需用户显式确认（前端按错误前缀弹确认框后重试）。
fn trust_rank(trust: &str) -> u8 {
    match trust {
        "signed" => 3,
        "repo-anchored" => 2,
        _ => 1,
    }
}

/// 解码单个文件条目并校验 sha256/size（逐文件完整性，对齐打包脚本记录口径）。
fn decode_and_verify_entry(entry: &SpkgFileEntry) -> Result<Vec<u8>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&entry.content_base64)
        .map_err(|e| sideload_invalid(&format!("file {}: {e}", entry.path)))?;
    if bytes.len() as u64 != entry.size {
        return Err(sideload_invalid(&format!("file {}: size mismatch", entry.path)));
    }
    if hex::encode(sha2::Sha256::digest(&bytes)) != entry.sha256 {
        return Err(sideload_invalid(&format!("file {}: sha256 mismatch", entry.path)));
    }
    Ok(bytes)
}

/// 取包内 manifest.json（缺失/损坏按 None 处理：名称回落 pluginId、权限空清单）。
/// pub(crate)：service.rs 旧侧载安装态回填复用同一读取。
pub(crate) fn read_inner_manifest(container: &SpkgContainer) -> Option<SpkgInnerManifest> {
    let entry = container.files.iter().find(|f| f.path == "manifest.json")?;
    let bytes = decode_and_verify_entry(entry).ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl PluginMarketService {
    /// 侧载预览（只读）：解析容器 + 计算整包哈希，不改任何状态。
    pub fn inspect_local_package(&self, path: &str) -> Result<SideloadPreview, String> {
        let source = Path::new(path);
        let bytes = read_container_bytes(source)?;
        let container = parse_container(&bytes)?;
        let inner = read_inner_manifest(&container);
        let supported_spaces = inner
            .as_ref()
            .and_then(|m| m.supported_spaces.clone());
        Ok(SideloadPreview {
            plugin_id: container.plugin_id.clone(),
            domain: container.domain.clone(),
            version: container.version.clone(),
            name: inner
                .as_ref()
                .and_then(|m| m.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| container.plugin_id.clone()),
            permissions: inner
                .and_then(|m| m.permissions)
                .map(|raw| normalize_declared_permissions(&raw))
                .unwrap_or_default(),
            supported_spaces: normalize_supported_spaces(supported_spaces),
            sha256: hex::encode(sha2::Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            file_name: source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .ok_or_else(|| sideload_invalid("file name invalid"))?,
        })
    }

    /// 侧载导入：读一次字节（哈希复核/容器解析/落盘写同一份，消换文件窗口）→
    /// 复核整包哈希（preview 后文件被替换即拒）→ 保留 id 与信任降级守卫 →
    /// 逐文件完整性校验 → 写 packages 目录 → 落状态（trust = "sideloaded"）。
    /// `confirm_overwrite`：覆盖既有更高信任安装（signed/repo-anchored）时须
    /// 传 true（前端经确认对话框取得用户同意后重试）。
    pub fn import_local_package(
        &mut self,
        path: &str,
        expected_sha256: &str,
        confirm_overwrite: bool,
    ) -> Result<InstalledPluginState, String> {
        let source = Path::new(path);
        let bytes = read_container_bytes(source)?;
        let digest = hex::encode(sha2::Sha256::digest(&bytes));
        if digest != expected_sha256 {
            return Err(
                "Sideload package changed since preview: sha256 mismatch".to_string(),
            );
        }
        let container = parse_container(&bytes)?;

        // 保留 id 拒载（I2）：system 与内置目录 id 不允许侧载顶替
        if container.plugin_id == "system"
            || list_plugin_catalog().iter().any(|c| c.id == container.plugin_id)
        {
            return Err(format!(
                "Sideload import refused: reserved plugin id {}",
                container.plugin_id
            ));
        }
        // 信任降级覆盖守卫（I2）：既有安装 trust 更高需显式确认
        if let Some(existing) = self.state.installed.get(&container.plugin_id) {
            let existing_trust = existing.trust.as_deref().unwrap_or("signed");
            if trust_rank(existing_trust) > trust_rank("sideloaded") && !confirm_overwrite {
                return Err(format!(
                    "Sideload overwrite requires confirmation: existing install trust={existing_trust} for {}",
                    container.plugin_id
                ));
            }
        }

        let size = bytes.len() as u64;
        // 逐文件完整性校验：条目记录与内容一致才可信（容器可能被篡改）
        for entry in &container.files {
            decode_and_verify_entry(entry)?;
        }
        let inner = read_inner_manifest(&container);
        let declared = inner
            .as_ref()
            .and_then(|m| m.permissions.clone())
            .map(|raw| normalize_declared_permissions(&raw))
            .unwrap_or_default();
        let supported_spaces = normalize_supported_spaces(inner.and_then(|m| m.supported_spaces));

        let file_name = source
            .file_name()
            .ok_or_else(|| sideload_invalid("file name invalid"))?;
        let plugin_dir = self
            .paths
            .packages_root
            .join(&container.plugin_id)
            .join("packages");
        fs::create_dir_all(&plugin_dir).map_err(|e| format!("{e}"))?;
        let file_path = plugin_dir.join(file_name);
        // 写入与哈希复核同一份字节：inspect/import 之后源文件再被换也不影响落盘内容
        fs::write(&file_path, &bytes).map_err(|e| format!("{e}"))?;

        let installed_state = InstalledPluginState {
            plugin_id: container.plugin_id.clone(),
            version: container.version.clone(),
            package_path: file_path.to_string_lossy().to_string(),
            sha256: digest,
            size,
            installed_at: now_millis(),
            enabled: true,
            granted_permissions: resolve_granted_permissions(&declared),
            trust: Some("sideloaded".to_string()),
            supported_spaces,
        };
        self.state
            .installed
            .insert(container.plugin_id.clone(), installed_state.clone());
        // 显式安装成功 → 清除卸载墓碑
        self.state.uninstalled.remove(&container.plugin_id);
        self.update_probes.insert(
            container.plugin_id.clone(),
            PluginUpdateProbe {
                plugin_id: container.plugin_id.clone(),
                checked_at: now_millis(),
                latest_version: Some(installed_state.version.clone()),
                update_available: false,
                reason: "installed".to_string(),
            },
        );
        if let Err(e) = self.persist() {
            // 持久化失败回滚内存插入，避免内存态与磁盘状态文件不一致
            self.state.installed.remove(&container.plugin_id);
            self.update_probes.remove(&container.plugin_id);
            return Err(e);
        }
        Ok(installed_state)
    }
}

// ------------------------------------------------------------------
// 单测：supportedSpaces 归一化（宽进口径）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::normalize_supported_spaces;

    #[test]
    fn normalize_supported_spaces_matrix() {
        // None / 空数组 / 全非法 → None（按未声明 ["org"] 口径处理）
        for raw in [
            None,
            Some(vec![]),
            Some(vec!["team".to_string()]),
            Some(vec!["TEAM".to_string(), "Personal".to_string()]),
        ] {
            assert_eq!(normalize_supported_spaces(raw), None);
        }
        // 非法值丢弃，合法值保留
        assert_eq!(
            normalize_supported_spaces(Some(vec!["personal".to_string(), "team".to_string()])),
            Some(vec!["personal".to_string()])
        );
        // 重复值去重（保持首次出现顺序）
        assert_eq!(
            normalize_supported_spaces(Some(vec![
                "org".to_string(),
                "org".to_string(),
                "personal".to_string(),
                "personal".to_string(),
            ])),
            Some(vec!["org".to_string(), "personal".to_string()])
        );
    }
}
