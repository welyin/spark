//! 命令层：每个 `#[tauri::command]` 是一个极薄的壳——锁定内核、调用同步 API、
//! 把 `KernelError` 映射为消息字符串（文案与 TS 主进程抛出的 `Error.message` 对齐）。
//!
//! 每个命令的 `*_inner` 核心函数直接接收 `&Kernel` / `&mut Kernel`，
//! 不依赖 Tauri State，单元测试直调。

pub mod data;
pub mod docs;
pub mod dto;
pub mod evidence;
pub mod identity;
pub mod market;
pub mod org;
pub mod p2p;
pub mod plugin;

use std::sync::MutexGuard;

use spark_core::kernel::Kernel;

use crate::KernelState;

/// 锁内核；poison 视为内部错误。
pub(crate) fn lock_kernel<'a>(
    state: &'a tauri::State<'a, KernelState>,
) -> Result<MutexGuard<'a, Kernel>, String> {
    state
        .inner()
        .lock()
        .map_err(|_| "kernel state lock poisoned".to_string())
}

/// KernelError → 前端错误字符串（Display 文案与 TS 对齐）。
pub(crate) fn err(e: spark_core::kernel::KernelError) -> String {
    e.to_string()
}
