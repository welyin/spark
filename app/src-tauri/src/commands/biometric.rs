//! 生物识别解锁命令（M4，方案 m4-m5-mobile-plan §3.4；契约 §1.2）。
//!
//! 不进内核——四个命令全是壳层/JNI 直通；唯一的内核耦合是
//! `biometric_store_password` 先经 `Kernel::verify_session_password` 验密
//! （不符返回 `invalid-password`，口令明文不落任何存储、不进芯片前未经内核背书）。
//!
//! 桌面端四命令恒返回 `unsupported`（契约 §1.2 PC 口径）；Android 端经
//! `biometric_android` JNI 桥调 `BiometricKeystoreHelper`，错误码七值
//! （unsupported/not-enrolled/no-secret/user-cancelled/auth-failed/lockout/
//! key-invalidated）原样透传给前端映射文案。

use serde::Serialize;
use spark_core::kernel::{Kernel, KernelError};

use super::err;
use crate::KernelState;

#[cfg(target_os = "android")]
use crate::biometric_android as platform;

/// `biometric_status` 返回。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BiometricStatusDto {
    pub available: bool,
    pub enrolled: bool,
    pub has_secret: bool,
}

/// `biometric_unlock` 返回：per-identity 标签（前端比对当前活动身份）+ 口令明文。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BiometricUnlockDto {
    pub root_id: String,
    pub password: String,
}

/// 开启生物识别的验密门控（§3.5）：解锁态 + 会话口令比对，零副作用；
/// 通过则返回当前身份 rootId（芯片载荷的 per-identity 标签）。
/// 桌面构建仅壳层测试直调（命令桩恒 unsupported，不触达本函数）。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn verify_store_password(kernel: &Kernel, password: &str) -> Result<String, String> {
    if !kernel.verify_session_password(password).map_err(err)? {
        return Err("invalid-password".to_string());
    }
    kernel
        .current_root_id()
        .map_err(err)?
        .ok_or_else(|| err(KernelError::NotInitialized))
}

// ------------------------------------------------------------------
// Android 实现
// ------------------------------------------------------------------

#[cfg(target_os = "android")]
#[tauri::command]
pub fn biometric_status() -> Result<BiometricStatusDto, String> {
    let status = platform::status()?;
    Ok(BiometricStatusDto {
        available: status.available,
        enrolled: status.enrolled,
        has_secret: status.has_secret,
    })
}

#[cfg(target_os = "android")]
#[tauri::command]
pub async fn biometric_store_password(
    state: tauri::State<'_, KernelState>,
    password: String,
) -> Result<super::dto::SuccessResult, String> {
    // 先验密（内核零副作用查询），再交芯片——口令明文不进未经内核背书的存储
    let root_id = {
        let guard = super::lock_kernel(&state)?;
        verify_store_password(&guard, &password)?
    };
    // BiometricPrompt 认证等待最长 2min（Kotlin 侧超时），挪阻塞线程池
    tauri::async_runtime::spawn_blocking(move || platform::store_password(&root_id, &password))
        .await
        .map_err(|e| format!("biometric task join failed: {e}"))??;
    Ok(super::dto::SuccessResult::ok())
}

#[cfg(target_os = "android")]
#[tauri::command]
pub async fn biometric_unlock() -> Result<BiometricUnlockDto, String> {
    let out = tauri::async_runtime::spawn_blocking(platform::unlock)
        .await
        .map_err(|e| format!("biometric task join failed: {e}"))??;
    Ok(BiometricUnlockDto {
        root_id: out.root_id,
        password: out.password,
    })
}

#[cfg(target_os = "android")]
#[tauri::command]
pub fn biometric_delete() -> Result<super::dto::SuccessResult, String> {
    platform::delete()?;
    Ok(super::dto::SuccessResult::ok())
}

// ------------------------------------------------------------------
// 桌面桩：恒 unsupported（契约 §1.2 PC 不实现）
// ------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn biometric_status() -> Result<BiometricStatusDto, String> {
    Err("unsupported".to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub async fn biometric_store_password(
    state: tauri::State<'_, KernelState>,
    password: String,
) -> Result<super::dto::SuccessResult, String> {
    let _ = (state, password);
    Err("unsupported".to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub async fn biometric_unlock() -> Result<BiometricUnlockDto, String> {
    Err("unsupported".to_string())
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn biometric_delete() -> Result<super::dto::SuccessResult, String> {
    Err("unsupported".to_string())
}

// ------------------------------------------------------------------
// 测试（M4 测试点 1：biometric_store_password 验密门控）
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

    /// 验密门控：解锁态 + 会话口令匹配 → 返回当前身份 rootId（芯片 per-identity 标签）。
    #[test]
    fn verify_store_password_accepts_session_password_and_returns_root_id() {
        let (_dir, mut kernel) = temp_kernel();
        let init = kernel
            .init_identity(PW, "alice", None)
            .expect("init identity");
        let root_id = verify_store_password(&kernel, PW).expect("session pw accepted");
        assert_eq!(root_id, init.root_id, "返回当前身份 rootId");
    }

    /// 会话口令不符 → invalid-password（口令明文不进芯片）。
    #[test]
    fn verify_store_password_rejects_wrong_password() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).expect("init identity");
        let err = verify_store_password(&kernel, "wrong-password").unwrap_err();
        assert_eq!(err, "invalid-password", "口令不符 → invalid-password");
    }

    /// 锁定态 → 走 err(Locked)（壳层直调 verify_session_password 的 Locked 透传）。
    #[test]
    fn verify_store_password_rejects_locked_kernel() {
        let (_dir, mut kernel) = temp_kernel();
        kernel.init_identity(PW, "alice", None).expect("init identity");
        kernel.lock();
        let err = verify_store_password(&kernel, PW).unwrap_err();
        assert!(err.contains("locked"), "锁定态应报 locked：{err}");
    }
}
