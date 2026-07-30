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
//!
//! 代码组织：本文件为公开信息类型、身份文件 IO、状态辅助与查询（status/
//! list_identities）；登录链路（注册/解锁/锁定/恢复）在 `login`，资料维护与
//! 签名/派生在 `profile`；单测在 `core/tests/unit_core/kernel_identity.rs`。

mod login;
mod profile;

use std::path::PathBuf;

use serde::Serialize;

use super::error::{KernelError, Result};
use super::{Kernel, UnlockedIdentity};
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
        let text = serde_json::to_string(&ActiveIdentityFile {
            active_root_id: root_id,
        })?;
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
        // 昵称/头像共享格同步（dm 入站应答用；身份文件在调用点前已落盘）
        let profile = self
            .read_identity_file(&identity.id())
            .ok()
            .flatten();
        let nickname = profile
            .as_ref()
            .and_then(|f| f.nickname.clone())
            .unwrap_or_default();
        *self.nickname_shared.lock().unwrap() = nickname;
        let avatar = profile.and_then(|f| f.avatar).unwrap_or_default();
        *self.avatar_shared.lock().unwrap() = avatar;
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
}
