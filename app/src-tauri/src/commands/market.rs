//! 插件市场命令：`plugin-market-list` / `plugin-market-check-updates` /
//! `plugin-market-install` / `plugin-market-upgrade` / `plugin-market-set-enabled`
//!（语义对齐 TS desktop/src/main/ipc/plugin-market.ts，全部仅系统域使用）。
//!
//! 与 TS 的差异：旧 IPC 以 `requireSystemDomain(event)` 限制调用方为系统域；
//! Tauri 壳当前只有单一主窗口（系统域），插件以 iframe tab 跑在同窗口内，
//! 域隔离待独立插件窗口排期时一并落地（见 commands/plugin.rs 注记）。
//!
//! 业务逻辑全在 `crate::market::PluginMarketService`（单测直调），此处只做
//! 锁与参数透传。更新探测/安装下载是阻塞网络 IO（市场源不可达时可能卡住数
//! 十秒），全部命令为 async + spawn_blocking，不占命令调用线程；MarketState
//! 为 Arc<Mutex<...>>（lib.rs），克隆句柄移入阻塞任务。

use std::sync::Arc;

use crate::market::repo::SparkPluginDeclaration;
use crate::market::types::{InstalledPluginState, PluginMarketItem, PluginUpdateProbe};
use crate::market::PluginMarketService;
use crate::MarketState;

/// 在阻塞线程上执行市场操作：持锁调用 service，Join/锁失败映射为命令错误。
async fn run_market<T, F>(state: tauri::State<'_, MarketState>, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut PluginMarketService) -> Result<T, String> + Send + 'static,
{
    let market = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = market
            .lock()
            .map_err(|_| "plugin market state lock poisoned".to_string())?;
        f(&mut guard)
    })
    .await
    .map_err(|e| format!("plugin market task join failed: {e}"))?
}

#[tauri::command]
pub async fn plugin_market_list(
    state: tauri::State<'_, MarketState>,
) -> Result<Vec<PluginMarketItem>, String> {
    run_market(state, |svc| Ok(svc.list_market())).await
}

#[tauri::command]
pub async fn plugin_market_check_updates(
    state: tauri::State<'_, MarketState>,
    plugin_id: Option<String>,
) -> Result<Vec<PluginUpdateProbe>, String> {
    run_market(state, move |svc| svc.check_for_updates(plugin_id.as_deref())).await
}

#[tauri::command]
pub async fn plugin_market_install(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
) -> Result<InstalledPluginState, String> {
    run_market(state, move |svc| svc.install(&plugin_id)).await
}

#[tauri::command]
pub async fn plugin_market_upgrade(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
) -> Result<InstalledPluginState, String> {
    run_market(state, move |svc| svc.upgrade(&plugin_id)).await
}

#[tauri::command]
pub async fn plugin_market_set_enabled(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
    enabled: bool,
) -> Result<InstalledPluginState, String> {
    run_market(state, move |svc| svc.set_enabled(&plugin_id, enabled)).await
}

/// 仓库锚定安装前置解析（plugin-dist §4.1）：拉取并校验 spark-plugin.json，
/// 供前端「按仓库地址安装」确认前展示名称/图标/简介/权限。
#[tauri::command]
pub async fn plugin_market_resolve_repo(
    state: tauri::State<'_, MarketState>,
    id: String,
) -> Result<SparkPluginDeclaration, String> {
    run_market(state, move |svc| svc.resolve_repo_plugin(&id)).await
}

/// 仓库锚定安装（plugin-dist §4.2）：声明验证 → 派生资产 → 签名可选验签 /
/// 双源交叉 → 复用现有 sha256 校验下载链路。
#[tauri::command]
pub async fn plugin_market_install_from_repo(
    state: tauri::State<'_, MarketState>,
    id: String,
) -> Result<InstalledPluginState, String> {
    run_market(state, move |svc| svc.install_from_repo(&id)).await
}

// ------------------------------------------------------------------
// .spkg 侧载导入（plugin_system.md「市场展示与排序 · 网络差降级」，波次 2b）：
// inspect 只读预览（含整包哈希供核对）→ import 复核哈希后落状态。
// ------------------------------------------------------------------

use crate::market::sideload::SideloadPreview;

/// 侧载预览：解析 .spkg 容器 + 计算整包 sha256/size，供确认对话框展示核对。
#[tauri::command]
pub async fn plugin_market_inspect_local(
    state: tauri::State<'_, MarketState>,
    path: String,
) -> Result<SideloadPreview, String> {
    run_market(state, move |svc| svc.inspect_local_package(&path)).await
}

/// 侧载导入：复核整包哈希（preview 后文件被替换即拒）→ 保留 id / 信任降级
/// 守卫 → 逐文件校验 → 落状态（trust = "sideloaded"）。
/// `confirm_overwrite`：覆盖既有更高信任安装时前端经确认对话框取得同意后传 true。
#[tauri::command]
pub async fn plugin_market_import_local(
    state: tauri::State<'_, MarketState>,
    path: String,
    expected_sha256: String,
    confirm_overwrite: bool,
) -> Result<InstalledPluginState, String> {
    run_market(state, move |svc| {
        svc.import_local_package(&path, &expected_sha256, confirm_overwrite)
    })
    .await
}

// ------------------------------------------------------------------
// 广播索引（plugin-dist §8，阶段 C 波次 2a）：发布声明 / 索引查询。
// 索引存内核 sled（`mkt:ann:`），内核 API 为同步且禁在 tokio 线程调用；
// 发布命令含 PoW 计算（秒级 CPU），改 async + spawn_blocking
// （announce_verify.rs 同款模式），不占命令调用线程。
// ------------------------------------------------------------------

use spark_core::p2p::plugin_announce::{PluginAnnounceIndexEntry, PluginAnnounceInput};

use super::{err, lock_kernel};
use crate::KernelState;

/// 发布插件声明（开发者模式）：字段校验 → 根身份签名 → 算 PoW → 广播 →
/// 入本地索引。需身份已解锁且 P2P 已启动。
#[tauri::command]
pub async fn plugin_market_announce_publish(
    state: tauri::State<'_, KernelState>,
    input: PluginAnnounceInput,
) -> Result<PluginAnnounceIndexEntry, String> {
    let kernel = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let guard = kernel
            .lock()
            .map_err(|_| "kernel state lock poisoned".to_string())?;
        guard.publish_plugin_announce(&input).map_err(err)
    })
    .await
    .map_err(|e| format!("kernel task join failed: {e}"))?
}

/// 本地广播索引列表（含 verified 状态；市场视图只展示 verified 条目，波次 2b）。
#[tauri::command]
pub fn plugin_market_announce_list(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<PluginAnnounceIndexEntry>, String> {
    lock_kernel(&state)?.list_plugin_announces().map_err(err)
}

/// 单条索引查询（verified 状态查询；id 为规范化线形）。
#[tauri::command]
pub fn plugin_market_announce_get(
    state: tauri::State<'_, KernelState>,
    id: String,
) -> Result<Option<PluginAnnounceIndexEntry>, String> {
    lock_kernel(&state)?.get_plugin_announce(&id).map_err(err)
}

#[cfg(test)]
mod announce_tests {
    use spark_core::kernel::{Kernel, KernelConfig};
    use spark_core::p2p::plugin_announce::PluginAnnounceInput;

    fn input(id: &str) -> PluginAnnounceInput {
        PluginAnnounceInput {
            id: id.to_string(),
            name: "待办".to_string(),
            icon: String::new(),
            summary: "测试".to_string(),
            category: "business".to_string(),
            version: "0.1.0".to_string(),
            release_url: String::new(),
        }
    }

    #[test]
    fn publish_requires_unlocked_identity() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        // 未初始化身份 → 锁定错误先于 P2P 检查
        assert_eq!(
            kernel
                .publish_plugin_announce(&input("github.com/acme/todo"))
                .unwrap_err()
                .to_string(),
            "Root identity is locked"
        );
    }

    #[test]
    fn publish_validates_id_before_p2p_check() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("correct-horse-battery", "alice", None).unwrap();
        // 非法 id → 专用文案（在 P2P 未启动检查之前：先校验字段）
        assert_eq!(
            kernel
                .publish_plugin_announce(&input("example.com/acme/todo"))
                .unwrap_err()
                .to_string(),
            "Plugin announce id invalid: example.com/acme/todo"
        );
        // 合法 id 但 P2P 未启动
        assert_eq!(
            kernel
                .publish_plugin_announce(&input("github.com/acme/todo"))
                .unwrap_err()
                .to_string(),
            "p2p node not started"
        );
        // 索引初始为空
        assert!(kernel.list_plugin_announces().unwrap().is_empty());
    }
}
