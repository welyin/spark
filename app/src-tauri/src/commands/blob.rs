//! blob 层命令（A3）：副本健康度查询 + 配额配置（A2 预留顺接）。
//!
//! 健康度只读聚合（只提醒不处置，personal-data §4.5/Q06 口径）；
//! 配额为本地配置（`blob:quota`，不进同步），`None` 清除回落设备类默认。

use spark_core::kernel::Kernel;
use spark_core::sync::blob::BlobHealth;

use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn blob_health_inner(kernel: &Kernel) -> Result<BlobHealth, String> {
    kernel.blob_health().map_err(err)
}

pub(crate) fn get_blob_quota_inner(kernel: &Kernel) -> Result<u64, String> {
    kernel.get_blob_quota().map_err(err)
}

pub(crate) fn set_blob_quota_inner(kernel: &mut Kernel, bytes: Option<u64>) -> Result<(), String> {
    kernel.set_blob_quota(bytes).map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

/// `root-blob-health`：域级副本健康度摘要（设备数/K/blob 总量/短板数/
/// 最差副本水位/配额水位）。
#[tauri::command]
pub fn root_blob_health(state: tauri::State<'_, KernelState>) -> Result<BlobHealth, String> {
    blob_health_inner(&*lock_kernel(&state)?)
}

/// `root-get-blob-quota`：本机生效配额（字节；用户配置或设备类默认）。
#[tauri::command]
pub fn root_get_blob_quota(state: tauri::State<'_, KernelState>) -> Result<u64, String> {
    get_blob_quota_inner(&*lock_kernel(&state)?)
}

/// `root-set-blob-quota`：设置用户配额（字节；`None` 清除回落默认）。
#[tauri::command]
pub fn root_set_blob_quota(
    state: tauri::State<'_, KernelState>,
    bytes: Option<u64>,
) -> Result<(), String> {
    set_blob_quota_inner(&mut *lock_kernel(&state)?, bytes)
}

// ------------------------------------------------------------------
// 单元测试
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    const PASSWORD: &str = "correct-horse-battery";

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity(PASSWORD, "alice", None).unwrap();
        (dir, kernel)
    }

    /// 配额三态：未配置（设备类默认）→ 已配置 → 清除回落默认。
    #[test]
    fn quota_three_states() {
        let (_dir, mut kernel) = unlocked_kernel();
        let default = get_blob_quota_inner(&kernel).unwrap();
        assert_eq!(
            default,
            spark_core::sync::blob::DEFAULT_QUOTA_PC_BYTES,
            "未配置 = PC 默认 10GiB（测试宿主非 mobile）"
        );
        set_blob_quota_inner(&mut kernel, Some(123_456_789)).unwrap();
        assert_eq!(get_blob_quota_inner(&kernel).unwrap(), 123_456_789);
        set_blob_quota_inner(&mut kernel, None).unwrap();
        assert_eq!(get_blob_quota_inner(&kernel).unwrap(), default);
    }

    /// 健康度：新账号单设备（K=1）、零 blob、配额水位默认。
    #[test]
    fn health_fresh_account() {
        let (_dir, kernel) = unlocked_kernel();
        let h = blob_health_inner(&kernel).unwrap();
        assert_eq!(h.device_count, 1, "新账号单设备");
        assert_eq!(h.k_target, 1, "单设备退化全量（K=1）");
        assert_eq!(h.total_blobs, 0);
        assert_eq!(h.min_full_replicas, None);
        assert_eq!(h.quota.quota_bytes, spark_core::sync::blob::DEFAULT_QUOTA_PC_BYTES);
        assert_eq!(h.quota.over_bytes, 0);
    }
}
