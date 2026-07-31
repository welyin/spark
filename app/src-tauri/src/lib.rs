//! Spark 桌面壳（Tauri 2.x）：内嵌 spark-core 内核，向前端暴露命令层。
//!
//! 线程模型：内核全部 API 为同步且禁止在 tokio 线程内调用
//! （内部以 `Handle::block_on` 驱动 P2P）。Tauri 的**同步** command 自动在
//! 独立线程池执行，因此命令层一律使用同步 command + `State<Mutex<Kernel>>`。
//!
//! P2P 事件：`Kernel::subscribe_p2p_events` 的 broadcast Receiver 由 setup 中
//! 的转发任务消费，P2pEvent 结构化序列化（`{kind, data}`）后以 `p2p-event`
//! 全局事件发到 WebView。

// pub 以便 tests/ 下的集成测试（unit_app）按公开 API 直调；私有项保持原可见性。
pub mod commands;
pub mod market;
pub mod plugin_src;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::p2p::P2pEvent;
use tauri::{Emitter, Manager, RunEvent};

/// 内核单例状态（全部命令共享；锁内只做同步调用）。
pub type KernelState = Mutex<Kernel>;

/// 插件市场服务单例状态（壳侧服务，不依赖内核）。
/// Arc 句柄：市场命令改为 async + spawn_blocking（更新探测/安装下载是阻塞网络 IO，
/// 不能占用命令调用线程），需要 'static 的状态克隆移入阻塞任务。
pub type MarketState = Arc<Mutex<market::PluginMarketService>>;

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

/// 数据目录解析：SPARK_DATA_DIR 显式指定优先（单机多开测试，每实例一个），
/// 未设置时走平台默认 app_data_dir。setup 与 plugin:// 协议回调共用，
/// 保证内核数据、市场状态与插件源服务读同一目录。
fn resolve_data_dir(app: &tauri::App) -> Result<PathBuf, std::io::Error> {
    match std::env::var("SPARK_DATA_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir)
                .map_err(|e| std::io::Error::other(format!("SPARK_DATA_DIR create failed: {e}")))?;
            Ok(dir)
        }
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| std::io::Error::other(format!("app_data_dir unavailable: {e}"))),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // 插件源服务：plugin://localhost/<pluginId>/<path>（插件 iframe 沙箱化阶段 A；
        // Windows 上页面实际引用 http://plugin.localhost/...，由 wry 拦截后 revert，
        // 见 plugin_src.rs 的 URL 形态说明）。已安装包（app_data_dir/plugins/<id>/
        // packages/*.spkg）优先，内置开发插件 dist（code/plugins/<id>/dist/）兜底。
        .register_uri_scheme_protocol("plugin", |ctx, request| {
            let data_dir = match std::env::var("SPARK_DATA_DIR") {
                Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
                _ => match ctx.app_handle().path().app_data_dir() {
                    Ok(dir) => dir,
                    Err(_) => {
                        return tauri::http::Response::builder()
                            .status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR)
                            .body(b"app_data_dir unavailable".to_vec())
                            .expect("plugin:// error response build failed");
                    }
                },
            };
            plugin_src::handle_plugin_request(&data_dir, request.uri())
        })
        .setup(|app| {
            // sled 单目录独占，多开必须隔离（resolve_data_dir 注释）
            let data_dir = resolve_data_dir(app)?;
            let app_version = app.package_info().version.to_string();
            let kernel = Kernel::init(KernelConfig {
                data_dir: data_dir.clone(),
                app_version: app_version.clone(),
                // 必须给 p2p 配置：kernel 登录链路（unlock/init/recover）的自动启动
                // 以 config.p2p 存在为开关（ensure_p2p_after_login，"登录即在线"）
                p2p: Some(spark_core::p2p::P2pConfig {
                    app_version,
                    ..Default::default()
                }),
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
            app.manage(MarketState::new(Mutex::new(market)));
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
            commands::org::org_set_gateways,
            commands::org::org_set_public,
            commands::org::org_update_info,
            commands::org::org_update_my_identity,
            commands::org::org_resolve_address,
            commands::org::org_search_known,
            commands::org::org_accept_invite,
            commands::org::org_send_invite,
            commands::org::org_respond_invite,
            commands::org::org_invite_records,
            // 通讯录
            commands::contact::contact_overview,
            commands::contact::contact_update_profile,
            commands::contact::contact_set_blocked,
            commands::contact::contact_remove_friend,
            commands::contact::contact_send_request,
            commands::contact::contact_resolve_request,
            commands::contact::contact_reply_request,
            commands::contact::contact_ask_request,
            commands::contact::contact_tag_create,
            commands::contact::contact_tag_rename,
            commands::contact::contact_tag_delete,
            commands::contact::contact_group_create,
            commands::contact::contact_group_rename,
            commands::contact::contact_group_delete,
            commands::contact::contact_group_move,
            commands::contact::contact_set_group,
            commands::contact::contact_org_group_create,
            commands::contact::contact_org_group_rename,
            commands::contact::contact_org_group_delete,
            commands::contact::contact_org_group_move,
            // 消息
            commands::message::message_list_conversations,
            commands::message::message_list_messages,
            commands::message::message_ensure_direct,
            commands::message::message_send_text,
            commands::message::message_resend,
            commands::message::message_recall,
            commands::message::message_delete,
            commands::message::message_mark_read,
            commands::message::message_set_draft,
            commands::message::message_toggle_pin,
            commands::message::message_toggle_mute,
            commands::message::message_clear,
            commands::message::message_delete_conversation,
            // 应用消息（服务号模型，p2p-messages.md §20）
            commands::message::message_app_send,
            commands::message::message_app_list,
            commands::message::message_app_mark_read,
            commands::message::message_app_delete_conversation,
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
            commands::p2p::p2p_get_dht_mode,
            commands::p2p::p2p_set_dht_mode,
            commands::p2p::p2p_make_node_card,
            commands::p2p::p2p_import_node_card,
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
            // 系统桥接（未读角标 → dock/任务栏徽标）
            commands::system::system_set_badge,
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
