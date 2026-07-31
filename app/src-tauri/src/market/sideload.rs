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
struct SpkgFileEntry {
    path: String,
    sha256: String,
    size: u64,
    content_base64: String,
}

/// .spkg 容器（pluginId/domain/version + 文件清单；未知字段忽略，前向兼容）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpkgContainer {
    plugin_id: String,
    domain: String,
    version: String,
    files: Vec<SpkgFileEntry>,
}

/// 包内 manifest.json 消费字段（名称/权限用于预览与授权；其余字段忽略）。
#[derive(Deserialize)]
struct SpkgInnerManifest {
    name: Option<String>,
    permissions: Option<Vec<String>>,
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
        })
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
fn parse_container(bytes: &[u8]) -> Result<SpkgContainer, String> {
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
fn read_inner_manifest(container: &SpkgContainer) -> Option<SpkgInnerManifest> {
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
        let declared = read_inner_manifest(&container)
            .and_then(|m| m.permissions)
            .map(|raw| normalize_declared_permissions(&raw))
            .unwrap_or_default();

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
        };
        self.state
            .installed
            .insert(container.plugin_id.clone(), installed_state.clone());
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
