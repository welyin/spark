//! 插件市场命令：`plugin-market-list` / `plugin-market-check-updates` /
//! `plugin-market-install` / `plugin-market-upgrade` / `plugin-market-set-enabled`
//!（语义对齐 TS desktop/src/main/ipc/plugin-market.ts，全部仅系统域使用）。
//!
//! 与 TS 的差异：旧 IPC 以 `requireSystemDomain(event)` 限制调用方为系统域；
//! Tauri 壳当前只有单一主窗口（系统域），插件以 iframe tab 跑在同窗口内，
//! 域隔离待独立插件窗口排期时一并落地（见 commands/plugin.rs 注记）。
//!
//! 业务逻辑全在 `crate::market::PluginMarketService`（单测直调），此处只做
//! 锁与参数透传。

use std::sync::MutexGuard;

use crate::market::types::{InstalledPluginState, PluginMarketItem, PluginUpdateProbe};
use crate::market::PluginMarketService;
use crate::MarketState;

/// 锁市场服务；poison 视为内部错误。
fn lock_market<'a>(
    state: &'a tauri::State<'a, MarketState>,
) -> Result<MutexGuard<'a, PluginMarketService>, String> {
    state
        .inner()
        .lock()
        .map_err(|_| "plugin market state lock poisoned".to_string())
}

#[tauri::command]
pub fn plugin_market_list(
    state: tauri::State<'_, MarketState>,
) -> Result<Vec<PluginMarketItem>, String> {
    Ok(lock_market(&state)?.list_market())
}

#[tauri::command]
pub fn plugin_market_check_updates(
    state: tauri::State<'_, MarketState>,
    plugin_id: Option<String>,
) -> Result<Vec<PluginUpdateProbe>, String> {
    lock_market(&state)?.check_for_updates(plugin_id.as_deref())
}

#[tauri::command]
pub fn plugin_market_install(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
) -> Result<InstalledPluginState, String> {
    lock_market(&state)?.install(&plugin_id)
}

#[tauri::command]
pub fn plugin_market_upgrade(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
) -> Result<InstalledPluginState, String> {
    lock_market(&state)?.upgrade(&plugin_id)
}

#[tauri::command]
pub fn plugin_market_set_enabled(
    state: tauri::State<'_, MarketState>,
    plugin_id: String,
    enabled: bool,
) -> Result<InstalledPluginState, String> {
    lock_market(&state)?.set_enabled(&plugin_id, enabled)
}
