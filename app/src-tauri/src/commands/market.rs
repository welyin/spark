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
