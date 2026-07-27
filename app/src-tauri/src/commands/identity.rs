//! 身份全组命令（优先级 P0）。
//!
//! 与 TS `ipc/identity.ts` 的通道一一对应；返回形状对齐 preload.ts 的
//! `rootIdentity.*` 类型，前端适配层零加工透传。

use serde::Serialize;
use spark_core::kernel::{
    DerivedDomainIdentityInfo, IdentityStatus, IdentitySummary, Kernel, MnemonicCheckInfo,
    ProfileInfo, PublicIdentity, RootSignatureInfo,
};

use super::{err, lock_kernel};
use crate::KernelState;

/// `root-init` 返回（TS `{ rootId, mnemonic }`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InitResultDto {
    pub root_id: String,
    pub mnemonic: String,
}

/// `{ rootId }` 形状（unlock/recover 的返回）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RootIdResultDto {
    pub root_id: String,
}

/// `root-reveal-mnemonic` 返回。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MnemonicResultDto {
    pub mnemonic: String,
}

/// `root-backup-payload` 返回。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PayloadResultDto {
    pub payload: String,
}

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn status_inner(kernel: &Kernel) -> Result<IdentityStatus, String> {
    kernel.status().map_err(err)
}

pub(crate) fn list_identities_inner(kernel: &Kernel) -> Result<Vec<IdentitySummary>, String> {
    kernel.list_identities().map_err(err)
}

pub(crate) fn init_inner(
    kernel: &mut Kernel,
    password: &str,
    nickname: &str,
    avatar: Option<&str>,
) -> Result<InitResultDto, String> {
    let result = kernel
        .init_identity(password, nickname, avatar)
        .map_err(err)?;
    Ok(InitResultDto {
        root_id: result.root_id,
        mnemonic: result.mnemonic,
    })
}

pub(crate) fn unlock_inner(
    kernel: &mut Kernel,
    password: &str,
    root_id: Option<&str>,
) -> Result<RootIdResultDto, String> {
    let root_id = kernel.unlock(password, root_id).map_err(err)?;
    Ok(RootIdResultDto { root_id })
}

pub(crate) fn lock_inner(kernel: &mut Kernel) -> super::dto::SuccessResult {
    kernel.lock();
    super::dto::SuccessResult::ok()
}

/// 切换登录目标（仅改活动指针；TS `setActive`）。
pub(crate) fn set_active_inner(
    kernel: &Kernel,
    root_id: &str,
) -> Result<super::dto::SuccessResult, String> {
    kernel.set_active_identity(root_id).map_err(err)?;
    Ok(super::dto::SuccessResult::ok())
}

pub(crate) fn recover_mnemonic_inner(
    kernel: &mut Kernel,
    mnemonic: &str,
    new_password: &str,
    nickname: &str,
    avatar: Option<&str>,
) -> Result<RootIdResultDto, String> {
    let root_id = kernel
        .recover_mnemonic(mnemonic, new_password, nickname, avatar)
        .map_err(err)?;
    Ok(RootIdResultDto { root_id })
}

pub(crate) fn recover_backup_inner(
    kernel: &mut Kernel,
    payload: &str,
    password: &str,
) -> Result<RootIdResultDto, String> {
    let root_id = kernel.recover_backup(payload, password).map_err(err)?;
    Ok(RootIdResultDto { root_id })
}

pub(crate) fn backup_payload_inner(kernel: &Kernel) -> Result<PayloadResultDto, String> {
    let payload = kernel.backup_payload().map_err(err)?;
    Ok(PayloadResultDto { payload })
}

pub(crate) fn reveal_mnemonic_inner(
    kernel: &Kernel,
    password: &str,
) -> Result<MnemonicResultDto, String> {
    let mnemonic = kernel.reveal_mnemonic(password).map_err(err)?;
    Ok(MnemonicResultDto { mnemonic })
}

/// `root-update-profile`（TS 为免密码会话版）：内核以 unlock 会话缓存口令重封，
/// 参数形状对齐 preload 的 `profile` 对象字段。
pub(crate) fn update_profile_inner(
    kernel: &mut Kernel,
    nickname: Option<&str>,
    avatar: Option<Option<&str>>,
) -> Result<ProfileInfo, String> {
    kernel.update_profile_session(nickname, avatar).map_err(err)
}

pub(crate) fn current_identity_inner(
    kernel: &Kernel,
) -> Result<Option<PublicIdentity>, String> {
    kernel.current_identity().map_err(err)
}

pub(crate) fn sign_inner(kernel: &Kernel, payload: &str) -> Result<RootSignatureInfo, String> {
    kernel.sign(payload).map_err(err)
}

pub(crate) fn derive_domain_inner(
    kernel: &Kernel,
    domain: &str,
) -> Result<DerivedDomainIdentityInfo, String> {
    kernel.derive_domain_identity(domain).map_err(err)
}

pub(crate) fn mnemonic_check_inner(input: &str) -> MnemonicCheckInfo {
    Kernel::check_mnemonic(input)
}

// ------------------------------------------------------------------
// Tauri 命令（同步 command → Tauri 自动放到线程池，满足内核线程模型）
// ------------------------------------------------------------------

#[tauri::command]
pub fn root_status(state: tauri::State<'_, KernelState>) -> Result<IdentityStatus, String> {
    status_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn root_list_identities(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<IdentitySummary>, String> {
    list_identities_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn root_init(
    state: tauri::State<'_, KernelState>,
    password: String,
    nickname: String,
    avatar: Option<String>,
) -> Result<InitResultDto, String> {
    init_inner(&mut *lock_kernel(&state)?, &password, &nickname, avatar.as_deref())
}

#[tauri::command]
pub fn root_unlock(
    state: tauri::State<'_, KernelState>,
    password: String,
    root_id: Option<String>,
) -> Result<RootIdResultDto, String> {
    unlock_inner(&mut *lock_kernel(&state)?, &password, root_id.as_deref())
}

#[tauri::command]
pub fn root_lock(state: tauri::State<'_, KernelState>) -> super::dto::SuccessResult {
    // lock 不返回错误；poison 时静默失败等价于 TS 的空操作语义不可达，直接 panic 由 Tauri 兜底。
    lock_inner(&mut lock_kernel(&state).expect("kernel state lock poisoned"))
}

#[tauri::command]
pub fn root_set_active(
    state: tauri::State<'_, KernelState>,
    root_id: String,
) -> Result<super::dto::SuccessResult, String> {
    set_active_inner(&*lock_kernel(&state)?, &root_id)
}

#[tauri::command]
pub fn root_recover_mnemonic(
    state: tauri::State<'_, KernelState>,
    mnemonic: String,
    new_password: String,
    nickname: String,
    avatar: Option<String>,
) -> Result<RootIdResultDto, String> {
    recover_mnemonic_inner(
        &mut *lock_kernel(&state)?,
        &mnemonic,
        &new_password,
        &nickname,
        avatar.as_deref(),
    )
}

#[tauri::command]
pub fn root_recover_backup(
    state: tauri::State<'_, KernelState>,
    payload: String,
    password: String,
) -> Result<RootIdResultDto, String> {
    recover_backup_inner(&mut *lock_kernel(&state)?, &payload, &password)
}

#[tauri::command]
pub fn root_backup_payload(
    state: tauri::State<'_, KernelState>,
) -> Result<PayloadResultDto, String> {
    backup_payload_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn root_reveal_mnemonic(
    state: tauri::State<'_, KernelState>,
    password: String,
) -> Result<MnemonicResultDto, String> {
    reveal_mnemonic_inner(&*lock_kernel(&state)?, &password)
}

#[tauri::command]
pub fn root_update_profile(
    state: tauri::State<'_, KernelState>,
    nickname: Option<String>,
    avatar: Option<Option<String>>,
) -> Result<ProfileInfo, String> {
    update_profile_inner(
        &mut *lock_kernel(&state)?,
        nickname.as_deref(),
        avatar.as_ref().map(|inner| inner.as_deref()),
    )
}

#[tauri::command]
pub fn root_current_identity(
    state: tauri::State<'_, KernelState>,
) -> Result<Option<PublicIdentity>, String> {
    current_identity_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn root_sign(
    state: tauri::State<'_, KernelState>,
    payload: String,
) -> Result<RootSignatureInfo, String> {
    sign_inner(&*lock_kernel(&state)?, &payload)
}

#[tauri::command]
pub fn root_derive_domain(
    state: tauri::State<'_, KernelState>,
    domain: String,
) -> Result<DerivedDomainIdentityInfo, String> {
    derive_domain_inner(&*lock_kernel(&state)?, &domain)
}

/// 录入助记词逐词校验（不需要身份态）。
#[tauri::command]
pub fn root_mnemonic_check(input: String) -> MnemonicCheckInfo {
    mnemonic_check_inner(&input)
}

#[cfg(test)]
mod tests;
