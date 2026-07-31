//! 懒惰核查队列（plugin-dist §8.8，阶段 C 波次 2a）。
//!
//! 订阅内核 P2pEvent：新声明入索引（PluginAnnounceReceived）→ 后台串行核查
//! （市场服务 resolve_repo_plugin，含 §4.4 缓存）→ 终态回写内核索引
//! （verified / failed + 原因，持久化）。只有 verified 条目进入市场视图（波次 2b）。
//!
//! 同时把 PluginAnnounceReceived/Verified 以独立 Tauri 事件
//! `plugin-announce-received` / `plugin-announce-verified` 推渲染端
//! （`p2p-event` 通道之外的便捷别名，载荷相同）。
//!
//! 线程模型：内核/市场 API 均为同步且禁止在 tokio runtime 线程调用，
//! 一律 `spawn_blocking`；内核锁与市场锁从不同时持有（无嵌套、无死锁）。

use std::collections::HashSet;

use spark_core::p2p::P2pEvent;
use spark_core::p2p::plugin_announce::AnnounceVerified;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;

use crate::{KernelState, MarketState};

/// 单条核查间隔（§8.8：避免对托管平台/镜像源造成请求尖峰）。
const VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// 失败原因归类（§8.7 verifyError 口径：unreachable / id-mismatch / 其他原文截断）。
fn classify_verify_error(raw: &str) -> String {
    if raw.contains("fetch failed") {
        "unreachable".to_string()
    } else if raw.contains("id mismatch") {
        "id-mismatch".to_string()
    } else {
        raw.chars().take(200).collect()
    }
}

/// 内核/市场同步 API 一律经阻塞线程池调用（禁在 tokio runtime 线程直调）。
async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tauri::async_runtime::spawn_blocking(f).await.ok()
}

/// 启动懒惰核查 worker（setup 调用一次；随内核事件通道关闭自然退出）。
pub fn spawn_announce_verify_worker(
    app: tauri::AppHandle,
    mut rx: tokio::sync::broadcast::Receiver<P2pEvent>,
) {
    let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<String>();

    // 事件泵：内核事件 → 核查队列 + 渲染端独立事件
    let pump_app = app.clone();
    let pump_tx = queue_tx.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(P2pEvent::PluginAnnounceReceived { id, publisher }) => {
                    let _ = pump_app.emit(
                        "plugin-announce-received",
                        serde_json::json!({ "id": id, "publisher": publisher }),
                    );
                    let _ = pump_tx.send(id);
                }
                Ok(P2pEvent::PluginAnnounceVerified { id, verified, error }) => {
                    let _ = pump_app.emit(
                        "plugin-announce-verified",
                        serde_json::json!({ "id": id, "verified": verified, "error": error }),
                    );
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 核查 worker：串行逐条；启动时先把存量未核查条目入队（重启补核查）
    tauri::async_runtime::spawn(async move {
        let backlog = run_blocking({
            let app = app.clone();
            move || {
                let state = app.state::<KernelState>();
                let kernel = state.inner().lock().ok()?;
                kernel
                    .list_plugin_announces()
                    .ok()
                    .map(|entries| {
                        entries
                            .into_iter()
                            .filter(|e| e.verified != AnnounceVerified::Verified)
                            .map(|e| e.announce.id)
                            .collect::<Vec<_>>()
                    })
            }
        })
        .await
        .flatten()
        .unwrap_or_default();
        for id in backlog {
            let _ = queue_tx.send(id);
        }

        // 在队/在处理去重（worker 单消费者，HashSet 无需共享锁）
        let mut queued: HashSet<String> = HashSet::new();
        while let Some(id) = queue_rx.recv().await {
            if !queued.insert(id.clone()) {
                continue;
            }
            // 跳过期判定：条目不存在或已 verified（含同 id 已被前一次核查收敛）
            let should_verify = run_blocking({
                let app = app.clone();
                let id = id.clone();
                move || {
                    let state = app.state::<KernelState>();
                    let kernel = state.inner().lock().ok()?;
                    let entry = kernel.get_plugin_announce(&id).ok()??;
                    Some(entry.verified != AnnounceVerified::Verified)
                }
            })
            .await
            .flatten()
            .unwrap_or(false);
            if should_verify {
                // 核查：仓库锚定验证（含声明文件缓存；市场锁只在阻塞任务内持有）
                let outcome = run_blocking({
                    let app = app.clone();
                    let id = id.clone();
                    move || -> Result<(), String> {
                        let state = app.state::<MarketState>();
                        let mut market = state.inner().lock().map_err(|e| format!("{e}"))?;
                        market.resolve_repo_plugin(&id).map(|_| ())
                    }
                })
                .await
                .unwrap_or_else(|| Err("verify task cancelled".to_string()));
                let (verified, error) = match outcome {
                    Ok(()) => (true, None),
                    Err(e) => (false, Some(classify_verify_error(&e))),
                };
                // 终态回写（内核索引持久化 + PluginAnnounceVerified 事件由内核发出）
                let _ = run_blocking({
                    let app = app.clone();
                    let id = id.clone();
                    move || {
                        let state = app.state::<KernelState>();
                        let kernel = state.inner().lock().ok()?;
                        kernel
                            .mark_plugin_announce_verified(&id, verified, error.as_deref())
                            .ok()
                    }
                })
                .await;
            }
            queued.remove(&id);
            tokio::time::sleep(VERIFY_INTERVAL).await;
        }
    });
}
