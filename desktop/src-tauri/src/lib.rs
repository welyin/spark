//! Spark 桌面壳（Tauri 2.x）：内嵌 spark-core 内核，向前端暴露命令层。
//!
//! 线程模型：内核全部 API 为同步且禁止在 tokio 线程内调用
//! （内部以 `Handle::block_on` 驱动 P2P）。Tauri 的**同步** command 自动在
//! 独立线程池执行，因此命令层一律使用同步 command + `State<Mutex<Kernel>>`。
//!
//! P2P 事件：`Kernel::subscribe_p2p_events` 的 broadcast Receiver 由 setup 中
//! 的转发任务消费，P2pEvent 结构化序列化（`{kind, data}`）后以 `p2p-event`
//! 全局事件发到 WebView。

mod commands;
mod market;

use std::sync::Mutex;

use serde_json::{json, Value};
use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::p2p::P2pEvent;
use tauri::{Emitter, Manager, RunEvent};

/// 内核单例状态（全部命令共享；锁内只做同步调用）。
pub type KernelState = Mutex<Kernel>;

/// 插件市场服务单例状态（壳侧服务，不依赖内核）。
pub type MarketState = Mutex<market::PluginMarketService>;

/// P2pEvent → 前端载荷：`{kind, data?}`（serde 相邻标签；序列化失败保底
/// `{kind:"Unknown", raw:Debug}`，事件流不因单事件中断）。
fn p2p_event_payload(event: &P2pEvent) -> Value {
    serde_json::to_value(event)
        .unwrap_or_else(|_| json!({ "kind": "Unknown", "raw": format!("{event:?}") }))
}

/// 把内核事件通道桥接到 WebView：慢订阅丢事件时上报 Lagged。
fn spawn_p2p_event_forwarder(app: tauri::AppHandle, mut rx: tokio::sync::broadcast::Receiver<P2pEvent>) {
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit("p2p-event", p2p_event_payload(&event));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let _ = app.emit("p2p-event", json!({ "kind": "Lagged", "skipped": skipped }));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| std::io::Error::other(format!("app_data_dir unavailable: {e}")))?;
            let kernel = Kernel::init(KernelConfig {
                data_dir: data_dir.clone(),
                app_version: app.package_info().version.to_string(),
                p2p: None,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
            let events = kernel.subscribe_p2p_events();
            app.manage(KernelState::new(kernel));
            spawn_p2p_event_forwarder(app.handle().clone(), events);
            // 插件市场：状态/包目录在 app_data_dir，本地 dist-market 与插件源码
            // 目录按编译期 crate 位置解析（见 market::MarketPaths::for_app）；
            // initialize = 读状态 → 回填授权 → 启动对账。
            let mut market = market::PluginMarketService::new(
                market::MarketPaths::for_app(&data_dir),
                market::trust::get_plugin_trust_config(),
            );
            market
                .initialize()
                .map_err(|e| std::io::Error::other(format!("plugin market init failed: {e}")))?;
            app.manage(MarketState::new(market));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 身份全组
            commands::identity::root_status,
            commands::identity::root_list_identities,
            commands::identity::root_init,
            commands::identity::root_unlock,
            commands::identity::root_lock,
            commands::identity::root_set_active,
            commands::identity::root_recover_mnemonic,
            commands::identity::root_recover_backup,
            commands::identity::root_backup_payload,
            commands::identity::root_reveal_mnemonic,
            commands::identity::root_update_profile,
            commands::identity::root_current_identity,
            commands::identity::root_sign,
            commands::identity::root_derive_domain,
            commands::identity::root_mnemonic_check,
            // 文档
            commands::docs::doc_get,
            commands::docs::doc_put,
            commands::docs::doc_delete,
            commands::docs::doc_query,
            commands::docs::doc_declare_collection,
            // 组织
            commands::org::org_list_mine,
            commands::org::org_create,
            commands::org::org_create_invite,
            commands::org::org_join_by_invite,
            commands::org::org_check_join,
            commands::org::org_sync_overview,
            commands::org::org_delete,
            commands::org::org_add_member,
            commands::org::org_remove_member,
            commands::org::org_accept_invite,
            // 数据治理
            commands::data::data_usage,
            commands::data::data_cleanup_now,
            commands::data::data_export,
            commands::data::data_purge_preview,
            commands::data::data_purge_execute,
            // 存证
            commands::evidence::evidence_head_hash,
            commands::evidence::evidence_verify,
            commands::evidence::evidence_entry,
            // P2P
            commands::p2p::p2p_start,
            commands::p2p::p2p_stop,
            commands::p2p::p2p_status,
            commands::p2p::p2p_broadcast,
            commands::p2p::p2p_clear_peer_records,
            commands::p2p::p2p_sync_peer_organizations,
            commands::p2p::p2p_list_peer_records,
            // 插件运行时（tab 模式语义，见 commands/plugin.rs 注记）
            commands::plugin::plugin_identity_sign,
            commands::plugin::plugin_identity_verify,
            commands::plugin::plugin_org_sync_now,
            // 插件市场（目录/检查更新/安装/升级/启停）
            commands::market::plugin_market_list,
            commands::market::plugin_market_check_updates,
            commands::market::plugin_market_install,
            commands::market::plugin_market_upgrade,
            commands::market::plugin_market_set_enabled,
        ])
        .build(tauri::generate_context!())
        .expect("error while building spark desktop");

    app.run(|app_handle, event| {
        // 退出前优雅关闭内核（停 P2P、flush sled，释放文件锁）。
        if let RunEvent::ExitRequested { .. } = event {
            if let Some(state) = app_handle.try_state::<KernelState>() {
                if let Ok(mut kernel) = state.lock() {
                    let _ = kernel.shutdown();
                }
            }
        }
    });
}
