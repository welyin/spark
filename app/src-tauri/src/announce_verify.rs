//! 懒惰核查队列（plugin-dist §8.8，阶段 C 波次 2a）。
//!
//! 订阅内核 P2pEvent：新声明入索引（PluginAnnounceReceived）→ 后台串行核查
//! （市场服务 resolve_repo_plugin，含 §4.4 缓存）→ 终态回写内核索引
//! （verified / failed + 原因，持久化；通过时回写仓库声明文件的
//! name/icon/summary/version 为索引 corrected 展示字段）。只有 verified
//! 条目进入市场视图（波次 2b）。
//!
//! 同时把 PluginAnnounceReceived/Verified 以独立 Tauri 事件
//! `plugin-announce-received` / `plugin-announce-verified` 推渲染端
//! （`p2p-event` 通道之外的便捷别名，载荷相同）。
//!
//! 终态回写绑定核查时读到的 announce.timestamp：核查期间同 id 新声明到达
//! （替换索引条目）则本次结论作废丢弃，防旧结论覆盖新声明。
//!
//! 事件通道 Lagged（广播溢出丢事件）后全量补扫未核查条目；failed 且原因为
//! unreachable 的条目每小时低频重扫（仓库暂时不可达的合法插件延迟出现）。
//!
//! 线程模型：内核/市场 API 均为同步且禁止在 tokio runtime 线程调用，
//! 一律 `spawn_blocking`；内核锁与市场锁从不同时持有（无嵌套、无死锁）。

use std::collections::HashSet;

use spark_core::p2p::P2pEvent;
use spark_core::p2p::plugin_announce::{
    AnnounceVerified, CorrectedAnnounceFields, PluginAnnounceIndexEntry,
};
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;

use crate::{KernelState, MarketState};

/// 单条核查间隔（§8.8：避免对托管平台/镜像源造成请求尖峰）。
const VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// failed(unreachable) 低频重扫间隔（§8.8：核查尽力而为，仓库暂时不可达的
/// 合法插件表现为延迟出现而非拒绝）。
const FAILED_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

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

/// 启动补扫集：未达 verified 终态的条目 id（重启/Lagged 后重核）。
fn backlog_ids(entries: &[PluginAnnounceIndexEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.verified != AnnounceVerified::Verified)
        .map(|e| e.announce.id.clone())
        .collect()
}

/// 低频重扫集：failed 且原因为 unreachable 的条目 id（网络恢复后自动出现）。
fn failed_unreachable_ids(entries: &[PluginAnnounceIndexEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.verified == AnnounceVerified::Failed && e.verify_error == "unreachable")
        .map(|e| e.announce.id.clone())
        .collect()
}

/// 读内核索引（锁失败/内核错误一律按空集处理：补扫是尽力而为）。
fn list_index_entries(app: &tauri::AppHandle) -> Option<Vec<PluginAnnounceIndexEntry>> {
    let state = app.state::<KernelState>();
    let kernel = state.inner().lock().ok()?;
    kernel.list_plugin_announces().ok()
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
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // 丢事件后全量补扫（§8.8 尽力而为收敛）：未核查条目重新入队
                    let app = pump_app.clone();
                    let tx = pump_tx.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(ids) = run_blocking(move || {
                            list_index_entries(&app).map(|entries| backlog_ids(&entries))
                        })
                        .await
                        .flatten()
                        {
                            for id in ids {
                                let _ = tx.send(id);
                            }
                        }
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 核查 worker：串行逐条；启动时先把存量未核查条目入队（重启补核查）
    tauri::async_runtime::spawn(async move {
        let backlog = run_blocking({
            let app = app.clone();
            move || list_index_entries(&app).map(|entries| backlog_ids(&entries))
        })
        .await
        .flatten()
        .unwrap_or_default();
        for id in backlog {
            let _ = queue_tx.send(id);
        }

        // failed(unreachable) 低频重扫定时器（首个 tick 立即触发——跳过，
        // 启动补扫已覆盖；之后每小时扫一次）
        let mut rescan = tokio::time::interval(FAILED_RESCAN_INTERVAL);
        rescan.tick().await;

        // 在队/在处理去重（worker 单消费者，HashSet 无需共享锁）
        let mut queued: HashSet<String> = HashSet::new();
        loop {
            let id = tokio::select! {
                received = queue_rx.recv() => {
                    match received {
                        Some(id) => id,
                        None => break,
                    }
                }
                _ = rescan.tick() => {
                    if let Some(ids) = run_blocking({
                        let app = app.clone();
                        move || list_index_entries(&app).map(|entries| failed_unreachable_ids(&entries))
                    })
                    .await
                    .flatten()
                    {
                        for id in ids {
                            let _ = queue_tx.send(id);
                        }
                    }
                    continue;
                }
            };
            if !queued.insert(id.clone()) {
                continue;
            }
            // 跳过期判定 + 绑定核查时 timestamp：条目不存在或已 verified
            // （含同 id 已被前一次核查收敛）跳过；否则记下 timestamp 供终态回写绑定
            let expect_timestamp = run_blocking({
                let app = app.clone();
                let id = id.clone();
                move || {
                    let state = app.state::<KernelState>();
                    let kernel = state.inner().lock().ok()?;
                    let entry = kernel.get_plugin_announce(&id).ok()??;
                    (entry.verified != AnnounceVerified::Verified).then_some(entry.announce.timestamp)
                }
            })
            .await
            .flatten();
            if let Some(expect_timestamp) = expect_timestamp {
                // 核查：仓库锚定验证（含声明文件缓存；市场锁只在阻塞任务内持有）；
                // 通过时取回声明文件，校正展示字段随终态回写（§8.8）
                let outcome = run_blocking({
                    let app = app.clone();
                    let id = id.clone();
                    move || -> Result<CorrectedAnnounceFields, String> {
                        let state = app.state::<MarketState>();
                        let mut market = state.inner().lock().map_err(|e| format!("{e}"))?;
                        let declaration = market.resolve_repo_plugin(&id)?;
                        Ok(CorrectedAnnounceFields {
                            name: declaration.name,
                            icon: declaration.icon,
                            summary: declaration.summary,
                            version: declaration.version,
                            supported_spaces: declaration.supported_spaces,
                        })
                    }
                })
                .await
                .unwrap_or_else(|| Err("verify task cancelled".to_string()));
                let (verified, error, corrected) = match outcome {
                    Ok(fields) => (true, None, Some(fields)),
                    Err(e) => (false, Some(classify_verify_error(&e)), None),
                };
                // 终态回写（timestamp 不匹配即核查期间有新声明到达，结论作废；
                // 内核索引持久化 + PluginAnnounceVerified 事件由内核发出）
                let _ = run_blocking({
                    let app = app.clone();
                    let id = id.clone();
                    move || {
                        let state = app.state::<KernelState>();
                        let kernel = state.inner().lock().ok()?;
                        kernel
                            .mark_plugin_announce_verified(
                                &id,
                                verified,
                                error.as_deref(),
                                expect_timestamp,
                                corrected,
                            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::p2p::plugin_announce::{
        AnnouncePow, PluginAnnounce, PLUGIN_ANNOUNCE_TYPE,
    };

    fn entry(id: &str, verified: AnnounceVerified, verify_error: &str) -> PluginAnnounceIndexEntry {
        PluginAnnounceIndexEntry {
            announce: PluginAnnounce {
                msg_type: PLUGIN_ANNOUNCE_TYPE.to_string(),
                id: id.to_string(),
                name: "n".to_string(),
                icon: String::new(),
                summary: "s".to_string(),
                category: "business".to_string(),
                version: "0.1.0".to_string(),
                release_url: String::new(),
                timestamp: 1,
                ttl: 2_592_000_000,
                publisher: "0".repeat(64),
                pub_key: String::new(),
                pow: AnnouncePow { bits: 20, nonce: 0 },
                signature: String::new(),
            },
            first_seen_at: 1,
            updated_at: 1,
            verified,
            verify_error: verify_error.to_string(),
            verified_at: 0,
            corrected: None,
        }
    }

    #[test]
    fn classify_verify_error_matrix() {
        assert_eq!(
            classify_verify_error("Repo plugin declaration fetch failed: github.com/a/b"),
            "unreachable"
        );
        assert_eq!(
            classify_verify_error("Repo plugin manifest fetch failed: github.com/a/b"),
            "unreachable"
        );
        assert_eq!(
            classify_verify_error("Repo plugin declaration id mismatch: expected a, got b"),
            "id-mismatch"
        );
        // 其他原因原文保留（截断 200 字符）
        let long = "x".repeat(300);
        assert_eq!(classify_verify_error(&long).chars().count(), 200);
        assert_eq!(classify_verify_error("Repo plugin id invalid: z"), "Repo plugin id invalid: z");
    }

    #[test]
    fn backlog_filters_non_verified() {
        let entries = vec![
            entry("github.com/a/pending", AnnounceVerified::Pending, ""),
            entry("github.com/a/verified", AnnounceVerified::Verified, ""),
            entry("github.com/a/failed", AnnounceVerified::Failed, "id-mismatch"),
        ];
        assert_eq!(
            backlog_ids(&entries),
            vec![
                "github.com/a/pending".to_string(),
                "github.com/a/failed".to_string()
            ]
        );
    }

    #[test]
    fn rescan_filters_failed_unreachable_only() {
        let entries = vec![
            entry("github.com/a/pending", AnnounceVerified::Pending, ""),
            entry("github.com/a/unreachable", AnnounceVerified::Failed, "unreachable"),
            entry("github.com/a/mismatch", AnnounceVerified::Failed, "id-mismatch"),
            entry("github.com/a/verified", AnnounceVerified::Verified, ""),
        ];
        assert_eq!(
            failed_unreachable_ids(&entries),
            vec!["github.com/a/unreachable".to_string()]
        );
    }

    #[test]
    fn dedup_set_semantics() {
        // 在队/在处理去重：同 id 重复入队只核查一次，处理完移除后可再次入队
        let mut queued: HashSet<String> = HashSet::new();
        assert!(queued.insert("github.com/a/b".to_string()));
        assert!(!queued.insert("github.com/a/b".to_string()));
        queued.remove("github.com/a/b");
        assert!(queued.insert("github.com/a/b".to_string()));
    }
}
