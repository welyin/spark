//! 默认内置插件预装（communication §4.2，A19）：应用资源目录
//! `builtin-plugins/*.spkg` 在启动时安装为市场已安装记录（trust = "builtin"）。
//!
//! 语义：
//! - 未安装且无卸载墓碑 → 安装（grantedPermissions = 基础 ∪ 声明∩高级，与
//!   市场安装授权同口径：默认内置聊天/通讯录声明的 messages/contacts 高危位
//!   随预装授予——预装即「系统默认聊天/通讯录应用」的安装时授权语义）；
//! - 已安装且 trust == "builtin"：版本一致跳过，版本不同（应用升级带了新
//!   内置包）覆盖升级；trust != "builtin"（用户自行安装/侧载/仓库锚定）不动；
//! - 卸载墓碑（state.uninstalled）中的 id 跳过（尊重用户显式卸载）；
//! - 资源目录不存在（dev 未生成内置包）整体跳过，不阻断启动。
//!
//! 与解耦纪律（plugin_decoupling.md §4.2）的关系：预装不恢复「内置 dist 源
//! 服务」——内置插件与市场插件同走「.spkg 落盘 + 市场状态记录」唯一通路，
//! plugin:// 源服务仍只信状态记录（含整包 sha256 复核）。

use std::fs;
use std::path::Path;

use sha2::Digest as _;

use super::permissions::{normalize_declared_permissions, resolve_granted_permissions};
use super::sideload::{normalize_requires, normalize_supported_spaces, parse_container, read_inner_manifest};
use super::sources::now_millis;
use super::types::{InstalledPluginState, PluginUpdateProbe};
use super::PluginMarketService;

/// 资源目录下的默认内置插件目录名（tauri.conf bundle.resources 打入）。
pub const BUILTIN_PLUGINS_DIR: &str = "builtin-plugins";

/// 信任标记：默认内置预装（与 signed / repo-anchored / sideloaded 并列）。
pub const BUILTIN_TRUST: &str = "builtin";

impl PluginMarketService {
    /// 启动预装对账：扫描 `resource_dir/builtin-plugins/*.spkg`，按上述语义
    /// 安装/升级/跳过。单个包损坏跳过不阻断其他包；整体不致命（旧内置 UI
    /// 灰度兜底），错误聚合后返回（调用方记录日志继续启动）。
    pub fn ensure_builtin_plugins(&mut self, resource_dir: &Path) -> Result<(), String> {
        let dir = resource_dir.join(BUILTIN_PLUGINS_DIR);
        if !dir.is_dir() {
            return Ok(());
        }
        let mut errors: Vec<String> = Vec::new();
        let mut changed = false;
        let entries = fs::read_dir(&dir).map_err(|e| format!("{e}"))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("spkg") {
                continue;
            }
            let file_name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => {
                    errors.push(format!("builtin spkg file name invalid: {}", path.display()));
                    continue;
                }
            };
            match self.install_builtin_package(&path, &file_name) {
                Ok(did_change) => changed |= did_change,
                Err(error) => errors.push(error),
            }
        }
        if changed {
            self.persist()?;
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("builtin plugins preinstall partial failure: {}", errors.join("; ")))
        }
    }

    /// 单个内置包对账：返回是否发生状态变更。
    fn install_builtin_package(&mut self, path: &Path, file_name: &str) -> Result<bool, String> {
        let bytes = fs::read(path).map_err(|e| format!("{e}"))?;
        let container = parse_container(&bytes)
            .map_err(|e| format!("builtin package {}: {e}", path.display()))?;
        let plugin_id = container.plugin_id.clone();

        // 卸载墓碑优先（尊重用户显式卸载）
        if self.state.uninstalled.contains(&plugin_id) {
            return Ok(false);
        }
        // 已安装：仅 trust == "builtin" 且版本不同才覆盖升级；其余不动
        if let Some(existing) = self.state.installed.get(&plugin_id) {
            let is_builtin = existing.trust.as_deref() == Some(BUILTIN_TRUST);
            if !is_builtin || existing.version == container.version {
                return Ok(false);
            }
        }

        // 逐文件完整性校验（与侧载同口径）
        for entry in &container.files {
            super::sideload::decode_and_verify_entry(entry)
                .map_err(|e| format!("builtin package {}: {e}", path.display()))?;
        }
        let inner = read_inner_manifest(&container);
        let declared = inner
            .as_ref()
            .and_then(|m| m.permissions.clone())
            .map(|raw| normalize_declared_permissions(&raw))
            .unwrap_or_default();
        let supported_spaces = normalize_supported_spaces(inner.as_ref().and_then(|m| m.supported_spaces.clone()));
        let window = super::catalog::normalize_window(inner.as_ref().and_then(|m| m.window));
        let requires = inner.and_then(|m| m.requires).and_then(normalize_requires);

        let plugin_dir = self.paths.packages_root.join(&plugin_id).join("packages");
        fs::create_dir_all(&plugin_dir).map_err(|e| format!("{e}"))?;
        let file_path = plugin_dir.join(file_name);
        fs::write(&file_path, &bytes).map_err(|e| format!("{e}"))?;
        let digest = hex::encode(sha2::Sha256::digest(&bytes));

        let installed_state = InstalledPluginState {
            plugin_id: plugin_id.clone(),
            version: container.version.clone(),
            package_path: file_path.to_string_lossy().to_string(),
            sha256: digest,
            size: bytes.len() as u64,
            installed_at: now_millis(),
            enabled: true,
            granted_permissions: resolve_granted_permissions(&declared),
            trust: Some(BUILTIN_TRUST.to_string()),
            supported_spaces,
            requires,
            window,
        };
        self.state.installed.insert(plugin_id.clone(), installed_state.clone());
        self.update_probes.insert(
            plugin_id.clone(),
            PluginUpdateProbe {
                plugin_id,
                checked_at: now_millis(),
                latest_version: Some(installed_state.version.clone()),
                update_available: false,
                reason: "installed".to_string(),
            },
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::types::PersistedPluginState;
    use crate::market::MarketPaths;
    use base64::Engine as _;
    use std::path::PathBuf;

    /// 构造独立临时目录对（资源目录 + 数据目录），tag 区分用例避免并行互踩
    fn temp_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("spark-builtin-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let resource_dir = base.join("resources");
        let data_dir = base.join("data");
        fs::create_dir_all(&resource_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        (resource_dir, data_dir)
    }

    fn service(data_dir: &Path) -> PluginMarketService {
        PluginMarketService::new(
            MarketPaths {
                state_file: data_dir.join("plugin-market-state.json"),
                packages_root: data_dir.join("plugins"),
                repo_cache_dir: data_dir.join("plugin-repo-cache"),
            },
            vec![],
        )
    }

    /// 写一个最小 .spkg（含 manifest.json 条目；permissions 可配）
    fn write_builtin_spkg(
        resource_dir: &Path,
        plugin_id: &str,
        version: &str,
        permissions: &[&str],
    ) -> PathBuf {
        let dir = resource_dir.join(BUILTIN_PLUGINS_DIR);
        fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "id": plugin_id,
            "name": plugin_id,
            "permissions": permissions,
        })
        .to_string();
        let manifest_bytes = manifest.as_bytes();
        let main_bytes = b"export {}".as_slice();
        let files = vec![
            serde_json::json!({
                "path": "manifest.json",
                "sha256": hex::encode(sha2::Sha256::digest(manifest_bytes)),
                "size": manifest_bytes.len(),
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(manifest_bytes),
            }),
            serde_json::json!({
                "path": "views/main.js",
                "sha256": hex::encode(sha2::Sha256::digest(main_bytes)),
                "size": main_bytes.len(),
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(main_bytes),
            }),
        ];
        let container = serde_json::json!({
            "pluginId": plugin_id,
            "domain": format!("plugin:{plugin_id}"),
            "version": version,
            "files": files,
        });
        let path = dir.join(format!("{plugin_id}-{version}.spkg"));
        fs::write(&path, container.to_string()).unwrap();
        path
    }

    #[test]
    fn preinstall_installs_absent_plugin_with_declared_permissions() {
        let (resource_dir, data_dir) = temp_dirs("install");
        write_builtin_spkg(&resource_dir, "spark-chat", "0.1.0", &["messages:read", "messages:write"]);
        let mut svc = service(&data_dir);
        svc.state = PersistedPluginState::default();

        svc.ensure_builtin_plugins(&resource_dir).unwrap();

        let installed = svc.state.installed.get("spark-chat").unwrap();
        assert_eq!(installed.trust.as_deref(), Some("builtin"));
        assert!(installed.enabled);
        assert!(installed.granted_permissions.contains(&"messages:read".to_string()));
        assert!(installed.granted_permissions.contains(&"messages:write".to_string()));
        // 基础权限恒授予
        assert!(installed.granted_permissions.contains(&"storage:read".to_string()));
        // 包落盘 + 状态持久化（plugin:// 源服务可定位）
        assert!(Path::new(&installed.package_path).is_file());
        assert!(svc.paths.state_file.is_file());
        // 幂等：再跑一轮无变化
        svc.ensure_builtin_plugins(&resource_dir).unwrap();
        assert_eq!(svc.state.installed.len(), 1);
    }

    #[test]
    fn preinstall_upgrades_builtin_when_version_changes() {
        let (resource_dir, data_dir) = temp_dirs("upgrade");
        write_builtin_spkg(&resource_dir, "spark-chat", "0.1.0", &["messages:read"]);
        let mut svc = service(&data_dir);
        svc.ensure_builtin_plugins(&resource_dir).unwrap();
        assert_eq!(svc.state.installed["spark-chat"].version, "0.1.0");

        // 应用升级带了新内置包：覆盖升级
        write_builtin_spkg(&resource_dir, "spark-chat", "0.2.0", &["messages:read", "messages:write"]);
        svc.ensure_builtin_plugins(&resource_dir).unwrap();
        let installed = &svc.state.installed["spark-chat"];
        assert_eq!(installed.version, "0.2.0");
        assert!(installed.granted_permissions.contains(&"messages:write".to_string()));
    }

    #[test]
    fn preinstall_respects_uninstall_tombstone_and_foreign_trust() {
        let (resource_dir, data_dir) = temp_dirs("respect");
        write_builtin_spkg(&resource_dir, "spark-chat", "0.1.0", &["messages:read"]);
        write_builtin_spkg(&resource_dir, "spark-contacts", "0.1.0", &["contacts:read"]);
        let mut svc = service(&data_dir);

        // 用户显式卸载过 spark-chat：墓碑在，预装跳过
        svc.state.uninstalled.insert("spark-chat".to_string());
        // 用户自行安装（sideloaded）的 spark-contacts：不动
        svc.state.installed.insert(
            "spark-contacts".to_string(),
            InstalledPluginState {
                plugin_id: "spark-contacts".to_string(),
                version: "9.9.9".to_string(),
                package_path: "user-package.spkg".to_string(),
                sha256: "x".to_string(),
                size: 1,
                installed_at: 0,
                enabled: true,
                granted_permissions: vec![],
                trust: Some("sideloaded".to_string()),
                supported_spaces: None,
                requires: None,
                window: None,
            },
        );

        svc.ensure_builtin_plugins(&resource_dir).unwrap();
        assert!(!svc.state.installed.contains_key("spark-chat"));
        assert_eq!(svc.state.installed["spark-contacts"].version, "9.9.9");
    }

    #[test]
    fn preinstall_skips_missing_resource_dir_and_corrupt_package() {
        let (resource_dir, data_dir) = temp_dirs("skip");
        let mut svc = service(&data_dir);
        // 资源目录下无 builtin-plugins：整体跳过
        svc.ensure_builtin_plugins(&resource_dir).unwrap();
        assert!(svc.state.installed.is_empty());

        // 坏包：报错但不落状态（其他包不受影响的语义由循环内逐包隔离保证）
        let dir = resource_dir.join(BUILTIN_PLUGINS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("broken.spkg"), b"{ not json").unwrap();
        write_builtin_spkg(&resource_dir, "spark-chat", "0.1.0", &[]);
        let result = svc.ensure_builtin_plugins(&resource_dir);
        assert!(result.is_err());
        assert!(svc.state.installed.contains_key("spark-chat"));
    }
}
