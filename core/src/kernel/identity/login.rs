//! 登录链路：注册（initialize）/解锁/锁定/切换活动身份/助记词与备份码恢复，
//! 以及登录成功后的 p2p 兜底启动（"登录即在线"）。

use super::{
    InitIdentityResult, check_password, map_identity_decrypt_error, normalize_mnemonic_input,
};
use crate::identity::{self, IdentityFile};
use crate::kernel::Kernel;
use crate::kernel::error::{KernelError, Result};

impl Kernel {
    /// 登录链路收尾：配置含 p2p 时尽力启动（"登录即在线"，对齐 TS
    /// `ensureCoreServicesStarted` 的兜底语义）。失败仅记录到
    /// `p2p_start_error`，不使登录失败；已在运行则不动。
    fn ensure_p2p_after_login(&mut self) {
        if self.config.p2p.is_none() || self.p2p.is_some() {
            return;
        }
        if let Err(e) = self.start_p2p() {
            eprintln!("[kernel] login auto start p2p failed: {e}");
            self.p2p_start_error = Some(e.to_string());
        }
    }

    /// `initialize`（注册）：生成 24 词中文助记词 → root 派生 → v2 加密落盘，
    /// 设为活动身份并解锁；存储目录对齐到该身份。
    ///
    /// 返回明文助记词（仅此一次展示）。
    pub fn init_identity(
        &mut self,
        password: &str,
        nickname: &str,
        avatar: Option<&str>,
    ) -> Result<InitIdentityResult> {
        check_password(password)?;
        let mnemonic = identity::generate_mnemonic()?;
        let (file, identity) = identity::recover_identity(&mnemonic, password, nickname, avatar)?;
        let seed = identity::parse_mnemonic(&mnemonic)?.seed;
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password);
        self.ensure_p2p_after_login();
        Ok(InitIdentityResult { root_id, mnemonic })
    }

    /// `unlock`：密码解锁指定身份（缺省为活动身份），设为当前并触发存储对齐。
    /// v1 遗留文件解锁成功后按 spec §5 迁移为 v2。
    pub fn unlock(&mut self, password: &str, root_id: Option<&str>) -> Result<String> {
        let target = match root_id {
            Some(rid) => rid.to_string(),
            None => self
                .read_active_root_id()?
                .ok_or(KernelError::NotInitialized)?,
        };
        let Some(mut file) = self.read_identity_file(&target)? else {
            return Err(KernelError::Internal("该账号不在本设备上".to_string()));
        };
        if file.version == identity::file::FILE_VERSION_V1 {
            file =
                identity::migrate_v1_to_v2(&file, password).map_err(map_identity_decrypt_error)?;
            self.write_identity_file(&file)?;
        }
        let (payload, identity) =
            identity::unlock_identity(&file, password).map_err(map_identity_decrypt_error)?;
        if identity.id() != file.root_id {
            return Err(KernelError::Internal(
                "Root identity verification failed".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&payload.mnemonic)?.seed;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password);
        self.ensure_p2p_after_login();
        Ok(root_id)
    }

    /// `lock`：锁定当前身份（活动指针不变）；会话私钥同步清除，P2P 一并停止
    ///（登出即离线，也避免切换用户后仍挂着旧身份的网络节点）。
    pub fn lock(&mut self) {
        let _ = self.stop_p2p();
        self.unlocked = None;
        *self.signing_key_shared.lock().unwrap() = None;
        *self.nickname_shared.lock().unwrap() = String::new();
        *self.avatar_shared.lock().unwrap() = String::new();
        if let Ok(active) = self.read_active_root_id() {
            *self.current_root_id_shared.lock().unwrap() = active;
        }
    }

    /// `setActive`：切换登录目标用户——仅改活动指针，不解锁、不迁移存储；
    /// 下次 `unlock`（缺省 rootId）以新活动身份为目标（对齐 TS root-id.ts
    /// `setActive` 的"仅改指针，解锁时生效"语义）。
    pub fn set_active_identity(&self, root_id: &str) -> Result<()> {
        if self.read_identity_file(root_id)?.is_none() {
            return Err(KernelError::Internal("该账号不在本设备上".to_string()));
        }
        self.write_active_root_id(root_id)
    }

    /// `recoverFromMnemonic`：助记词恢复（最高权限，无需旧密码），
    /// 以新密码重新加密存储并解锁。中文连续书写/空格分隔、英文词表均可。
    pub fn recover_mnemonic(
        &mut self,
        mnemonic_input: &str,
        new_password: &str,
        nickname: &str,
        avatar: Option<&str>,
    ) -> Result<String> {
        check_password(new_password)?;
        let normalized = normalize_mnemonic_input(mnemonic_input);
        let (file, identity) =
            identity::recover_identity(&normalized, new_password, nickname, avatar).map_err(
                |e| match e {
                    identity::IdentityError::InvalidMnemonic(_) => KernelError::Internal(
                        "助记词校验失败：请检查是否有错别字、漏字或顺序错误".to_string(),
                    ),
                    other => KernelError::Identity(other),
                },
            )?;
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Internal(
                "该账号已在本设备上，请直接登录".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&normalized)?.seed;
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, new_password);
        self.ensure_p2p_after_login();
        Ok(root_id)
    }

    /// `getEncryptedBackupPayload`：导出加密备份载荷（QR 备份码内容），
    /// 即当前身份密文记录的紧凑 JSON；恢复时必须配合原登录密码。
    pub fn backup_payload(&self) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        Ok(file.to_json()?)
    }

    /// `recoverFromBackup`：备份码恢复。载荷即身份密文记录，解密口令为原登录密码；
    /// 结构无效与密码错误分别报错；写入前 sanitize 外部资料字段。
    pub fn recover_backup(&mut self, payload_json: &str, password: &str) -> Result<String> {
        let file = IdentityFile::from_json(payload_json)
            .map_err(|_| KernelError::Internal("备份数据无效或已损坏".to_string()))?;
        let (payload, identity) =
            identity::unlock_identity(&file, password).map_err(|e| match e {
                identity::IdentityError::DecryptionFailed => {
                    KernelError::Internal("密码不正确".to_string())
                }
                identity::IdentityError::InvalidMnemonic(_) | identity::IdentityError::Json(_) => {
                    KernelError::Internal("备份数据无效或已损坏".to_string())
                }
                other => KernelError::Identity(other),
            })?;
        if identity.id() != file.root_id {
            return Err(KernelError::Internal(
                "备份数据校验失败：rootId 不匹配".to_string(),
            ));
        }
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Internal(
                "该账号已在本设备上，请直接登录".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&payload.mnemonic)?.seed;
        // 备份载荷即身份记录本身；资料字段清洗后落库（非法值静默剔除）
        let (nickname, avatar) =
            identity::sanitize_profile(file.nickname.as_deref(), file.avatar.as_deref());
        let file = IdentityFile {
            nickname,
            avatar,
            ..file
        };
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password);
        self.ensure_p2p_after_login();
        Ok(root_id)
    }
}
