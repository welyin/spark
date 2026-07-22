//! 数据治理命令。
//!
//! `data-export` 与 TS 的差异：TS 在主进程弹保存对话框取路径；本层命令接收
//! 显式路径，对话框由前端适配层（tauri-plugin-dialog，后续里程碑）补上。

use serde::Serialize;
use spark_core::data_mgmt::{AutoCleanupResult, DataUsageReport, ExportWriteResult, PurgeResult};
use spark_core::kernel::Kernel;

use super::dto::OrgSyncOverviewDto;
use super::{err, lock_kernel};
use crate::KernelState;

/// `data-purge-preview` 返回（内核 `PurgePreviewInfo` 的 serde 版）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgePreviewInfoDto {
    pub org_id: String,
    pub domain: String,
    pub before_ts: i64,
    pub preview: spark_core::data_mgmt::PurgePreview,
    pub replica: Option<OrgSyncOverviewDto>,
    pub is_current_user_admin: bool,
}

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn usage_inner(kernel: &mut Kernel) -> Result<DataUsageReport, String> {
    kernel.get_usage().map_err(err)
}

pub(crate) fn cleanup_now_inner(kernel: &mut Kernel) -> Result<AutoCleanupResult, String> {
    kernel.run_cleanup_now().map_err(err)
}

pub(crate) fn export_inner(
    kernel: &Kernel,
    file_path: &str,
) -> Result<ExportWriteResult, String> {
    kernel.export_dump(file_path).map_err(err)
}

pub(crate) fn purge_preview_inner(
    kernel: &Kernel,
    org_id: &str,
    before_ts: i64,
) -> Result<PurgePreviewInfoDto, String> {
    let info = kernel.preview_purge(org_id, before_ts).map_err(err)?;
    Ok(PurgePreviewInfoDto {
        org_id: info.org_id,
        domain: info.domain,
        before_ts: info.before_ts,
        preview: info.preview,
        replica: info.replica.map(OrgSyncOverviewDto::from),
        is_current_user_admin: info.is_current_user_admin,
    })
}

pub(crate) fn purge_execute_inner(
    kernel: &mut Kernel,
    org_id: &str,
    before_ts: i64,
    confirm_exported: bool,
) -> Result<PurgeResult, String> {
    kernel
        .execute_purge(org_id, before_ts, confirm_exported)
        .map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn data_usage(state: tauri::State<'_, KernelState>) -> Result<DataUsageReport, String> {
    usage_inner(&mut *lock_kernel(&state)?)
}

#[tauri::command]
pub fn data_cleanup_now(
    state: tauri::State<'_, KernelState>,
) -> Result<AutoCleanupResult, String> {
    cleanup_now_inner(&mut *lock_kernel(&state)?)
}

/// TODO(适配层)：接 tauri-plugin-dialog 的保存对话框；当前要求前端显式传路径。
#[tauri::command]
pub fn data_export(
    state: tauri::State<'_, KernelState>,
    file_path: String,
) -> Result<ExportWriteResult, String> {
    export_inner(&*lock_kernel(&state)?, &file_path)
}

#[tauri::command]
pub fn data_purge_preview(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    before_ts: i64,
) -> Result<PurgePreviewInfoDto, String> {
    purge_preview_inner(&*lock_kernel(&state)?, &org_id, before_ts)
}

#[tauri::command]
pub fn data_purge_execute(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    before_ts: i64,
    confirm_exported: bool,
) -> Result<PurgeResult, String> {
    purge_execute_inner(&mut *lock_kernel(&state)?, &org_id, before_ts, confirm_exported)
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

    #[test]
    fn usage_and_cleanup_run() {
        let (_dir, mut kernel) = unlocked_kernel();
        let report = usage_inner(&mut kernel).unwrap();
        assert!(report.total_bytes > 0 || report.total_keys == 0);
        let cleanup = cleanup_now_inner(&mut kernel).unwrap();
        assert!(cleanup.ran_at > 0);
    }

    #[test]
    fn export_writes_dump_file() {
        let (_dir, kernel) = unlocked_kernel();
        let out = tempfile::NamedTempFile::new().unwrap();
        let result = export_inner(&kernel, out.path().to_str().unwrap()).unwrap();
        assert!(result.bytes > 0);
        assert!(std::fs::metadata(out.path()).unwrap().len() > 0);
    }

    #[test]
    fn purge_preview_unknown_org_errors() {
        let (_dir, kernel) = unlocked_kernel();
        let e = purge_preview_inner(&kernel, "org_0000000000000000", 0).unwrap_err();
        assert!(e.contains("Organization not found"));
    }

    #[test]
    fn purge_execute_requires_admin_and_export_confirmation() {
        let (_dir, mut kernel) = unlocked_kernel();
        let org = kernel
            .create_org(spark_core::org::service::CreateOrganizationInput {
                name: "org".into(),
                description: None,
                base_plugin_domain: "plugin:base".into(),
            })
            .unwrap();
        // 未确认导出 → 拒绝执行
        let e = purge_execute_inner(&mut kernel, &org.record.org_id, i64::MAX, false).unwrap_err();
        assert!(!e.is_empty());
        // 管理员 + 已确认导出，但 P2P 未启动 → 拒绝（副本充足性无法验证）
        let preview = purge_preview_inner(&kernel, &org.record.org_id, i64::MAX).unwrap();
        assert!(preview.is_current_user_admin);
        assert!(preview.replica.is_none());
        assert_eq!(
            purge_execute_inner(&mut kernel, &org.record.org_id, i64::MAX, true).unwrap_err(),
            "P2P network is not started; cannot verify replica sufficiency, purge refused"
        );
    }
}
