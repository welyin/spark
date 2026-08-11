//! 设备管理命令：设备清单查询（本机 + 同身份已配对设备）与撤销。
//!
//! 设备记录由内核 device 模块维护：本机条目在 p2p start 时采集落库
//! （设备名/操作系统/架构/物理地址），其他设备条目经 device-sync 自设备
//! 通道同步。本命令只做内核视图到壳层 DTO 的直通。

use serde_json::Value;
use spark_core::kernel::DeviceView;

use super::dto::{DeviceRevokeResult, RootRevokeDeviceArgs, SecurityLogEntryDto, SecurityLogListArgs, SecurityLogListResult};
use super::lock_kernel;
use crate::KernelState;

/// `devices-list`：设备清单（本机置顶，其余按最近在线证据降序）。
#[tauri::command]
pub fn devices_list(state: tauri::State<'_, KernelState>) -> Result<Vec<DeviceView>, String> {
    lock_kernel(&state)?.devices_list().map_err(|e| e.to_string())
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
pub fn security_log_list(
    state: tauri::State<'_, KernelState>,
    args: SecurityLogListArgs,
) -> Result<SecurityLogListResult, String> {
    let entries = lock_kernel(&state)?
        .security_log_list(args.limit)
        .map_err(|e| e.to_string())?;
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
