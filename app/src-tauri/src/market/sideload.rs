//! .spkg 侧载导入（plugin_system.md「市场展示与排序 · 网络差降级」，阶段 C 波次 2b）。
//!
//! 两步命令：
//! - inspect：只读解析 .spkg（容器字段 + 包内 manifest.json 的名称/权限），
//!   计算整包 sha256/size，供前端确认对话框「显示包哈希供核对」；
//! - import：复核整包哈希（inspect 之后文件被替换即拒）→ 逐文件 sha256/size
//!   校验 → 复制进 packages 目录 → 落状态（trust = "sideloaded"）。
//!
//! 信任口径：侧载绕过签名信任链与仓库锚定，哈希核对责任在用户（与发布者
//! 公布的哈希比对）；状态显式标记 trust = "sideloaded"，与 signed /
//! repo-anchored 区分。

use std::fs;
use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::permissions::{normalize_declared_permissions, resolve_granted_permissions};
use super::sources::{compute_file_sha256, file_size, now_millis};
use super::types::{InstalledPluginState, PluginUpdateProbe};
use super::PluginMarketService;

/// .spkg 容器文件条目（与 code/plugins/scripts/build-weibo-package.mjs 产物同构）。
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
fn plugin_id_valid(id: &str) -> bool {
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

/// 读取并解析 .spkg 容器（扩展名/大小上限/JSON 线形/容器字段校验）。
fn read_container(path: &Path) -> Result<SpkgContainer, String> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("spkg") {
        return Err(sideload_invalid("not a .spkg file"));
    }
    if file_size(path)? > SPKG_MAX_BYTES {
        return Err(sideload_invalid("package exceeds 64 MiB"));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{e}"))?;
    let container: SpkgContainer =
        serde_json::from_str(&text).map_err(|e| sideload_invalid(&format!("{e}")))?;
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
        let container = read_container(source)?;
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
            sha256: compute_file_sha256(source)?,
            size: file_size(source)?,
            file_name: source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .ok_or_else(|| sideload_invalid("file name invalid"))?,
        })
    }

    /// 侧载导入：复核整包哈希（preview 后文件被替换即拒）→ 逐文件完整性校验 →
    /// 复制进 `<packages_root>/<id>/packages/` → 落状态（trust = "sideloaded"）。
    pub fn import_local_package(
        &mut self,
        path: &str,
        expected_sha256: &str,
    ) -> Result<InstalledPluginState, String> {
        let source = Path::new(path);
        let container = read_container(source)?;
        let digest = compute_file_sha256(source)?;
        if digest != expected_sha256 {
            return Err(
                "Sideload package changed since preview: sha256 mismatch".to_string(),
            );
        }
        let size = file_size(source)?;
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
        fs::copy(source, &file_path).map_err(|e| format!("{e}"))?;

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
                plugin_id: container.plugin_id,
                checked_at: now_millis(),
                latest_version: Some(installed_state.version.clone()),
                update_available: false,
                reason: "installed".to_string(),
            },
        );
        self.persist()?;
        Ok(installed_state)
    }
}
