//! kernel 门面的身份 API：注册/解锁/恢复/资料维护 + 身份文件目录管理。
//!
//! 目录结构与 TS `RootIdentityManager` 对齐（desktop/src/main/identity/root-id.ts）：
//! ```text
//! {data_dir}/identities/{rootId}.json     身份文件（JSON.stringify(payload, null, 2) 风格两空格缩进）
//! {data_dir}/active-identity.json         活动指针 {"activeRootId":"..."}（紧凑 JSON）
//! {data_dir}/root-identity.json           v1 时代单身份遗留文件（init 时幂等迁移）
//! ```
//!
//! 身份文件的内容格式由 `identity` 模块定义（core/spec/identity.md §5 + 验收向量），
//! 本层只负责落盘/扫描/活动指针与流程编排。

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::Signer as _;
use serde::Serialize;

use super::{Kernel, UnlockedIdentity};
use super::error::{KernelError, Result};
use crate::identity::{self, IdentityFile};

/// `RootIdentityStatus`（root-id.ts:95-103）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct IdentityStatus {
    /// 本设备是否已有任何身份。
    pub initialized: bool,
    /// 当前是否有已解锁身份。
    pub unlocked: bool,
    /// 当前身份 rootId（解锁中的优先，其次活动指针）。
    #[serde(rename = "rootId")]
    pub root_id: Option<String>,
    /// 当前身份昵称。
    pub nickname: Option<String>,
    /// 当前身份头像 dataURL。
    pub avatar: Option<String>,
}

/// `IdentitySummary`（root-id.ts:304-311）：切换用户列表项。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct IdentitySummary {
    /// 身份 rootId。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    /// 是否为当前活动身份。
    pub active: bool,
    /// 昵称（缺省/空白为 `None`）。
    pub nickname: Option<String>,
    /// 头像 dataURL（非 `data:image/` 前缀的非法值为 `None`）。
    pub avatar: Option<String>,
}

/// `initialize` 的返回：rootId 与明文助记词（仅此一次展示，供用户备份）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitIdentityResult {
    /// 新身份 rootId。
    pub root_id: String,
    /// 24 词中文助记词（空格分隔）。
    pub mnemonic: String,
}

/// 当前已解锁身份的公开信息。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PublicIdentity {
    /// rootId。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// root 公钥 hex。
    #[serde(rename = "publicKeyHex")]
    pub public_key_hex: String,
    /// 昵称。
    pub nickname: Option<String>,
    /// 头像 dataURL。
    pub avatar: Option<String>,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// `updateProfile` 的返回：生效后的资料。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProfileInfo {
    /// 昵称。
    pub nickname: Option<String>,
    /// 头像 dataURL。
    pub avatar: Option<String>,
}

/// `sign` 的返回（TS `RootSignature`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootSignatureInfo {
    /// 签名者 rootId。
    pub root_id: String,
    /// ed25519 签名（base64，64 字节签名值）。
    pub signature: String,
    /// 载荷 sha256 hex（UTF-8 字节）。
    pub payload_hash: String,
}

/// `deriveDomainIdentity` 的返回（TS `DerivedDomainIdentity`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedDomainIdentityInfo {
    /// 数据域（原样回显，不 trim）。
    pub domain: String,
    /// 域身份 id = sha256hex(域公钥)。
    pub domain_id: String,
    /// 域公钥（base64，32 字节）。
    pub public_key: String,
    /// 完整派生路径（root 路径后追加两段硬化索引）。
    pub derivation_path: String,
}

/// `signWithDomainIdentity` 的返回（TS `DomainSignature`，root-id.ts:777）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainSignatureInfo {
    /// 数据域（原样回显，不 trim）。
    pub domain: String,
    /// 域身份 id = sha256hex(域公钥)。
    pub domain_id: String,
    /// 域公钥（base64，32 字节）。
    pub public_key: String,
    /// ed25519 签名（base64，64 字节签名值）。
    pub signature: String,
    /// 载荷 sha256 hex（UTF-8 字节）。
    pub payload_hash: String,
}

/// `root-mnemonic-check` 的返回（词数组 + 词表外词下标，供 UI 高亮错字）。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MnemonicCheckInfo {
    /// 规范化后的词数组。
    pub words: Vec<String>,
    /// 不在任何可恢复词表（中文简体/英文）中的词下标。
    pub invalid_indexes: Vec<usize>,
}

/// active-identity.json 的内容形状（紧凑序列化 = TS `JSON.stringify({activeRootId})`）。
#[derive(Serialize)]
struct ActiveIdentityFile<'a> {
    #[serde(rename = "activeRootId")]
    active_root_id: &'a str,
}

fn check_password(password: &str) -> Result<()> {
    if password.chars().count() < 8 {
        return Err(KernelError::PasswordTooShort);
    }
    Ok(())
}

/// TS `splitMnemonicInput`：接受"空格分隔"与"连续书写"（中文每词单字）两种录入，
/// 返回词数组。
fn split_mnemonic_input(input: &str) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.chars().any(char::is_whitespace) {
        trimmed.split_whitespace().map(str::to_string).collect()
    } else {
        trimmed.chars().map(|c| c.to_string()).collect()
    }
}

/// 录入规范化：词数组以单空格连接（`recover_mnemonic` 的输入归一）。
fn normalize_mnemonic_input(input: &str) -> String {
    split_mnemonic_input(input).join(" ")
}

/// 解密类错误映射：密码错误统一为 TS 的 `Invalid password`。
fn map_identity_decrypt_error(e: identity::IdentityError) -> KernelError {
    match e {
        identity::IdentityError::DecryptionFailed => KernelError::InvalidPassword,
        other => KernelError::Identity(other),
    }
}

impl Kernel {
    // ------------------------------------------------------------------
    // 目录与文件 IO
    // ------------------------------------------------------------------

    pub(crate) fn identities_dir(&self) -> PathBuf {
        self.config.data_dir.join("identities")
    }

    pub(crate) fn active_file_path(&self) -> PathBuf {
        self.config.data_dir.join("active-identity.json")
    }

    pub(crate) fn legacy_file_path(&self) -> PathBuf {
        self.config.data_dir.join("root-identity.json")
    }

    pub(crate) fn identity_file_path(&self, root_id: &str) -> PathBuf {
        self.identities_dir().join(format!("{root_id}.json"))
    }

    /// 读取身份文件；不存在返回 `Ok(None)`，损坏 JSON 返回解析错误。
    pub(crate) fn read_identity_file(&self, root_id: &str) -> Result<Option<IdentityFile>> {
        match std::fs::read_to_string(self.identity_file_path(root_id)) {
            Ok(raw) => Ok(Some(IdentityFile::from_json(&raw)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 写入身份文件（两空格缩进，对齐 TS `JSON.stringify(payload, null, 2)`）。
    pub(crate) fn write_identity_file(&self, file: &IdentityFile) -> Result<()> {
        std::fs::create_dir_all(self.identities_dir())?;
        let text = serde_json::to_string_pretty(file)?;
        std::fs::write(self.identity_file_path(&file.root_id), text)?;
        Ok(())
    }

    /// 读取活动 rootId（文件缺失/损坏均视为无）。
    pub(crate) fn read_active_root_id(&self) -> Result<Option<String>> {
        let Ok(raw) = std::fs::read_to_string(self.active_file_path()) else {
            return Ok(None);
        };
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        Ok(parsed
            .get("activeRootId")
            .and_then(|v| v.as_str())
            .map(str::to_string))
    }

    /// 写入活动指针：`{"activeRootId":"..."}`（紧凑，与 TS 逐字节一致）。
    pub(crate) fn write_active_root_id(&self, root_id: &str) -> Result<()> {
        std::fs::create_dir_all(&self.config.data_dir)?;
        let text = serde_json::to_string(&ActiveIdentityFile { active_root_id: root_id })?;
        std::fs::write(self.active_file_path(), text)?;
        Ok(())
    }

    /// 旧版单身份文件迁移（TS `migrateLegacyIfNeeded`，幂等）：
    /// `root-identity.json` → `identities/{rootId}.json` 并设为活动。
    pub(crate) fn migrate_legacy_identity_if_needed(&self) -> Result<()> {
        let Ok(raw) = std::fs::read_to_string(self.legacy_file_path()) else {
            return Ok(());
        };
        let Ok(legacy) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Ok(());
        };
        let Some(root_id) = legacy.get("rootId").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        std::fs::create_dir_all(self.identities_dir())?;
        if self.read_identity_file(root_id)?.is_none() {
            // TS 原样搬运遗留文件文本
            std::fs::write(self.identity_file_path(root_id), &raw)?;
        }
        std::fs::remove_file(self.legacy_file_path())?;
        if self.read_active_root_id()?.is_none() {
            self.write_active_root_id(root_id)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // 状态辅助
    // ------------------------------------------------------------------

    /// 当前身份 rootId：解锁中的优先，其次活动指针。
    pub fn current_root_id(&self) -> Result<Option<String>> {
        if let Some(unlocked) = &self.unlocked {
            return Ok(Some(unlocked.root_id()));
        }
        self.read_active_root_id()
    }

    /// 要求当前身份（解锁或活动指针），否则 `NotInitialized`。
    pub(crate) fn require_current_root_id(&self) -> Result<String> {
        self.current_root_id()?.ok_or(KernelError::NotInitialized)
    }

    /// 要求已解锁身份，否则 `Locked`。
    pub(crate) fn require_unlocked_root_id(&self) -> Result<String> {
        self.unlocked
            .as_ref()
            .map(UnlockedIdentity::root_id)
            .ok_or(KernelError::Locked)
    }

    /// 写入解锁状态并同步 p2p 宿主可见的当前身份指针。
    ///
    /// 会话同时缓存 BIP39 种子（域派生用）与口令（资料重封用）；`lock` 时随
    /// `unlocked` 整体清除。签名私钥同步给 org-sync worker（自签 claim 用）。
    pub(crate) fn set_unlocked(
        &mut self,
        identity: identity::Identity,
        seed: [u8; 64],
        password: &str,
    ) {
        *self.current_root_id_shared.lock().unwrap() = Some(identity.id());
        *self.signing_key_shared.lock().unwrap() = Some(identity.signing_key.clone());
        self.unlocked = Some(UnlockedIdentity {
            identity,
            seed,
            password: password.to_string(),
        });
    }

    // ------------------------------------------------------------------
    // 身份 API
    // ------------------------------------------------------------------

    /// `getStatus`：初始化/解锁状态与当前身份摘要。
    pub fn status(&self) -> Result<IdentityStatus> {
        let identities = self.list_identities()?;
        let root_id = self.current_root_id()?;
        let current = root_id
            .as_ref()
            .and_then(|rid| identities.iter().find(|item| &item.root_id == rid));
        Ok(IdentityStatus {
            initialized: !identities.is_empty(),
            unlocked: self.unlocked.is_some(),
            root_id,
            nickname: current.and_then(|item| item.nickname.clone()),
            avatar: current.and_then(|item| item.avatar.clone()),
        })
    }

    /// `listIdentities`：扫描 `identities/` 下全部身份（文件名与内容 rootId 必须一致），
    /// 按创建时间升序。
    pub fn list_identities(&self) -> Result<Vec<IdentitySummary>> {
        let active = self.read_active_root_id()?;
        let entries = match std::fs::read_dir(self.identities_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.ends_with(".json") {
                continue;
            }
            // 损坏文件跳过（TS catch continue）
            let Ok(raw) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(file) = IdentityFile::from_json(&raw) else {
                continue;
            };
            if file.root_id.is_empty() || file_name != format!("{}.json", file.root_id) {
                continue;
            }
            result.push(IdentitySummary {
                active: active.as_deref() == Some(file.root_id.as_str()),
                nickname: file.nickname.filter(|n| !n.trim().is_empty()),
                avatar: file
                    .avatar
                    .filter(|a| a.starts_with(identity::file::AVATAR_PREFIX)),
                created_at: file.created_at,
                root_id: file.root_id,
            });
        }
        result.sort_by_key(|item| item.created_at);
        Ok(result)
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
        Ok(InitIdentityResult { root_id, mnemonic })
    }

    /// `unlock`：密码解锁指定身份（缺省为活动身份），设为当前并触发存储对齐。
    /// v1 遗留文件解锁成功后按 spec §5 迁移为 v2。
    pub fn unlock(&mut self, password: &str, root_id: Option<&str>) -> Result<String> {
        let target = match root_id {
            Some(rid) => rid.to_string(),
            None => self.read_active_root_id()?.ok_or(KernelError::NotInitialized)?,
        };
        let Some(mut file) = self.read_identity_file(&target)? else {
            return Err(KernelError::Message("该账号不在本设备上".to_string()));
        };
        if file.version == identity::file::FILE_VERSION_V1 {
            file = identity::migrate_v1_to_v2(&file, password).map_err(map_identity_decrypt_error)?;
            self.write_identity_file(&file)?;
        }
        let (payload, identity) =
            identity::unlock_identity(&file, password).map_err(map_identity_decrypt_error)?;
        if identity.id() != file.root_id {
            return Err(KernelError::Message(
                "Root identity verification failed".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&payload.mnemonic)?.seed;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, password);
        Ok(root_id)
    }

    /// `lock`：锁定当前身份（活动指针不变）；会话私钥同步清除。
    pub fn lock(&mut self) {
        self.unlocked = None;
        *self.signing_key_shared.lock().unwrap() = None;
        if let Ok(active) = self.read_active_root_id() {
            *self.current_root_id_shared.lock().unwrap() = active;
        }
    }

    /// `setActive`：切换登录目标用户——仅改活动指针，不解锁、不迁移存储；
    /// 下次 `unlock`（缺省 rootId）以新活动身份为目标（对齐 TS root-id.ts
    /// `setActive` 的"仅改指针，解锁时生效"语义）。
    pub fn set_active_identity(&self, root_id: &str) -> Result<()> {
        if self.read_identity_file(root_id)?.is_none() {
            return Err(KernelError::Message("该账号不在本设备上".to_string()));
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
        let (file, identity) = identity::recover_identity(&normalized, new_password, nickname, avatar)
            .map_err(|e| match e {
                identity::IdentityError::InvalidMnemonic(_) => KernelError::Message(
                    "助记词校验失败：请检查是否有错别字、漏字或顺序错误".to_string(),
                ),
                other => KernelError::Identity(other),
            })?;
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Message(
                "该账号已在本设备上，请直接登录".to_string(),
            ));
        }
        let seed = identity::parse_mnemonic(&normalized)?.seed;
        self.write_identity_file(&file)?;
        self.write_active_root_id(&file.root_id)?;
        self.align_storage(&file.root_id)?;
        let root_id = file.root_id.clone();
        self.set_unlocked(identity, seed, new_password);
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
            .map_err(|_| KernelError::Message("备份数据无效或已损坏".to_string()))?;
        let (payload, identity) = identity::unlock_identity(&file, password).map_err(|e| match e {
            identity::IdentityError::DecryptionFailed => {
                KernelError::Message("密码不正确".to_string())
            }
            identity::IdentityError::InvalidMnemonic(_) | identity::IdentityError::Json(_) => {
                KernelError::Message("备份数据无效或已损坏".to_string())
            }
            other => KernelError::Identity(other),
        })?;
        if identity.id() != file.root_id {
            return Err(KernelError::Message(
                "备份数据校验失败：rootId 不匹配".to_string(),
            ));
        }
        if self.read_identity_file(&file.root_id)?.is_some() {
            return Err(KernelError::Message(
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
        Ok(root_id)
    }

    /// `revealMnemonic`：密码门控的助记词再次查看（解密当前身份文件）。
    pub fn reveal_mnemonic(&self, password: &str) -> Result<String> {
        let target = self.current_root_id()?.ok_or(KernelError::NotInitialized)?;
        let Some(file) = self.read_identity_file(&target)? else {
            return Err(KernelError::NotInitialized);
        };
        let payload = identity::file::decrypt_payload(&file, password)
            .map_err(map_identity_decrypt_error)?;
        Ok(payload.mnemonic)
    }

    /// `updateProfile`：更新当前已解锁身份的资料（昵称/头像）。
    ///
    /// - `nickname`：`Some(n)` 修改；`None` 不变
    /// - `avatar`：`Some(Some(a))` 设置；`Some(None)` 清除（恢复自动头像）；`None` 不变
    ///
    /// 内核身份文件把资料字段同时放在加密 payload 内（spec §5），故需要密码重新
    /// 封装；密码错误返回 `InvalidPassword`。
    pub fn update_profile(
        &mut self,
        password: &str,
        nickname: Option<&str>,
        avatar: Option<Option<&str>>,
    ) -> Result<ProfileInfo> {
        let root_id = self.require_unlocked_root_id()?;
        let Some(mut file) = self.read_identity_file(&root_id)? else {
            return Err(KernelError::NotInitialized);
        };
        identity::update_profile(&mut file, password, nickname, avatar)
            .map_err(map_identity_decrypt_error)?;
        self.write_identity_file(&file)?;
        Ok(ProfileInfo {
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
        })
    }

    /// `updateProfile` 的会话版（对齐 TS root-id.ts 现行语义：免密码——主进程持有
    /// 解锁会话直接重封资料）。内核按 spec §5 需重封加密 payload，口令取自 unlock
    /// 时缓存的会话态；`lock` 后调用报 `Locked`。
    ///
    /// 参数语义同 [`Kernel::update_profile`]。
    pub fn update_profile_session(
        &mut self,
        nickname: Option<&str>,
        avatar: Option<Option<&str>>,
    ) -> Result<ProfileInfo> {
        let (root_id, password) = {
            let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
            (unlocked.root_id(), unlocked.password.clone())
        };
        let Some(mut file) = self.read_identity_file(&root_id)? else {
            return Err(KernelError::NotInitialized);
        };
        identity::update_profile(&mut file, &password, nickname, avatar)?;
        self.write_identity_file(&file)?;
        Ok(ProfileInfo {
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
        })
    }

    /// `sign`（TS root-id.ts:725）：以当前已解锁身份的根私钥做 ed25519 签名。
    /// IPC 通道只传字符串，载荷按 UTF-8 字节取；签名 base64，payloadHash 为
    /// 载荷字节的 sha256 hex。
    pub fn sign(&self, payload: &str) -> Result<RootSignatureInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let signature = unlocked.identity.signing_key.sign(payload.as_bytes());
        Ok(RootSignatureInfo {
            root_id: unlocked.root_id(),
            signature: B64.encode(signature.to_bytes()),
            payload_hash: crate::evidence::sha256_hex(payload),
        })
    }

    /// `deriveDomainIdentity`（TS root-id.ts:759）：由会话缓存的 BIP39 种子派生
    /// 域身份（root 路径后追加 `/{idxA}'/{idxB}'`，索引取自 sha256(domain) 前 8
    /// 字节）。域密钥即时派生、不持久化（对齐 TS 安全说明）。
    pub fn derive_domain_identity(&self, domain: &str) -> Result<DerivedDomainIdentityInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        if domain.trim().is_empty() {
            return Err(KernelError::Message("Domain is required".to_string()));
        }
        let derived = identity::derive_domain_identity(&unlocked.seed, domain);
        Ok(DerivedDomainIdentityInfo {
            domain: domain.to_string(),
            domain_id: derived.id(),
            public_key: B64.encode(derived.public_key()),
            derivation_path: derived.path.clone(),
        })
    }

    /// `signWithDomainIdentity`（TS root-id.ts:777）：以域身份私钥做 ed25519
    /// 签名。域密钥由根种子即时派生、仅存在于本方法调用栈内（不持久化、不返回），
    /// 调用方只能拿到签名与公钥；根身份不暴露。
    pub fn sign_with_domain_identity(
        &self,
        domain: &str,
        payload: &str,
    ) -> Result<DomainSignatureInfo> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        if domain.trim().is_empty() {
            return Err(KernelError::Message("Domain is required".to_string()));
        }
        let derived = identity::derive_domain_identity(&unlocked.seed, domain);
        let signature = derived.signing_key.sign(payload.as_bytes());
        Ok(DomainSignatureInfo {
            domain: domain.to_string(),
            domain_id: derived.id(),
            public_key: B64.encode(derived.public_key()),
            signature: B64.encode(signature.to_bytes()),
            payload_hash: crate::evidence::sha256_hex(payload),
        })
    }

    /// `root-mnemonic-check`（ipc/identity.ts:76-80）：录入助记词时逐词校验，
    /// 返回词数组与词表外词下标。纯函数，不需要身份态。
    pub fn check_mnemonic(input: &str) -> MnemonicCheckInfo {
        let words = split_mnemonic_input(input);
        let invalid_indexes = identity::find_invalid_mnemonic_words(&words);
        MnemonicCheckInfo {
            words,
            invalid_indexes,
        }
    }

    /// 当前已解锁身份的公开信息；锁定时返回 `Ok(None)`。
    pub fn current_identity(&self) -> Result<Option<PublicIdentity>> {
        let Some(unlocked) = &self.unlocked else {
            return Ok(None);
        };
        let root_id = unlocked.root_id();
        let file = self.read_identity_file(&root_id)?.ok_or(KernelError::NotInitialized)?;
        Ok(Some(PublicIdentity {
            public_key_hex: unlocked.identity.public_key_hex(),
            nickname: file.nickname.clone(),
            avatar: file.avatar.clone(),
            created_at: file.created_at,
            root_id,
        }))
    }
}

// ------------------------------------------------------------------
// 单元测试：域身份签名（`plugin-identity-sign` 的内核侧）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Kernel, KernelConfig};

    const PASSWORD: &str = "correct-horse-battery";
    const DOMAIN: &str = "plugin:weibo-core";
    const PAYLOAD: &str = "org_123:post_456:hello spark";

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
    fn sign_with_domain_identity_roundtrip() {
        let (_dir, mut kernel) = temp_kernel();

        // 锁定 → TS `Root identity is locked`
        assert_eq!(
            kernel
                .sign_with_domain_identity(DOMAIN, PAYLOAD)
                .unwrap_err()
                .to_string(),
            "Root identity is locked"
        );

        kernel.init_identity(PASSWORD, "alice", None).unwrap();

        // 固定载荷签名：形状齐全，验签通过
        let sig = kernel.sign_with_domain_identity(DOMAIN, PAYLOAD).unwrap();
        assert_eq!(sig.domain, DOMAIN);
        assert_eq!(sig.payload_hash, crate::evidence::sha256_hex(PAYLOAD));
        assert!(identity::verify_ed25519_signature(
            PAYLOAD,
            &sig.signature,
            &sig.public_key
        ));

        // domainId 与 derive_domain_identity 一致（= sha256hex(域公钥)）
        let derived = kernel.derive_domain_identity(DOMAIN).unwrap();
        assert_eq!(sig.domain_id, derived.domain_id);
        assert_eq!(sig.public_key, derived.public_key);

        // 确定性：同域同载荷再签结果一致
        let sig2 = kernel.sign_with_domain_identity(DOMAIN, PAYLOAD).unwrap();
        assert_eq!(sig, sig2);

        // 与 root sign 区分：签名者公钥/签名均不同
        let root_sig = kernel.sign(PAYLOAD).unwrap();
        let root_public_key_hex = kernel.current_identity().unwrap().unwrap().public_key_hex;
        assert_ne!(
            B64.decode(&sig.public_key).unwrap(),
            hex::decode(root_public_key_hex).unwrap()
        );
        assert_ne!(sig.signature, root_sig.signature);
        // root 签名用域公钥验不过，域签名亦然（payloadHash 口径一致，仅签名者不同）
        assert!(!identity::verify_ed25519_signature(
            PAYLOAD,
            &root_sig.signature,
            &sig.public_key
        ));

        // 不同域 → 不同域公钥；篡改载荷验签失败
        let other = kernel
            .sign_with_domain_identity("plugin:chat", PAYLOAD)
            .unwrap();
        assert_ne!(other.public_key, sig.public_key);
        assert!(!identity::verify_ed25519_signature(
            "tampered",
            &sig.signature,
            &sig.public_key
        ));
        // 坏 base64 / 长度不符 → false（不 panic）
        assert!(!identity::verify_ed25519_signature(PAYLOAD, "!!!", &sig.public_key));
        assert!(!identity::verify_ed25519_signature(PAYLOAD, &sig.signature, "aGk="));

        // 空域 → TS `Domain is required`
        assert_eq!(
            kernel
                .sign_with_domain_identity("  ", PAYLOAD)
                .unwrap_err()
                .to_string(),
            "Domain is required"
        );
    }
}
