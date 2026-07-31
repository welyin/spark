//! 主程序自动更新命令（tauri-plugin-updater 薄封装）。
//!
//! 更新源为 GitHub Releases 的 `latest.json` 清单（tauri.conf.json `plugins.updater`
//! 配置 endpoints + 公钥）；清单与安装包签名校验由插件完成，本层只做状态编排：
//! `check`（查清单）→ `stage_latest`（下载+验签，字节暂存内存）→ `apply_restart`
//! （安装并重启）。启动后 `spawn_auto_check` 后台自动走 check+stage，就绪后向
//! 前端发 `updater://ready` 事件，由弹窗/关于页引导用户确认重启。
//!
//! 与内核命令不同：不碰 Kernel，状态是壳侧 `UpdaterShared`（std Mutex）。
//! check/stage 是 async 网络调用，锁只在读改状态标记的临界区内持有，不跨 await；
//! `busy` 标记防止 check/stage/apply 并发重入。

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

/// 更新器共享状态（`app.manage` 单例，与 MarketState 同口径的 Arc 包装）。
pub type UpdaterState = Arc<Mutex<UpdaterShared>>;

#[derive(Default)]
pub struct UpdaterShared {
    /// check/stage/apply 互斥标记：任一进行中时其它入口直接报错，避免
    /// 「检查到一半又触发下载」之类的交错。
    busy: bool,
    last_check: Option<LastCheck>,
    staged: Option<StagedUpdate>,
}

struct LastCheck {
    checked_at: i64,
    /// 触发来源（"startup"/"manual"），失败时记录为 "error:<msg>"。
    reason: String,
    available_version: Option<String>,
}

struct StagedUpdate {
    update: Update,
    bytes: Vec<u8>,
    file_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterStatusDto {
    configured: bool,
    app_id: String,
    channel: String,
    current_version: String,
    last_check: Option<LastCheckDto>,
    staged: Option<UpdaterStagedDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastCheckDto {
    checked_at: i64,
    reason: String,
    available_version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterCheckResultDto {
    update_available: bool,
    available_version: Option<String>,
    notes: Option<String>,
    date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterStagedDto {
    file_name: String,
    version: String,
}

/// `updater://ready` 事件载荷（前端弹窗消费）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UpdaterReadyPayload {
    version: String,
    notes: Option<String>,
    date: Option<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 占用互斥标记；已在进行中时报错（文案与前端提示对齐）。
fn begin(shared: &Arc<Mutex<UpdaterShared>>) -> Result<(), String> {
    let mut guard = shared.lock().map_err(|_| "updater state lock poisoned".to_string())?;
    if guard.busy {
        return Err("另一更新任务进行中".to_string());
    }
    guard.busy = true;
    Ok(())
}

fn finish(shared: &Arc<Mutex<UpdaterShared>>) {
    if let Ok(mut guard) = shared.lock() {
        guard.busy = false;
    }
}

fn record_check(
    shared: &Arc<Mutex<UpdaterShared>>,
    reason: String,
    available_version: Option<String>,
) {
    if let Ok(mut guard) = shared.lock() {
        guard.last_check = Some(LastCheck {
            checked_at: now_ms(),
            reason,
            available_version,
        });
    }
}

/// 就绪事件载荷从 Update 提取（date 序列化为 RFC3339 字符串）。
fn ready_payload(update: &Update) -> UpdaterReadyPayload {
    UpdaterReadyPayload {
        version: update.version.clone(),
        notes: update.body.clone(),
        date: update
            .date
            .map(|d| d.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
    }
}

/// 下载文件名取 URL 末段，用于状态展示（下载失败兜底 "<version>.bin"）。
fn staged_file_name(update: &Update) -> String {
    update
        .download_url
        .path_segments()
        .and_then(|mut segs| segs.next_back().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}.bin", update.version))
}

/// 查清单：有更新返回 Some(Update)，并把本次检查记入状态。
async fn check_once(
    app: &AppHandle,
    shared: &Arc<Mutex<UpdaterShared>>,
    reason: &str,
) -> Result<Option<Update>, String> {
    let updater = app
        .updater()
        .map_err(|e| format!("updater unavailable: {e}"))?;
    match updater.check().await {
        Ok(update) => {
            record_check(
                shared,
                reason.to_string(),
                update.as_ref().map(|u| u.version.clone()),
            );
            Ok(update)
        }
        Err(e) => {
            record_check(shared, format!("error:{e}"), None);
            Err(e.to_string())
        }
    }
}

/// 下载+验签并暂存，返回 staged 摘要。
async fn stage_update(
    shared: &Arc<Mutex<UpdaterShared>>,
    update: &Update,
) -> Result<UpdaterStagedDto, String> {
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    let dto = UpdaterStagedDto {
        file_name: staged_file_name(update),
        version: update.version.clone(),
    };
    let mut guard = shared
        .lock()
        .map_err(|_| "updater state lock poisoned".to_string())?;
    guard.staged = Some(StagedUpdate {
        update: update.clone(),
        bytes,
        file_name: dto.file_name.clone(),
    });
    Ok(dto)
}

#[tauri::command]
pub fn updater_status(app: AppHandle, state: tauri::State<'_, UpdaterState>) -> UpdaterStatusDto {
    let guard = state.lock().ok();
    let (last_check, staged) = match guard.as_ref() {
        Some(g) => (
            g.last_check.as_ref().map(|c| LastCheckDto {
                checked_at: c.checked_at,
                reason: c.reason.clone(),
                available_version: c.available_version.clone(),
            }),
            g.staged.as_ref().map(|s| UpdaterStagedDto {
                file_name: s.file_name.clone(),
                version: s.update.version.clone(),
            }),
        ),
        None => (None, None),
    };
    UpdaterStatusDto {
        configured: app.updater().is_ok(),
        app_id: "com.spark.desktop".to_string(),
        channel: "github-releases".to_string(),
        current_version: app.package_info().version.to_string(),
        last_check,
        staged,
    }
}

#[tauri::command]
pub async fn updater_check(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<UpdaterCheckResultDto, String> {
    let shared = state.inner().clone();
    begin(&shared)?;
    let result = check_once(&app, &shared, "manual").await;
    finish(&shared);
    let update = result?;
    Ok(match update {
        Some(u) => {
            let payload = ready_payload(&u);
            UpdaterCheckResultDto {
                update_available: true,
                available_version: Some(payload.version),
                notes: payload.notes,
                date: payload.date,
            }
        }
        None => UpdaterCheckResultDto {
            update_available: false,
            available_version: None,
            notes: None,
            date: None,
        },
    })
}

#[tauri::command]
pub async fn updater_stage_latest(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<UpdaterStagedDto, String> {
    let shared = state.inner().clone();
    begin(&shared)?;
    let result = async {
        let update = check_once(&app, &shared, "manual")
            .await?
            .ok_or_else(|| "当前已是最新版本".to_string())?;
        stage_update(&shared, &update).await
    }
    .await;
    finish(&shared);
    result
}

#[tauri::command]
pub async fn updater_apply_restart(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<(), String> {
    let shared = state.inner().clone();
    begin(&shared)?;
    let staged = {
        let mut guard = shared
            .lock()
            .map_err(|_| "updater state lock poisoned".to_string())?;
        guard
            .staged
            .take()
            .ok_or_else(|| "没有已下载的更新，请先检查更新".to_string())
    };
    let staged = match staged {
        Ok(staged) => staged,
        Err(e) => {
            finish(&shared);
            return Err(e);
        }
    };
    // install 是同步的文件解压/替换，放进阻塞池，不占 async runtime 线程；
    // 失败把安装包带回（字节仍在内存，用户可立即重试，不必重新下载）。
    let result = tauri::async_runtime::spawn_blocking(move || {
        staged
            .update
            .install(&staged.bytes)
            .map_err(|e| (staged, e.to_string()))
    })
    .await;
    finish(&shared);
    match result {
        Ok(Ok(())) => app.restart(),
        Ok(Err((staged, e))) => {
            if let Ok(mut guard) = shared.lock() {
                guard.staged = Some(staged);
            }
            Err(format!("安装失败：{e}（安装包仍保留，可重试）"))
        }
        Err(e) => Err(format!("install task join failed: {e}")),
    }
}

/// 启动后自动检查：静默 check → 有更新自动 stage → 就绪发 `updater://ready`
/// 事件（前端弹「重启安装」对话框）；任何失败只记 last_check，不打扰用户。
pub(crate) fn spawn_auto_check(app: AppHandle, shared: UpdaterState) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        if begin(&shared).is_err() {
            return;
        }
        let staged = async {
            let update = match check_once(&app, &shared, "startup").await {
                Ok(Some(update)) => update,
                _ => return None,
            };
            stage_update(&shared, &update).await.ok().map(|_| update)
        }
        .await;
        finish(&shared);
        if let Some(update) = staged {
            let _ = app.emit("updater://ready", ready_payload(&update));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Mutex<UpdaterShared>> {
        Arc::new(Mutex::new(UpdaterShared::default()))
    }

    #[test]
    fn busy_guard_blocks_reentry() {
        let s = shared();
        begin(&s).expect("first begin should succeed");
        assert_eq!(begin(&s).unwrap_err(), "另一更新任务进行中");
        finish(&s);
        begin(&s).expect("begin after finish should succeed");
    }

    #[test]
    fn record_check_stores_latest() {
        let s = shared();
        record_check(&s, "startup".to_string(), Some("0.2.0".to_string()));
        record_check(&s, "error:timeout".to_string(), None);
        let guard = s.lock().unwrap();
        let last = guard.last_check.as_ref().expect("last_check recorded");
        assert_eq!(last.reason, "error:timeout");
        assert_eq!(last.available_version, None);
        assert!(last.checked_at > 0);
    }

    #[test]
    fn check_result_dto_serializes_camel_case() {
        let dto = UpdaterCheckResultDto {
            update_available: true,
            available_version: Some("0.2.0".to_string()),
            notes: Some("修复若干问题".to_string()),
            date: Some("2026-07-30T00:00:00Z".to_string()),
        };
        let value = serde_json::to_value(&dto).unwrap();
        assert_eq!(value["updateAvailable"], true);
        assert_eq!(value["availableVersion"], "0.2.0");
        assert_eq!(value["notes"], "修复若干问题");
        assert!(value.get("update_available").is_none());
    }

    #[test]
    fn status_dto_serializes_camel_case() {
        let dto = UpdaterStatusDto {
            configured: true,
            app_id: "com.spark.desktop".to_string(),
            channel: "github-releases".to_string(),
            current_version: "0.1.0".to_string(),
            last_check: Some(LastCheckDto {
                checked_at: 123,
                reason: "startup".to_string(),
                available_version: None,
            }),
            staged: Some(UpdaterStagedDto {
                file_name: "Spark.app.tar.gz".to_string(),
                version: "0.2.0".to_string(),
            }),
        };
        let value = serde_json::to_value(&dto).unwrap();
        assert_eq!(value["currentVersion"], "0.1.0");
        assert_eq!(value["lastCheck"]["checkedAt"], 123);
        assert_eq!(value["lastCheck"]["availableVersion"], serde_json::Value::Null);
        assert_eq!(value["staged"]["fileName"], "Spark.app.tar.gz");
    }

    #[test]
    fn ready_payload_serializes_camel_case() {
        let payload = UpdaterReadyPayload {
            version: "0.2.0".to_string(),
            notes: None,
            date: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["version"], "0.2.0");
        assert!(value.get("notes").is_some());
    }
}
