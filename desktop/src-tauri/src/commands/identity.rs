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

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "correct-horse-battery";

    fn temp_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(spark_core::kernel::KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        (dir, kernel)
    }

    #[test]
    fn status_on_fresh_dir_is_uninitialized() {
        let (_dir, kernel) = temp_kernel();
        let status = status_inner(&kernel).unwrap();
        assert!(!status.initialized);
        assert!(!status.unlocked);
        assert_eq!(status.root_id, None);
        assert!(list_identities_inner(&kernel).unwrap().is_empty());
        // 未初始化：依赖当前身份的命令报 NotInitialized 文案
        assert_eq!(
            backup_payload_inner(&kernel).unwrap_err(),
            "Root identity is not initialized"
        );
        assert!(current_identity_inner(&kernel).unwrap().is_none());
    }

    #[test]
    fn full_identity_lifecycle() {
        let (_dir, mut kernel) = temp_kernel();

        // init：返回 rootId + 24 词助记词，身份随即解锁
        let init = init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();
        assert!(!init.root_id.is_empty());
        assert_eq!(init.mnemonic.split_whitespace().count(), 24);

        let status = status_inner(&kernel).unwrap();
        assert!(status.initialized && status.unlocked);
        assert_eq!(status.root_id.as_deref(), Some(init.root_id.as_str()));
        assert_eq!(status.nickname.as_deref(), Some("alice"));

        let list = list_identities_inner(&kernel).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].active);
        assert_eq!(list[0].root_id, init.root_id);

        let current = current_identity_inner(&kernel).unwrap().unwrap();
        assert_eq!(current.root_id, init.root_id);
        assert!(!current.public_key_hex.is_empty());

        // reveal_mnemonic：密码门控，错误密码报 Invalid password
        assert_eq!(
            reveal_mnemonic_inner(&kernel, "wrong-password").unwrap_err(),
            "Invalid password"
        );
        let revealed = reveal_mnemonic_inner(&kernel, PASSWORD).unwrap();
        assert_eq!(revealed.mnemonic, init.mnemonic);

        // update_profile（免密码会话版）：改昵称、清头像
        let profile = update_profile_inner(&mut kernel, Some("alice-2"), Some(None)).unwrap();
        assert_eq!(profile.nickname.as_deref(), Some("alice-2"));
        assert_eq!(profile.avatar, None);

        // backup_payload：返回当前身份密文 JSON
        let backup = backup_payload_inner(&kernel).unwrap();
        assert!(backup.payload.contains(&init.root_id));

        // lock → status 反映锁定；解锁后恢复
        lock_inner(&mut kernel);
        let status = status_inner(&kernel).unwrap();
        assert!(status.initialized && !status.unlocked);
        assert!(current_identity_inner(&kernel).unwrap().is_none());

        let unlocked = unlock_inner(&mut kernel, PASSWORD, None).unwrap();
        assert_eq!(unlocked.root_id, init.root_id);
        assert_eq!(
            unlock_inner(&mut kernel, "wrong-password", None).unwrap_err(),
            "Invalid password"
        );
    }

    #[test]
    fn recover_mnemonic_on_second_device_and_set_active() {
        let (_dir1, mut kernel_a) = temp_kernel();
        let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();

        // 另一"设备"（独立数据目录）：助记词恢复出同一 rootId
        let (_dir2, mut kernel_b) = temp_kernel();
        let recovered =
            recover_mnemonic_inner(&mut kernel_b, &init.mnemonic, "new-password-1", "alice-b", None)
                .unwrap();
        assert_eq!(recovered.root_id, init.root_id);

        // 重复恢复同一身份报"已在本设备上"
        assert!(recover_mnemonic_inner(
            &mut kernel_b,
            &init.mnemonic,
            "new-password-1",
            "x",
            None
        )
        .unwrap_err()
        .contains("已在本设备上"));

        // 坏助记词报校验失败文案
        assert!(recover_mnemonic_inner(&mut kernel_b, "一二三四", "new-password-1", "x", None)
            .unwrap_err()
            .contains("助记词校验失败"));

        // 同目录第二个身份 + set_active 切换指针
        let second = init_inner(&mut kernel_a, PASSWORD, "bob", None).unwrap();
        assert_ne!(second.root_id, init.root_id);
        lock_inner(&mut kernel_a);
        set_active_inner(&kernel_a, &init.root_id).unwrap();
        let list = list_identities_inner(&kernel_a).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|i| i.root_id == init.root_id && i.active));
        set_active_inner(&kernel_a, "no-such-root-id").unwrap_err();
    }

    #[test]
    fn recover_backup_roundtrip() {
        let (_dir1, mut kernel_a) = temp_kernel();
        let init = init_inner(&mut kernel_a, PASSWORD, "alice", None).unwrap();
        let backup = backup_payload_inner(&kernel_a).unwrap();

        let (_dir2, mut kernel_b) = temp_kernel();
        // 密码错误 → 专用文案
        assert_eq!(
            recover_backup_inner(&mut kernel_b, &backup.payload, "wrong-password").unwrap_err(),
            "密码不正确"
        );
        // 载荷损坏 → 专用文案
        assert_eq!(
            recover_backup_inner(&mut kernel_b, "{not-json", PASSWORD).unwrap_err(),
            "备份数据无效或已损坏"
        );
        let recovered = recover_backup_inner(&mut kernel_b, &backup.payload, PASSWORD).unwrap();
        assert_eq!(recovered.root_id, init.root_id);
        assert!(status_inner(&kernel_b).unwrap().unlocked);
    }

    #[test]
    fn password_policy_enforced() {
        let (_dir, mut kernel) = temp_kernel();
        assert_eq!(
            init_inner(&mut kernel, "short", "alice", None).unwrap_err(),
            "Password must be at least 8 characters"
        );
    }

    #[test]
    fn sign_derive_domain_and_mnemonic_check() {
        let (_dir, mut kernel) = temp_kernel();

        // 锁定状态
        assert_eq!(sign_inner(&kernel, "p").unwrap_err(), "Root identity is locked");
        assert_eq!(
            derive_domain_inner(&kernel, "plugin:chat").unwrap_err(),
            "Root identity is locked"
        );

        let init = init_inner(&mut kernel, PASSWORD, "alice", None).unwrap();

        // sign：rootId/payloadHash 形状
        let sig = sign_inner(&kernel, "hello").unwrap();
        assert_eq!(sig.root_id, init.root_id);
        assert_eq!(sig.payload_hash.len(), 64);
        assert!(!sig.signature.is_empty());

        // derive：确定性 + 域回显 + 空域报错
        let d1 = derive_domain_inner(&kernel, "plugin:chat").unwrap();
        let d2 = derive_domain_inner(&kernel, "plugin:chat").unwrap();
        assert_eq!(d1, d2);
        assert_eq!(d1.domain, "plugin:chat");
        assert!(d1.derivation_path.starts_with("m/44'/607'/0'/0'/0'/"));
        assert_eq!(
            derive_domain_inner(&kernel, "  ").unwrap_err(),
            "Domain is required"
        );

        // mnemonic-check：词数组 + 词表外词下标
        let check = mnemonic_check_inner("legal winner notaword");
        assert_eq!(check.words.len(), 3);
        assert_eq!(check.invalid_indexes, vec![2]);
        let continuous = mnemonic_check_inner("与祝产");
        assert_eq!(continuous.words, vec!["与", "祝", "产"]);
        assert!(continuous.invalid_indexes.is_empty());
    }
}
