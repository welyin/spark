//! M3 乙+校验器（口令统一）命令层（E5）。
//!
//! 对应内核 `Kernel::{verify_password_ticket, unify_password, password_unify_status}`。
//! 三态错误码由内核 `KernelError` Display 透传（ticket-mismatch /
//! invalid-password / ticket-unavailable），前端按码映射三段式文案（F7）。
//! scrypt 验票/重封为 CPU 密集 → `async + run_kernel`（同 root_change_password）；
//! 状态查询锁定态可调（无 scrypt）→ 同步 `lock_kernel`。

use serde::Serialize;
use spark_core::epoch::RotationReason;
use spark_core::kernel::Kernel;

use super::{dto::SuccessResult, err, lock_kernel, run_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// DTO（camelCase，R2 教训：线形与前端 types.ts 消费字段一致）
// ------------------------------------------------------------------

/// `root_verify_password_ticket` 返回（TS `PasswordVerifyTicketResultDto`）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyTicketResultDto {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_by_device: Option<String>,
}

/// `root_password_unify_status` 返回（TS `PasswordUnifyStatusDto`）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifyStatusDto {
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_by_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ------------------------------------------------------------------
// Inner helpers（测试直调，不依赖 Tauri State）
// ------------------------------------------------------------------

pub(crate) fn verify_ticket_inner(
    kernel: &Kernel,
    password: &str,
) -> Result<VerifyTicketResultDto, String> {
    kernel.verify_password_ticket(password).map_err(err)?;
    Ok(VerifyTicketResultDto {
        ok: true,
        rotated_at: None,
        rotated_by_device: None,
    })
}

pub(crate) fn unify_password_inner(
    kernel: &mut Kernel,
    old_password: &str,
    new_password: &str,
) -> Result<SuccessResult, String> {
    kernel
        .unify_password(old_password, new_password, RotationReason::PasswordChange)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn unify_status_inner(kernel: &Kernel) -> Result<UnifyStatusDto, String> {
    let status = kernel.password_unify_status().map_err(err)?;
    Ok(UnifyStatusDto {
        pending: status.stale,
        rotated_at: status.v_changed_at,
        rotated_by_device: status.v_changed_by_device,
        reason: status.v_changed_reason,
    })
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub async fn root_verify_password_ticket(
    state: tauri::State<'_, KernelState>,
    password: String,
) -> Result<VerifyTicketResultDto, String> {
    run_kernel(state, move |kernel| verify_ticket_inner(kernel, &password)).await
}

#[tauri::command]
pub async fn root_unify_password(
    state: tauri::State<'_, KernelState>,
    old_password: String,
    new_password: String,
) -> Result<SuccessResult, String> {
    run_kernel(
        state,
        move |kernel| unify_password_inner(kernel, &old_password, &new_password),
    )
    .await
}

#[tauri::command]
pub fn root_password_unify_status(
    state: tauri::State<'_, KernelState>,
) -> Result<UnifyStatusDto, String> {
    unify_status_inner(&*lock_kernel(&state)?)
}

// ------------------------------------------------------------------
// 测试：三命令各三态 / 锁定态可调状态查询 / DTO camelCase 线形
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
    fn verify_ticket_ok_and_mismatch() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let res = verify_ticket_inner(&kernel, PW).unwrap();
        assert!(res.ok, "正确口令验票 → ok=true");

        let err = verify_ticket_inner(&kernel, "wrong-pass").unwrap_err();
        assert_eq!(err, "Ticket mismatch", "错误口令 → ticket-mismatch");
    }

    #[test]
    fn unify_password_success_and_invalid_old() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        let res = unify_password_inner(&mut kernel, PW, "brand-new-pass").unwrap();
        assert!(res.success, "unify 成功形状 success=true");

        // 旧密码错误 → invalid-password（identity::change_password 解密失败映射）。
        let err = unify_password_inner(&mut kernel, "wrong-old", "another-pass").unwrap_err();
        assert_eq!(err, "Invalid password", "旧密码错 → invalid-password");
    }

    #[test]
    fn unify_status_initial_and_locked_available() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).unwrap();
        // 创世后无外部改密标记：pending=false，但 V 已发布（rotatedAt 存在）。
        let st = unify_status_inner(&kernel).unwrap();
        assert!(!st.pending, "创世后 pending=false");
        assert!(st.rotated_at.is_some(), "已有 V，rotatedAt 存在");
        assert!(st.rotated_by_device.is_some(), "设备名已装配");

        // 锁定态可调：lock 后仍能查（契约 m45 §13.1）。
        kernel.lock();
        let st_locked = unify_status_inner(&kernel).unwrap();
        assert!(!st_locked.pending, "锁定态仍可调");
    }

    #[test]
    fn dto_camel_case_line() {
        let dto = UnifyStatusDto {
            pending: true,
            rotated_at: Some(123),
            rotated_by_device: Some("phone".to_string()),
            reason: Some("password_change".to_string()),
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v["pending"], true);
        assert_eq!(v["rotatedAt"], 123);
        assert_eq!(v["rotatedByDevice"], "phone");
        assert_eq!(v["reason"], "password_change");
        assert!(v.get("rotated_at").is_none(), "不得出现 snake_case 字段");
    }
}
