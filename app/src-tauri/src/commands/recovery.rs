//! M5 延迟恢复命令层（S6）。
//!
//! 对应内核 `Kernel::{recovery_initiate, recovery_confirm, recovery_veto, recovery_status}`。
//! 所有命令在锁定态/解锁态的权限检查由内核对等完成；壳层只负责 JSON DTO 转换与错误透传。

use serde::{Deserialize, Serialize};
use spark_core::kernel::{
    RecoveryConfirmArgs as KernelRecoveryConfirmArgs,
    RecoveryInitiateArgs as KernelRecoveryInitiateArgs,
    RecoveryStatusResult as KernelRecoveryStatusResult,
    RecoveryVetoArgs as KernelRecoveryVetoArgs,
};

use super::{dto::SuccessResult, err, KernelState, lock_kernel};

// ------------------------------------------------------------------
// DTO
// ------------------------------------------------------------------

/// 恢复操作类型（与内核 `RecoveryOp` snake_case 序列化一致）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOpDto {
    ResetPassword,
    PairNewDevice,
}

impl RecoveryOpDto {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ResetPassword => "reset_password",
            Self::PairNewDevice => "pair_new_device",
        }
    }
}

/// 恢复请求状态（与内核 `RecoveryState` snake_case 序列化一致）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStateDto {
    Initiated,
    Vetoed,
    Committed,
}

/// 待确认恢复请求 DTO。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRecoveryDto {
    pub request_id: String,
    pub op: String,
    pub deadline: i64,
    pub initiated_at: i64,
    pub vetoed: bool,
    pub state: String,
}

/// 恢复状态查询结果 DTO。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryStatusDto {
    pub pending: Option<PendingRecoveryDto>,
    pub ready_to_confirm: bool,
}

/// `root_recovery_initiate` 请求参数。
#[derive(Clone, Debug, Deserialize)]
pub struct RecoveryInitiateRequest {
    pub op: RecoveryOpDto,
    /// 延迟小时数；省略时使用内核默认值（24h）。
    #[serde(rename = "delayHours")]
    pub delay_hours: Option<f64>,
}

/// `root_recovery_confirm` 请求参数。
#[derive(Clone, Debug, Deserialize)]
pub struct RecoveryConfirmRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// 新密码；`reset_password` 时必填，`pair_new_device` 可选（M5b 扩展）。
    #[serde(rename = "newPassword")]
    pub new_password: Option<String>,
}

/// `root_recovery_veto` 请求参数。
#[derive(Clone, Debug, Deserialize)]
pub struct RecoveryVetoRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
}

/// `root_recovery_initiate` / `root_recovery_confirm` 返回的待确认记录 DTO。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPendingResultDto {
    pub request_id: String,
    pub op: String,
    pub deadline: i64,
    pub initiated_at: i64,
    pub vetoed: bool,
    pub state: String,
}

// ------------------------------------------------------------------
// Inner helpers（方便测试直接注入 Kernel，不依赖 Tauri State）
// ------------------------------------------------------------------

fn serialize_state(state: &spark_core::recovery::RecoveryState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn status_from_kernel(result: KernelRecoveryStatusResult) -> RecoveryStatusDto {
    RecoveryStatusDto {
        pending: result.pending.map(|p| PendingRecoveryDto {
            request_id: p.request_id,
            op: p.op.as_str().to_string(),
            deadline: p.deadline,
            initiated_at: p.initiated_at,
            vetoed: p.vetoed,
            state: serialize_state(&p.state),
        }),
        ready_to_confirm: result.ready_to_confirm,
    }
}

fn pending_from_kernel(p: spark_core::recovery::PendingRecovery) -> RecoveryPendingResultDto {
    RecoveryPendingResultDto {
        request_id: p.request_id,
        op: p.op.as_str().to_string(),
        deadline: p.deadline,
        initiated_at: p.initiated_at,
        vetoed: p.vetoed,
        state: serialize_state(&p.state),
    }
}

pub(crate) fn status_inner(kernel: &spark_core::kernel::Kernel) -> Result<RecoveryStatusDto, String> {
    let result = kernel.recovery_status().map_err(err)?;
    Ok(status_from_kernel(result))
}

pub(crate) fn initiate_inner(
    kernel: &mut spark_core::kernel::Kernel,
    req: RecoveryInitiateRequest,
) -> Result<RecoveryPendingResultDto, String> {
    let args = KernelRecoveryInitiateArgs {
        op: req.op.as_str().to_string(),
        delay_hours: req.delay_hours,
    };
    let pending = kernel.recovery_initiate(args).map_err(err)?;
    Ok(pending_from_kernel(pending))
}

pub(crate) fn confirm_inner(
    kernel: &mut spark_core::kernel::Kernel,
    req: RecoveryConfirmRequest,
) -> Result<RecoveryPendingResultDto, String> {
    let args = KernelRecoveryConfirmArgs {
        request_id: req.request_id,
        new_password: req.new_password,
    };
    let pending = kernel.recovery_confirm(args).map_err(err)?;
    Ok(pending_from_kernel(pending))
}

pub(crate) fn veto_inner(
    kernel: &mut spark_core::kernel::Kernel,
    req: RecoveryVetoRequest,
) -> Result<SuccessResult, String> {
    let args = KernelRecoveryVetoArgs {
        request_id: req.request_id,
    };
    kernel.recovery_veto(args).map_err(err)?;
    Ok(SuccessResult::ok())
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn root_recovery_status(
    state: tauri::State<'_, KernelState>,
) -> Result<RecoveryStatusDto, String> {
    status_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn root_recovery_initiate(
    state: tauri::State<'_, KernelState>,
    req: RecoveryInitiateRequest,
) -> Result<RecoveryPendingResultDto, String> {
    initiate_inner(&mut *lock_kernel(&state)?, req)
}

#[tauri::command]
pub fn root_recovery_confirm(
    state: tauri::State<'_, KernelState>,
    req: RecoveryConfirmRequest,
) -> Result<RecoveryPendingResultDto, String> {
    confirm_inner(&mut *lock_kernel(&state)?, req)
}

#[tauri::command]
pub fn root_recovery_veto(
    state: tauri::State<'_, KernelState>,
    req: RecoveryVetoRequest,
) -> Result<SuccessResult, String> {
    veto_inner(&mut *lock_kernel(&state)?, req)
}

// ------------------------------------------------------------------
// 测试（M5 测试点 5：壳层四命令参数校验 / 错误透传 / 成功形状）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::{Kernel, KernelConfig};

    const PW: &str = "correct-horse-battery";

    fn temp_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        (dir, kernel)
    }

    #[test]
    fn status_empty_shape() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let status = status_inner(&kernel).unwrap();
        assert!(status.pending.is_none());
        assert!(!status.ready_to_confirm);
    }

    #[test]
    fn initiate_success_shape_and_op_field() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let pending = initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::ResetPassword,
                delay_hours: Some(1.0),
            },
        )
        .unwrap();
        assert!(pending.request_id.starts_with("rc"), "requestId 以 rc 开头");
        assert_eq!(pending.op, "reset_password");
        assert_eq!(pending.state, "initiated");
        assert!(!pending.vetoed);
        assert!(pending.deadline > pending.initiated_at, "deadline 在发起时间之后");

        // 状态查询回读 pending。
        let status = status_inner(&kernel).unwrap();
        let p = status.pending.expect("有 pending");
        assert_eq!(p.request_id, pending.request_id);
        assert_eq!(p.op, "reset_password");
        assert_eq!(p.state, "initiated");
    }

    #[test]
    fn initiate_duplicate_active_reports_recovery_pending() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::ResetPassword,
                delay_hours: Some(24.0),
            },
        )
        .unwrap();
        let err = initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::ResetPassword,
                delay_hours: Some(24.0),
            },
        )
        .unwrap_err();
        assert_eq!(err, "RecoveryPending", "活跃 pending 再发起 → RecoveryPending");
    }

    #[test]
    fn initiate_unsupported_op_and_invalid_delay() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let err = initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::PairNewDevice,
                delay_hours: None,
            },
        )
        .unwrap_err();
        assert_eq!(err, "UnsupportedOp", "PairNewDevice → UnsupportedOp");
        let err2 = initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::ResetPassword,
                delay_hours: Some(0.0),
            },
        )
        .unwrap_err();
        assert_eq!(err2, "Invalid input", "过短 delay → Invalid input");
    }

    #[test]
    fn confirm_wrong_request_id_reports_not_found() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let err = confirm_inner(
            &mut kernel,
            RecoveryConfirmRequest {
                request_id: "rc-no-such".to_string(),
                new_password: Some("newpassword456".to_string()),
            },
        )
        .unwrap_err();
        assert_eq!(err, "RecoveryNotFound", "未知 requestId → RecoveryNotFound");
    }

    #[test]
    fn veto_unknown_request_reports_not_found_and_success_shape() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let err = veto_inner(
            &mut kernel,
            RecoveryVetoRequest {
                request_id: "rc-no-such".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(err, "RecoveryNotFound", "未知 requestId → RecoveryNotFound");

        // 先发起再否决 → 成功形状 { success: true }。
        let pending = initiate_inner(
            &mut kernel,
            RecoveryInitiateRequest {
                op: RecoveryOpDto::ResetPassword,
                delay_hours: Some(24.0),
            },
        )
        .unwrap();
        let res = veto_inner(
            &mut kernel,
            RecoveryVetoRequest {
                request_id: pending.request_id.clone(),
            },
        )
        .unwrap();
        assert!(res.success, "veto 成功形状 success=true");
        // 状态回读：pending 已置 vetoed。
        let status = status_inner(&kernel).unwrap();
        let p = status.pending.expect("pending 仍在");
        assert_eq!(p.request_id, pending.request_id);
        assert!(p.vetoed, "veto 后 pending.vetoed=true");
        assert_eq!(p.state, "vetoed");
    }
}
