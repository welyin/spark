//! 设备管理命令：设备清单查询（本机 + 同身份已配对设备）与撤销。
//!
//! 设备记录由内核 device 模块维护：本机条目在 p2p start 时采集落库
//! （设备名/操作系统/架构/物理地址），其他设备条目经 device-sync 自设备
//! 通道同步。本命令只做内核视图到壳层 DTO 的直通。
//!
//! 线程模型：内核 API 为同步且禁在 tokio runtime 线程直调，但 Tauri 同步
//! command 在主线程执行——`devices_list` 内含 `block_on(local_node_info)`
//! 与兜底采集（`cmd /c ver` 子进程/scrypt 派生），且锁等待在内核忙
//! （同步循环/p2p 入站）时不可控，一律 async + `spawn_blocking` 挪阻塞
//! 线程池（同 `plugin_market_announce_publish` 口径），不占 UI 线程。

use serde_json::Value;
use spark_core::kernel::DeviceView;

use super::dto::{DeviceRevokeResult, RootRevokeDeviceArgs, SecurityLogEntryDto, SecurityLogListArgs, SecurityLogListResult};
use super::lock_kernel;
use crate::KernelState;

/// `devices-list`：设备清单（本机置顶，其余按最近在线证据降序）。
#[tauri::command]
pub async fn devices_list(state: tauri::State<'_, KernelState>) -> Result<Vec<DeviceView>, String> {
    let kernel = std::sync::Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        kernel
            .lock()
            .map_err(|_| "kernel state lock poisoned".to_string())?
            .devices_list()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("devices list task join failed: {e}"))?
}

/// `root-revoke-device`：撤销指定设备（peerId 或 deviceUid）。
#[tauri::command]
pub fn root_revoke_device(
    state: tauri::State<'_, KernelState>,
    args: RootRevokeDeviceArgs,
) -> Result<DeviceRevokeResult, String> {
    lock_kernel(&state)?
        .revoke_device(&args.device_id)
        .map(|_| DeviceRevokeResult { success: true })
        .map_err(|e| e.to_string())
}

/// `security-log-list`：内部调试命令，读取 `security:log:` 前缀 KV。
#[tauri::command]
pub async fn security_log_list(
    state: tauri::State<'_, KernelState>,
    args: SecurityLogListArgs,
) -> Result<SecurityLogListResult, String> {
    let kernel = std::sync::Arc::clone(state.inner());
    let entries = tauri::async_runtime::spawn_blocking(move || {
        kernel
            .lock()
            .map_err(|_| "kernel state lock poisoned".to_string())?
            .security_log_list(args.limit)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("security log task join failed: {e}"))??;
    let mut items = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let parsed: Value = serde_json::from_str(&value).unwrap_or_default();
        items.push(SecurityLogEntryDto {
            key,
            kind: parsed.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
            device_id: parsed.get("deviceId").and_then(Value::as_str).unwrap_or("").to_string(),
            device_name: parsed.get("deviceName").and_then(Value::as_str).map(String::from),
            actor: parsed.get("actor").and_then(Value::as_str).map(String::from),
            ts: parsed.get("ts").and_then(Value::as_i64).unwrap_or(0),
        });
    }
    Ok(SecurityLogListResult { items })
}
