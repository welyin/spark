//! 设备管理命令：设备清单查询（本机 + 同身份已配对设备）。
//!
//! 设备记录由内核 device 模块维护：本机条目在 p2p start 时采集落库
//! （设备名/操作系统/架构/物理地址），其他设备条目经 device-sync 自设备
//! 通道同步。本命令只做内核视图到壳层 DTO 的直通。

use spark_core::kernel::DeviceView;

use super::lock_kernel;
use crate::KernelState;

/// `devices-list`：设备清单（本机置顶，其余按最近在线证据降序）。
#[tauri::command]
pub fn devices_list(state: tauri::State<'_, KernelState>) -> Result<Vec<DeviceView>, String> {
    lock_kernel(&state)?.devices_list().map_err(|e| e.to_string())
}
