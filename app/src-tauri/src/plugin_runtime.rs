//! 插件后台运行时编排（壳层侧）：manifest `background` 入口解析 → 内核启停，
//! 生命周期对账（应用启动 / 启用禁用 / 卸载 / 登录后前端触发同步）。
//!
//! 边界（plugin_system.md「后台运行时」）：包形态与市场状态知识留在壳层，
//! 内核只认「插件 id + JS 源码」。脚本读取复用 `plugin_src` 的 fail-closed
//! 解析管线（已安装包只信市场状态记录的 packagePath + 整包 sha256 复核，
//! 内置开发插件 dist 兜底），不引入第二份包读取逻辑。
//!
//! 对账语义：期望集 = 已安装且启用的插件；其中 manifest 声明 `background`
//! 入口的拉起内核常驻线程，未声明的跳过。运行中但已不在期望集（禁用/卸载）
//! 的即时停止。单插件失败（脚本缺失/语法错误）记日志不阻塞其余。

use std::path::Path;

use serde::Deserialize;
use spark_core::kernel::Kernel;

use crate::{KernelState, MarketState};

/// manifest.json 中本模块关心的最小字段（serde 忽略未知字段）。
#[derive(Deserialize)]
struct BackgroundManifest {
    background: Option<String>,
}

/// 解析插件的后台入口源码：manifest 无 `background` 字段（或 manifest 不可读，
/// 与前端 `fetchPluginManifest` 的降级口径一致）返回 `Ok(None)`；声明了入口
/// 但脚本不可读/非 UTF-8 返回 `Err`（声明即承诺，缺失按故障上报而非静默降级）。
fn load_background_script(data_dir: &Path, plugin_id: &str) -> Result<Option<String>, String> {
    let Some((manifest_bytes, _)) =
        crate::plugin_src::resolve_plugin_resource(data_dir, plugin_id, "manifest.json")
    else {
        return Ok(None);
    };
    let manifest: BackgroundManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("plugin manifest parse failed: {e}"))?;
    let Some(entry) = manifest.background else {
        return Ok(None);
    };
    let (script_bytes, _) = crate::plugin_src::resolve_plugin_resource(data_dir, plugin_id, &entry)
        .ok_or_else(|| format!("plugin background entry not found: {plugin_id}/{entry}"))?;
    let script = String::from_utf8(script_bytes)
        .map_err(|e| format!("plugin background entry is not utf-8: {e}"))?;
    Ok(Some(script))
}

/// 后台运行时对账：缺的启动、多余的停止（幂等，各生命周期钩子统一走这里）。
///
/// `installed` 为（插件 id, 是否启用）清单，取自市场状态（含 dev-source
/// 对账登记的记录）。
pub fn sync_background_runtimes(data_dir: &Path, kernel: &mut Kernel, installed: &[(String, bool)]) {
    let desired: Vec<&str> = installed
        .iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(id, _)| id.as_str())
        .collect();
    // 停用/卸载的插件后台即时停止
    for plugin_id in kernel.plugin_background_running_ids() {
        if !desired.contains(&plugin_id.as_str()) {
            let _ = kernel.plugin_stop_background(&plugin_id);
        }
    }
    for plugin_id in desired {
        if kernel.plugin_background_running(plugin_id) {
            continue;
        }
        match load_background_script(data_dir, plugin_id) {
            Ok(Some(script)) => match kernel.plugin_start_background(plugin_id, &script) {
                Ok(()) => eprintln!("[plugin-runtime] started plugin={plugin_id}"),
                Err(error) => {
                    eprintln!("[plugin-runtime] start failed plugin={plugin_id} error={error}")
                }
            },
            Ok(None) => {}
            Err(error) => {
                eprintln!("[plugin-runtime] load failed plugin={plugin_id} error={error}")
            }
        }
    }
}

/// 命令层对账入口：读市场已安装清单 → 内核后台运行时对账。
/// 锁序：先市场锁取清单（即放），后内核锁执行——两锁不嵌套，与
/// `io_lock` 无交集（对账只在命令线程持 `Mutex<Kernel>`）。
pub fn sync_from_market(data_dir: &Path, kernel_state: &KernelState, market_state: &MarketState) {
    let installed: Vec<(String, bool)> = match market_state.lock() {
        Ok(market) => market
            .state
            .installed
            .values()
            .map(|entry| (entry.plugin_id.clone(), entry.enabled))
            .collect(),
        Err(_) => {
            eprintln!("[plugin-runtime] market state lock poisoned, skip sync");
            return;
        }
    };
    let mut kernel = match kernel_state.lock() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("[plugin-runtime] kernel state lock poisoned, skip sync");
            return;
        }
    };
    sync_background_runtimes(data_dir, &mut kernel, &installed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use base64::Engine as _;
    use sha2::Digest as _;
    use spark_core::kernel::KernelConfig;

    /// 构造独立临时数据目录（tag 区分用例，避免并行测试互相踩）
    fn temp_data_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spark-plugin-runtime-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写最小 .spkg（JSON 容器：files 精确匹配 + base64 内容；对齐
    /// plugin-src 测试夹具格式）
    fn write_spkg(spkg_path: &Path, entries: &[(&str, &[u8])]) {
        fs::create_dir_all(spkg_path.parent().unwrap()).unwrap();
        let files: Vec<serde_json::Value> = entries
            .iter()
            .map(|(path, bytes)| {
                serde_json::json!({
                    "path": path,
                    "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
                })
            })
            .collect();
        fs::write(spkg_path, serde_json::json!({ "files": files }).to_string()).unwrap();
    }

    /// 写 plugin-market-state.json（单插件记录；enabled 可配；sha256 按包文件实算）
    fn write_market_state(data_dir: &Path, plugin_id: &str, spkg_path: &Path, enabled: bool) {
        let bytes = fs::read(spkg_path).unwrap();
        let state = serde_json::json!({
            "installed": {
                plugin_id: {
                    "pluginId": plugin_id,
                    "version": "1.0.0",
                    "packagePath": spkg_path.to_string_lossy(),
                    "sha256": hex::encode(sha2::Sha256::digest(&bytes)),
                    "size": bytes.len(),
                    "installedAt": 0,
                    "enabled": enabled,
                    "grantedPermissions": []
                }
            }
        });
        fs::write(data_dir.join("plugin-market-state.json"), state.to_string()).unwrap();
    }

    const MANIFEST_WITH_BG: &str = r#"{"id":"bg-demo","background":"background.js"}"#;
    const ECHO_SCRIPT: &str = "spark.onMessage(function () {});";

    fn install_demo(data_dir: &Path, enabled: bool) {
        let spkg = data_dir.join("plugins/bg-demo/packages/bg-demo-1.0.0.spkg");
        write_spkg(
            &spkg,
            &[
                ("manifest.json", MANIFEST_WITH_BG.as_bytes()),
                ("background.js", ECHO_SCRIPT.as_bytes()),
            ],
        );
        write_market_state(data_dir, "bg-demo", &spkg, enabled);
    }

    fn temp_kernel(data_dir: &Path) -> Kernel {
        Kernel::init(KernelConfig {
            data_dir: data_dir.to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap()
    }

    #[test]
    fn manifest_without_background_yields_none() {
        let data_dir = temp_data_dir("no-bg");
        let spkg = data_dir.join("plugins/bg-demo/packages/bg-demo-1.0.0.spkg");
        write_spkg(&spkg, &[("manifest.json", br#"{"id":"bg-demo"}"#)]);
        write_market_state(&data_dir, "bg-demo", &spkg, true);
        assert_eq!(load_background_script(&data_dir, "bg-demo").unwrap(), None);
    }

    #[test]
    fn background_entry_loaded_from_spkg() {
        let data_dir = temp_data_dir("load");
        install_demo(&data_dir, true);
        let script = load_background_script(&data_dir, "bg-demo").unwrap();
        assert_eq!(script, Some(ECHO_SCRIPT.to_string()));
    }

    #[test]
    fn declared_but_missing_entry_is_error() {
        let data_dir = temp_data_dir("missing-entry");
        let spkg = data_dir.join("plugins/bg-demo/packages/bg-demo-1.0.0.spkg");
        write_spkg(&spkg, &[("manifest.json", MANIFEST_WITH_BG.as_bytes())]);
        write_market_state(&data_dir, "bg-demo", &spkg, true);
        let error = load_background_script(&data_dir, "bg-demo").unwrap_err();
        assert!(
            error.contains("background entry not found"),
            "声明了入口但包内缺失应报错：{error}"
        );
    }

    #[test]
    fn disabled_plugin_entry_not_served() {
        // enabled=false 时源服务 fail-closed 拒服（兜底 dist 无此 id）→ 无后台
        let data_dir = temp_data_dir("disabled");
        install_demo(&data_dir, false);
        assert_eq!(load_background_script(&data_dir, "bg-demo").unwrap(), None);
    }

    #[test]
    fn reconcile_starts_and_stops_by_enabled_flag() {
        let data_dir = temp_data_dir("reconcile");
        install_demo(&data_dir, true);
        let mut kernel = temp_kernel(&data_dir);

        // 启用 → 对账启动后台线程
        sync_background_runtimes(&data_dir, &mut kernel, &[("bg-demo".to_string(), true)]);
        assert!(kernel.plugin_background_running("bg-demo"));

        // 幂等：再对账不重复启动（AlreadyRunning 路径走不到）
        sync_background_runtimes(&data_dir, &mut kernel, &[("bg-demo".to_string(), true)]);
        assert!(kernel.plugin_background_running("bg-demo"));

        // 禁用 → 对账即停
        sync_background_runtimes(&data_dir, &mut kernel, &[("bg-demo".to_string(), false)]);
        assert!(!kernel.plugin_background_running("bg-demo"));
    }

    #[test]
    fn builtin_dist_background_entry_resolves() {
        // 内置开发插件链路：ai-chat dist 声明了 background 入口时应能从
        // CARGO_MANIFEST_DIR/../../plugins 兜底解析到脚本（无市场记录时
        // resolve_plugin_resource 回落内置 dist）。dist 为构建输出，缺失跳过
        // ——与 plugin-src 测试同口径。
        let data_dir = temp_data_dir("builtin-dist");
        match load_background_script(&data_dir, "ai-chat") {
            Ok(Some(script)) => {
                assert!(script.contains("onMessage"), "后台脚本应含消息监听注册");
            }
            Ok(None) => eprintln!("skip: ai-chat dist not built"),
            Err(error) => panic!("ai-chat 声明了入口但解析失败：{error}"),
        }
    }

    #[test]
    fn reconcile_skips_plugin_without_background() {
        let data_dir = temp_data_dir("skip");
        let spkg = data_dir.join("plugins/bg-demo/packages/bg-demo-1.0.0.spkg");
        write_spkg(&spkg, &[("manifest.json", br#"{"id":"bg-demo"}"#)]);
        write_market_state(&data_dir, "bg-demo", &spkg, true);
        let mut kernel = temp_kernel(&data_dir);
        sync_background_runtimes(&data_dir, &mut kernel, &[("bg-demo".to_string(), true)]);
        assert!(!kernel.plugin_background_running("bg-demo"));
    }
}
