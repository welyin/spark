//! 身份文件结构（serde）与资料字段校验。
//!
//! 磁盘文件 `{rootId}.json`，UTF-8 JSON：
//! - v2 字段：`{version, kdf, salt, iv, data, authTag, publicKeyHex, rootId,
//!   nickname?, avatar?, gender?, region?, signature?, createdAt, updatedAt}`
//!   （全 hex 编码，authTag 单独存储；三个扩展字段为明文可选键）
//! - v1 legacy：同布局但 `kdf:"pbkdf2"`、无 authTag、iv 16B（只读兼容，解锁后迁移 v2）
//!
//! 加密 payload 明文 JSON：`{mnemonic, derivationPath, version, wordlist?,
//! nickname?, avatar?, gender?, region?, signature?, createdAt}`。
//!
//! 注：规格 §5 将 payload 路径字段记作 `path`，但 golden vectors 的真实明文
//! （从 TS 实现逐字节复刻）使用 `derivationPath`。此处以向量为准，序列化输出
//! `derivationPath`，反序列化同时接受别名 `path`。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::Rng;
use serde::{Deserialize, Serialize};

use super::crypto;
use super::derive::{Identity, derive_identity_at_path, derive_root_identity};
use super::error::{IdentityError, Result};
use super::mnemonic::{Wordlist, generate_mnemonic, parse_mnemonic};

/// 当前身份文件版本。
pub const FILE_VERSION_V2: u32 = 2;
/// v1 legacy 版本号。
pub const FILE_VERSION_V1: u32 = 1;
/// v2 KDF 标识。
pub const KDF_SCRYPT: &str = "scrypt";
/// v1 KDF 标识。
pub const KDF_PBKDF2: &str = "pbkdf2";
/// 昵称最大长度（trim 后字符数）。
pub const NICKNAME_MAX_CHARS: usize = 24;
/// 扩展字段字符数上限（宽限口径；UI 上限更小：性别单选、地区 20、签名 30）。
pub const GENDER_MAX_CHARS: usize = 16;
pub const REGION_MAX_CHARS: usize = 64;
pub const SIGNATURE_MAX_CHARS: usize = 128;
/// 头像序列化后最大字节数（200KB）。
pub const AVATAR_MAX_SERIALIZED_BYTES: usize = 200 * 1024;
/// 头像 data URL 前缀。
pub const AVATAR_PREFIX: &str = "data:image/";
/// 备份码紧凑格式版本（与顶层 `v` 对齐，独立字段便于单测）。
pub const COMPACT_BACKUP_VERSION: u32 = 2;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_payload_version() -> u32 {
    FILE_VERSION_V2
}

/// 加密 payload（身份文件 `data` 字段解密后的明文）。
///
/// 只放真正需要保密的字段：助记词 + 派生路径（+ 词表/版本/创建时间）。
/// 资料字段（昵称/头像/性别/地区/签名）**不进 payload**——它们本就明文存于
/// `IdentityFile` 头部、无保密需求，且资料更新不再触碰加密（避免重封换 salt
/// 导致会话缓存密钥失效的 decryption failed，见 kernel/identity/profile.rs）。
/// 历史文件的 payload 曾带这些资料字段：反序列化忽略之，资料以明文头为准。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityPayload {
    /// 助记词（空格分隔）。
    pub mnemonic: String,
    /// 派生路径；序列化为 `derivationPath`（与真实实现/向量一致），接受别名 `path`。
    #[serde(rename = "derivationPath", alias = "path")]
    pub path: String,
    /// payload 版本（当前 2）。
    #[serde(default = "default_payload_version")]
    pub version: u32,
    /// 词表标识（`chinese_simplified` / `english`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wordlist: Option<String>,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}

/// 磁盘身份文件（`{rootId}.json`）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityFile {
    /// 文件版本（1 = v1 legacy，2 = 当前）。
    pub version: u32,
    /// KDF 标识（`scrypt` / `pbkdf2`）。
    pub kdf: String,
    /// KDF salt（hex）。
    pub salt: String,
    /// 加密 IV（hex；v2 12B，v1 16B）。
    pub iv: String,
    /// 密文（hex）。
    pub data: String,
    /// GCM authTag（hex；仅 v2）。
    #[serde(rename = "authTag", skip_serializing_if = "Option::is_none")]
    pub auth_tag: Option<String>,
    /// root 公钥 hex。
    #[serde(rename = "publicKeyHex")]
    pub public_key_hex: String,
    /// rootId = sha256hex(publicKey)。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 昵称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 头像 data URL。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// 性别（扩展字段；旧文件无此字段可读，`None` 不序列化）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// 地区（扩展字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 个性签名（扩展字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    /// 更新时间（ms）。
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
}

impl IdentityFile {
    /// 从 JSON 字符串解析身份文件。
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// 序列化为 JSON 字符串。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// 备份二维码紧凑载荷：独立于磁盘 [`IdentityFile`] 的专用结构体，自带独立
/// Serialize/Deserialize，与磁盘格式完全解耦（互不继承、互不牵连）。
///
/// 相对磁盘格式的取舍（只存在于本结构体，不回写磁盘）：
/// - `salt`/`iv`/`data`/`authTag` 由 hex 改为 base64（省 1/3 体积）；
/// - 删除 `publicKeyHex`（64 字符 hex）与 `version`（内层版本由 `v` 承担）；
/// - `rootId` 保留作防篡改校验锚点（恢复端用派生公钥重算后比对）；
/// - 保留 `kdf`/`nickname`/`createdAt`/`updatedAt`（资料状态未知恒置 0）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactBackupFile {
    /// 内层格式版本（当前 2）。
    pub v: u32,
    /// KDF 标识（`scrypt`）。
    pub kdf: String,
    /// KDF salt（base64，16B）。
    pub salt: String,
    /// 加密 IV（base64，12B）。
    pub iv: String,
    /// 密文（base64）。
    pub data: String,
    /// GCM authTag（base64，16B）。
    #[serde(rename = "authTag")]
    pub auth_tag: String,
    /// rootId = sha256hex(publicKey)，防篡改校验锚点。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 昵称（可选；恢复后直接有昵称，体验）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 创建时间（ms；profile-sync LWW 时间戳锚点）。
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    /// 更新时间（ms；紧凑备份资料字段残缺，恒置 0 = 资料状态未知）。
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
}

/// 校验昵称：trim 后 1–24 字符。返回 trim 后的昵称。
pub fn validate_nickname(nickname: &str) -> Result<String> {
    let trimmed = nickname.trim();
    let chars = trimmed.chars().count();
    if chars == 0 {
        return Err(IdentityError::InvalidNickname("nickname is empty".into()));
    }
    if chars > NICKNAME_MAX_CHARS {
        return Err(IdentityError::InvalidNickname(format!(
            "nickname too long: {chars} chars > {NICKNAME_MAX_CHARS}"
        )));
    }
    Ok(trimmed.to_string())
}

/// 校验头像：必须 `data:image/` 前缀，JSON 序列化后 ≤200KB。
pub fn validate_avatar(avatar: &str) -> Result<()> {
    if !avatar.starts_with(AVATAR_PREFIX) {
        return Err(IdentityError::InvalidAvatar(format!(
            "avatar must start with `{AVATAR_PREFIX}`"
        )));
    }
    let serialized_len = serde_json::to_string(avatar)?.len();
    if serialized_len > AVATAR_MAX_SERIALIZED_BYTES {
        return Err(IdentityError::InvalidAvatar(format!(
            "avatar too large: {serialized_len} bytes serialized > {AVATAR_MAX_SERIALIZED_BYTES}"
        )));
    }
    Ok(())
}

/// sanitize 外部资料字段（recoverFromBackup 写入前调用）：非法值一律丢弃。
pub fn sanitize_profile(
    nickname: Option<&str>,
    avatar: Option<&str>,
) -> (Option<String>, Option<String>) {
    let nickname = nickname.and_then(|n| validate_nickname(n).ok());
    let avatar = avatar
        .filter(|a| validate_avatar(a).is_ok())
        .map(str::to_string);
    (nickname, avatar)
}

/// 生成新身份：24 词中文助记词 → root 派生 → v2 加密落盘结构。
///
/// 返回 `(身份文件, root 身份)`；助记词仅在 payload 密文中保存。
pub fn create_identity(
    password: &str,
    nickname: &str,
    avatar: Option<&str>,
) -> Result<(IdentityFile, Identity)> {
    let mnemonic = generate_mnemonic()?;
    recover_identity(&mnemonic, password, nickname, avatar)
}

/// 从助记词恢复身份（注册/助记词恢复路径；词表自动探测）。
pub fn recover_identity(
    mnemonic: &str,
    password: &str,
    nickname: &str,
    avatar: Option<&str>,
) -> Result<(IdentityFile, Identity)> {
    let (file, identity, _key) = recover_identity_and_key(mnemonic, password, nickname, avatar)?;
    Ok((file, identity))
}

/// 同 [`recover_identity`]，额外回传 v2 封装所用的 scrypt 派生密钥
/// （内核解锁会话缓存用：资料重封复用该密钥，免重跑 KDF）。
pub fn recover_identity_and_key(
    mnemonic: &str,
    password: &str,
    nickname: &str,
    avatar: Option<&str>,
) -> Result<(IdentityFile, Identity, [u8; crypto::KEY_LEN])> {
    let nickname = validate_nickname(nickname)?;
    if let Some(a) = avatar {
        validate_avatar(a)?;
    }
    let parsed = parse_mnemonic(mnemonic)?;
    let identity = derive_root_identity(&parsed.seed);
    let now = now_ms();
    let payload = IdentityPayload {
        mnemonic: parsed.mnemonic,
        path: identity.path.clone(),
        version: FILE_VERSION_V2,
        wordlist: Some(parsed.wordlist.as_str().to_string()),
        created_at: Some(now),
    };
    let (file, key) = seal_v2_and_key(
        &payload,
        password,
        identity.public_key_hex(),
        identity.id(),
        Some(nickname),
        avatar.map(str::to_string),
        None,
        None,
        None,
        now,
        now,
    )?;
    Ok((file, identity, key))
}

/// 解锁身份文件（v2 / v1 均可），返回 `(payload, 按 payload.path 派生的身份)`。
pub fn unlock_identity(file: &IdentityFile, password: &str) -> Result<(IdentityPayload, Identity)> {
    let (payload, identity, _key) = unlock_identity_and_key(file, password)?;
    Ok((payload, identity))
}

/// 同 [`unlock_identity`]，额外回传 v2 scrypt 派生密钥（v1 文件为 `None`）：
/// 供内核解锁会话缓存，后续资料重封复用该密钥，免重跑 KDF。
pub fn unlock_identity_and_key(
    file: &IdentityFile,
    password: &str,
) -> Result<(IdentityPayload, Identity, Option<[u8; crypto::KEY_LEN]>)> {
    let (payload, key) = decrypt_payload_and_key(file, password)?;
    let parsed = parse_mnemonic(&payload.mnemonic)?;
    let identity = derive_identity_at_path(&parsed.seed, &payload.path)?;
    Ok((payload, identity, key))
}

/// v1 文件迁移到 v2：解密 → sanitize 资料 → v2 重新加密。
///
/// 迁移后 nickname/avatar 取 v1 文件层（缺失则取 payload 层）并做 sanitize；
/// createdAt 保留，updatedAt 刷新。
pub fn migrate_v1_to_v2(file: &IdentityFile, password: &str) -> Result<IdentityFile> {
    if file.version != FILE_VERSION_V1 {
        return Err(IdentityError::UnsupportedVersion(file.version));
    }
    let payload = decrypt_payload(file, password)?;
    // 资料以 v1 文件明文头为准（payload 已无资料字段），sanitize 后写回明文头。
    let (nickname, avatar) = sanitize_profile(file.nickname.as_deref(), file.avatar.as_deref());
    let now = now_ms();
    let new_payload = IdentityPayload {
        mnemonic: payload.mnemonic,
        path: payload.path,
        version: FILE_VERSION_V2,
        wordlist: payload
            .wordlist
            .or_else(|| Some(Wordlist::English.as_str().to_string())),
        created_at: Some(file.created_at),
    };
    seal_v2(
        &new_payload,
        password,
        file.public_key_hex.clone(),
        file.root_id.clone(),
        nickname,
        avatar,
        file.gender.clone(),
        file.region.clone(),
        file.signature.clone(),
        file.created_at,
        now,
    )
}

/// 更新资料（昵称/头像 + 扩展字段性别/地区/签名）：**纯明文头更新**，不触碰
/// 加密 payload（资料字段已移出 payload，无保密需求）。仅校验 + 刷新明文头与
/// `updatedAt`；不解密、不重封、不换 salt——因此不会使会话缓存密钥失效。
///
/// `password` 参数仅为保持调用方签名而保留（当前不使用）；资料更新无需密码。
///
/// - `nickname`：`Some(n)` 修改；`None` 不变。
/// - `avatar`：`Some(Some(a))` 设置；`Some(None)` 清除；`None` 不变。
/// - `gender`/`region`/`signature`：`Some(非空)` 设置；`Some("")` 清除；
///   `None` 不变（与前端 `'' = 未设置` 的模型对齐）。
pub fn update_profile(
    file: &mut IdentityFile,
    password: &str,
    nickname: Option<&str>,
    avatar: Option<Option<&str>>,
    gender: Option<&str>,
    region: Option<&str>,
    signature: Option<&str>,
) -> Result<()> {
    let _ = password; // 资料明文存储，无需密码
    patch_profile_fields(file, nickname, avatar, gender, region, signature)
}

/// 同 [`update_profile`]。历史版本中此函数回传重封的新密钥供会话缓存刷新；
/// 资料改为明文存储后不再重封、密钥不变，返回固定零密钥仅为保持调用方签名
/// （调用方对返回值的赋值是无害的幂等操作，会话密钥语义以 unlock 时为准）。
#[allow(clippy::too_many_arguments)]
pub fn update_profile_and_key(
    file: &mut IdentityFile,
    password: &str,
    nickname: Option<&str>,
    avatar: Option<Option<&str>>,
    gender: Option<&str>,
    region: Option<&str>,
    signature: Option<&str>,
) -> Result<[u8; crypto::KEY_LEN]> {
    update_profile(file, password, nickname, avatar, gender, region, signature)?;
    Ok([0u8; crypto::KEY_LEN])
}

/// 历史会话缓存密钥版。资料明文存储后与 [`update_profile`] 等价（key 不再用于
/// 资料重封）。保留仅为兼容 `update_profile_session` 的调用签名。
pub fn update_profile_with_key(
    file: &mut IdentityFile,
    key: &[u8; crypto::KEY_LEN],
    nickname: Option<&str>,
    avatar: Option<Option<&str>>,
    gender: Option<&str>,
    region: Option<&str>,
    signature: Option<&str>,
) -> Result<()> {
    let _ = key; // 资料明文存储，无需密钥
    patch_profile_fields(file, nickname, avatar, gender, region, signature)
}

/// 资料明文头补丁（`update_profile*` 系列共用）：校验后写 `IdentityFile` 明文头，
/// 刷新 `updatedAt`。不动加密 payload / salt / iv / data。
fn patch_profile_fields(
    file: &mut IdentityFile,
    nickname: Option<&str>,
    avatar: Option<Option<&str>>,
    gender: Option<&str>,
    region: Option<&str>,
    signature: Option<&str>,
) -> Result<()> {
    // 内容未变的补丁不得刷新 updatedAt：同步采纳路径（apply_self_profile /
    // apply_profile_from_sled）会对内容相同的快照重复调用本函数，无条件
    // 重盖 updatedAt=now 会让 profile-sync 快照 ts 逐跳递增，两端互发永不
    // 收敛（真机实测 100% CPU 的根因）。仅实际变更时推进时间戳。
    let before = (
        file.nickname.clone(),
        file.avatar.clone(),
        file.gender.clone(),
        file.region.clone(),
        file.signature.clone(),
    );
    if let Some(n) = nickname {
        file.nickname = Some(validate_nickname(n)?);
    }
    if let Some(a) = avatar {
        file.avatar = match a {
            Some(a) => {
                validate_avatar(a)?;
                Some(a.to_string())
            }
            None => None,
        };
    }
    file.gender = patch_extra_field(file.gender.take(), gender, "gender", GENDER_MAX_CHARS)?;
    file.region = patch_extra_field(file.region.take(), region, "region", REGION_MAX_CHARS)?;
    file.signature = patch_extra_field(
        file.signature.take(),
        signature,
        "signature",
        SIGNATURE_MAX_CHARS,
    )?;
    if file.nickname != before.0
        || file.avatar != before.1
        || file.gender != before.2
        || file.region != before.3
        || file.signature != before.4
    {
        file.updated_at = now_ms();
    }
    Ok(())
}

/// 扩展字段补丁语义：`None` 不变；`Some("")`（或全空白）清除；其余校验长度后设置
/// （字符数上限防身份文件被刷大；UI 自身上限更小——地区 20/签名 30，此处取宽限）。
///
/// pub：组织成员身份写路径（org::service `update_my_identity`）复用同一口径。
pub fn patch_extra_field(
    current: Option<String>,
    patch: Option<&str>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>> {
    match patch {
        None => Ok(current),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => {
            let chars = v.chars().count();
            if chars > max_chars {
                return Err(IdentityError::InvalidProfileField(format!(
                    "{field} too long: {chars} chars > {max_chars}"
                )));
            }
            Ok(Some(v.trim().to_string()))
        }
    }
}

/// 解密身份文件 payload（按 version 分派 v2/v1）。
pub fn decrypt_payload(file: &IdentityFile, password: &str) -> Result<IdentityPayload> {
    Ok(decrypt_payload_and_key(file, password)?.0)
}

/// 解密 payload 并回传 v2 scrypt 派生密钥（v1 为 `None`）：供内核解锁会话
/// 缓存，后续资料重封复用该密钥，免重跑 KDF。
pub fn decrypt_payload_and_key(
    file: &IdentityFile,
    password: &str,
) -> Result<(IdentityPayload, Option<[u8; crypto::KEY_LEN]>)> {
    let salt = hex::decode(&file.salt)?;
    let iv = hex::decode(&file.iv)?;
    let data = hex::decode(&file.data)?;
    match file.version {
        FILE_VERSION_V2 => {
            if file.kdf != KDF_SCRYPT {
                return Err(IdentityError::MalformedFile(format!(
                    "v2 file with kdf `{}`",
                    file.kdf
                )));
            }
            let auth_tag =
                hex::decode(file.auth_tag.as_deref().ok_or_else(|| {
                    IdentityError::MalformedFile("v2 file missing authTag".into())
                })?)?;
            let key = crypto::scrypt_v2_key(password, &salt)?;
            let plaintext = crypto::decrypt_v2_with_key(&data, &auth_tag, &key, &iv)?;
            Ok((serde_json::from_slice(&plaintext)?, Some(key)))
        }
        FILE_VERSION_V1 => {
            if file.kdf != KDF_PBKDF2 {
                return Err(IdentityError::MalformedFile(format!(
                    "v1 file with kdf `{}`",
                    file.kdf
                )));
            }
            let plaintext = crypto::decrypt_v1(&data, password, &salt, &iv)?;
            Ok((serde_json::from_slice(&plaintext)?, None))
        }
        other => Err(IdentityError::UnsupportedVersion(other)),
    }
}

/// 以会话缓存的 v2 派生密钥解密 payload（免 KDF；salt 不参与解密，仅密钥
/// 派生时需要）。仅 v2 文件。
pub fn decrypt_payload_with_key(
    file: &IdentityFile,
    key: &[u8; crypto::KEY_LEN],
) -> Result<IdentityPayload> {
    if file.version != FILE_VERSION_V2 {
        return Err(IdentityError::UnsupportedVersion(file.version));
    }
    if file.kdf != KDF_SCRYPT {
        return Err(IdentityError::MalformedFile(format!(
            "v2 file with kdf `{}`",
            file.kdf
        )));
    }
    let iv = hex::decode(&file.iv)?;
    let data = hex::decode(&file.data)?;
    let auth_tag = hex::decode(
        file.auth_tag
            .as_deref()
            .ok_or_else(|| IdentityError::MalformedFile("v2 file missing authTag".into()))?,
    )?;
    let plaintext = crypto::decrypt_v2_with_key(&data, &auth_tag, key, &iv)?;
    Ok(serde_json::from_slice(&plaintext)?)
}

/// 组装二维码备份的紧凑载荷：payload 剔除 avatar 及其他可选大字段
/// （gender/region/signature），仅保留身份恢复必需的
/// mnemonic/path/version/wordlist/nickname/createdAt，同口令重新加密
/// （新 salt/iv），产 [`CompactBackupFile`]（不再走磁盘 `seal_v2`）。
/// 二维码容量有限（约 3KB），完整文件备份见 `backup_payload`。
///
/// `updatedAt` 置 0：紧凑备份的资料字段是残缺的（avatar 等被剔除），不能
/// 携带源文件的资料时间戳——否则恢复端会以「残缺的最新资料」在 profile-sync
/// LWW 裁决中挤掉对端的完整资料（严格大于才应用，同时间戳不应用，残缺快照
/// 永远赢）。置 0 表示「资料状态未知」，恢复端收到的任何全量快照都严格更新、
/// 必然被应用（identity.md §5「恢复后头像经 profile-sync 找回」）。
pub fn seal_compact_backup(
    file: &IdentityFile,
    identity: &Identity,
    payload: &IdentityPayload,
    password: &str,
) -> Result<CompactBackupFile> {
    build_compact_backup(
        identity,
        payload,
        password,
        file.nickname.clone(),
        file.created_at,
    )
}

/// 构造紧凑备份载荷：从身份派生公钥/rootId，随机 salt 派生 scrypt 密钥、
/// 随机 iv 加密 payload（`crypto::encrypt_v2_with_key`），字段 base64 化。
///
/// 与磁盘 `seal_v2` 系列完全解耦——不经过磁盘 `IdentityFile`，`publicKeyHex`
/// 不落盘；恢复端从派生公钥重算补全。
pub fn build_compact_backup(
    identity: &Identity,
    payload: &IdentityPayload,
    password: &str,
    nickname: Option<String>,
    created_at: u64,
) -> Result<CompactBackupFile> {
    let compact = IdentityPayload {
        mnemonic: payload.mnemonic.clone(),
        path: payload.path.clone(),
        version: payload.version,
        wordlist: payload.wordlist.clone(),
        created_at: payload.created_at,
    };
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let key = crypto::scrypt_v2_key(password, &salt)?;
    let mut iv = [0u8; crypto::GCM_IV_LEN];
    rand::rng().fill_bytes(&mut iv);
    let plaintext = serde_json::to_vec(&compact)?;
    let (data, auth_tag) = crypto::encrypt_v2_with_key(&plaintext, &key, &iv)?;
    Ok(CompactBackupFile {
        v: COMPACT_BACKUP_VERSION,
        kdf: KDF_SCRYPT.to_string(),
        salt: B64.encode(salt),
        iv: B64.encode(iv),
        data: B64.encode(data),
        auth_tag: B64.encode(auth_tag),
        root_id: identity.id(),
        nickname,
        created_at,
        updated_at: 0,
    })
}

/// 把紧凑备份载荷 → 磁盘 [`IdentityFile`]：base64 字段回 hex、补 `version:2`。
///
/// `publicKeyHex`（磁盘必填）从派生公钥重算——本函数不持有口令，故只回
/// hex 化的 `pk` 派生值前的占位；恢复端解锁派生 identity 后须以
/// `identity.public_key_hex()` 覆写（权威来源）。损坏输入 fail-closed：
/// 版本/kdf 不符、base64 解码失败、salt/iv/authTag 长度不符（QR 扫码位翻转
/// 会损坏这些字段）一律报错，统一归「备份数据无效或已损坏」，避免误报
/// 「密码不正确」/Crypto。
pub fn decode_compact_backup(c: &CompactBackupFile) -> Result<IdentityFile> {
    if c.v != COMPACT_BACKUP_VERSION {
        return Err(IdentityError::UnsupportedVersion(c.v));
    }
    if c.kdf != KDF_SCRYPT {
        return Err(IdentityError::MalformedFile(format!(
            "compact backup with kdf `{}`",
            c.kdf
        )));
    }
    let b64_err = |_| IdentityError::MalformedFile("compact backup base64 decode failed".into());
    let salt = B64.decode(&c.salt).map_err(b64_err)?;
    let iv = B64.decode(&c.iv).map_err(b64_err)?;
    let data = B64.decode(&c.data).map_err(b64_err)?;
    let auth_tag = B64.decode(&c.auth_tag).map_err(b64_err)?;
    if salt.len() != 16 {
        return Err(IdentityError::MalformedFile(format!(
            "compact backup salt must be 16 bytes, got {}",
            salt.len()
        )));
    }
    if iv.len() != crypto::GCM_IV_LEN {
        return Err(IdentityError::MalformedFile(format!(
            "compact backup iv must be {} bytes, got {}",
            crypto::GCM_IV_LEN,
            iv.len()
        )));
    }
    if auth_tag.len() != crypto::GCM_TAG_LEN {
        return Err(IdentityError::MalformedFile(format!(
            "compact backup authTag must be {} bytes, got {}",
            crypto::GCM_TAG_LEN,
            auth_tag.len()
        )));
    }
    Ok(IdentityFile {
        version: FILE_VERSION_V2,
        kdf: KDF_SCRYPT.to_string(),
        salt: hex::encode(salt),
        iv: hex::encode(iv),
        data: hex::encode(data),
        auth_tag: Some(hex::encode(auth_tag)),
        // 磁盘必填字段；恢复端解锁后以派生公钥覆写（权威来源）。
        public_key_hex: String::new(),
        root_id: c.root_id.clone(),
        nickname: c.nickname.clone(),
        avatar: None,
        gender: None,
        region: None,
        signature: None,
        created_at: c.created_at,
        updated_at: c.updated_at,
    })
}

/// `changePassword`：以旧口令验证解密当前 payload 后，用新口令重新封装
/// （新 salt/iv；明文头资料字段保留、createdAt 不变、updatedAt 刷新）。
/// 返回新文件与新会话封装密钥（供内核更新解锁会话缓存，避免后续路径以
/// 旧口令重封身份文件）。
///
/// 旧口令错误返回 [`IdentityError::DecryptionFailed`]（内核映射为
/// `InvalidPassword`，与 reveal_mnemonic 同口径）。
pub fn change_password(
    file: &IdentityFile,
    old_password: &str,
    new_password: &str,
) -> Result<(IdentityFile, [u8; crypto::KEY_LEN])> {
    let payload = decrypt_payload(file, old_password)?;
    seal_v2_and_key(
        &payload,
        new_password,
        file.public_key_hex.clone(),
        file.root_id.clone(),
        file.nickname.clone(),
        file.avatar.clone(),
        file.gender.clone(),
        file.region.clone(),
        file.signature.clone(),
        file.created_at,
        now_ms(),
    )
}

/// v2 加密并组装身份文件（随机 salt/iv）。
#[allow(clippy::too_many_arguments)]
fn seal_v2(
    payload: &IdentityPayload,
    password: &str,
    public_key_hex: String,
    root_id: String,
    nickname: Option<String>,
    avatar: Option<String>,
    gender: Option<String>,
    region: Option<String>,
    signature: Option<String>,
    created_at: u64,
    updated_at: u64,
) -> Result<IdentityFile> {
    let (file, _key) = seal_v2_and_key(
        payload,
        password,
        public_key_hex,
        root_id,
        nickname,
        avatar,
        gender,
        region,
        signature,
        created_at,
        updated_at,
    )?;
    Ok(file)
}

/// v2 加密并组装身份文件（随机 salt/iv），回传 scrypt 派生密钥。
#[allow(clippy::too_many_arguments)]
pub(crate) fn seal_v2_and_key(
    payload: &IdentityPayload,
    password: &str,
    public_key_hex: String,
    root_id: String,
    nickname: Option<String>,
    avatar: Option<String>,
    gender: Option<String>,
    region: Option<String>,
    signature: Option<String>,
    created_at: u64,
    updated_at: u64,
) -> Result<(IdentityFile, [u8; crypto::KEY_LEN])> {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    let key = crypto::scrypt_v2_key(password, &salt)?;
    let file = seal_v2_with_key(
        payload,
        &key,
        salt,
        public_key_hex,
        root_id,
        nickname,
        avatar,
        gender,
        region,
        signature,
        created_at,
        updated_at,
    )?;
    Ok((file, key))
}

/// v2 加密并组装身份文件（预派生密钥 + 指定 salt，随机 iv，免 KDF）。
///
/// salt 必须与派生 key 时所用的一致——密钥 = scrypt(password, salt)，文件头
/// salt 与密钥派生绑定；换了 salt 而密码不变会导致解锁时派生出不同密钥。
#[allow(clippy::too_many_arguments)]
fn seal_v2_with_key(
    payload: &IdentityPayload,
    key: &[u8; crypto::KEY_LEN],
    salt: [u8; 16],
    public_key_hex: String,
    root_id: String,
    nickname: Option<String>,
    avatar: Option<String>,
    gender: Option<String>,
    region: Option<String>,
    signature: Option<String>,
    created_at: u64,
    updated_at: u64,
) -> Result<IdentityFile> {
    let mut iv = [0u8; crypto::GCM_IV_LEN];
    rand::rng().fill_bytes(&mut iv);
    let plaintext = serde_json::to_vec(payload)?;
    let (data, auth_tag) = crypto::encrypt_v2_with_key(&plaintext, key, &iv)?;
    Ok(IdentityFile {
        version: FILE_VERSION_V2,
        kdf: KDF_SCRYPT.to_string(),
        salt: hex::encode(salt),
        iv: hex::encode(iv),
        data: hex::encode(data),
        auth_tag: Some(hex::encode(auth_tag)),
        public_key_hex,
        root_id,
        nickname,
        avatar,
        gender,
        region,
        signature,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::derive::derive_root_identity;

    fn sample_identity() -> (Identity, IdentityPayload, String) {
        let password = "correct-horse-99".to_string();
        let mnemonic = generate_mnemonic().unwrap();
        let parsed = parse_mnemonic(&mnemonic).unwrap();
        let identity = derive_root_identity(&parsed.seed);
        let payload = IdentityPayload {
            mnemonic: parsed.mnemonic,
            path: identity.path.clone(),
            version: FILE_VERSION_V2,
            wordlist: Some(parsed.wordlist.as_str().to_string()),
            created_at: Some(123_456_789),
        };
        (identity, payload, password)
    }

    #[test]
    fn compact_backup_base64_hex_roundtrip() {
        let (identity, payload, password) = sample_identity();
        let compact = build_compact_backup(
            &identity,
            &payload,
            &password,
            Some("小明".to_string()),
            123_456_789,
        )
        .unwrap();

        // 紧凑结构体字段：v/kdf/rootId/nickname/createdAt/updatedAt
        assert_eq!(compact.v, COMPACT_BACKUP_VERSION);
        assert_eq!(compact.kdf, KDF_SCRYPT);
        assert_eq!(compact.root_id, identity.id());
        assert_eq!(compact.nickname.as_deref(), Some("小明"));
        assert_eq!(compact.created_at, 123_456_789);
        assert_eq!(compact.updated_at, 0);

        // 解码回磁盘 IdentityFile：base64 字段与 hex 字段同源字节一致
        let file = decode_compact_backup(&compact).unwrap();
        assert_eq!(file.version, FILE_VERSION_V2);
        assert_eq!(file.kdf, KDF_SCRYPT);
        assert_eq!(file.salt, hex::encode(B64.decode(&compact.salt).unwrap()));
        assert_eq!(file.iv, hex::encode(B64.decode(&compact.iv).unwrap()));
        assert_eq!(file.data, hex::encode(B64.decode(&compact.data).unwrap()));
        assert_eq!(
            file.auth_tag.as_deref(),
            Some(hex::encode(B64.decode(&compact.auth_tag).unwrap()).as_str())
        );
        assert_eq!(file.root_id, identity.id());
        assert_eq!(file.nickname.as_deref(), Some("小明"));
        assert_eq!(file.created_at, 123_456_789);
        assert_eq!(file.updated_at, 0);

        // 恢复端以派生公钥补全磁盘必填 publicKeyHex（权威来源）
        let mut file = file;
        file.public_key_hex = identity.public_key_hex();
        assert_eq!(file.public_key_hex, identity.public_key_hex());

        // 解出 payload 与原一致，rootId 校验通过
        let (decoded, decoded_identity) = unlock_identity(&file, &password).unwrap();
        assert_eq!(decoded.mnemonic, payload.mnemonic);
        assert_eq!(decoded.path, payload.path);
        assert_eq!(decoded_identity.id(), identity.id());
    }

    #[test]
    fn compact_backup_serialize_shape() {
        let (identity, payload, password) = sample_identity();
        let compact = build_compact_backup(&identity, &payload, &password, None, 1).unwrap();
        let json = serde_json::to_value(&compact).unwrap();
        let obj = json.as_object().unwrap();
        // 顶层字段名与设计 §4.2 一致，不含 publicKeyHex/version/pk
        assert!(obj.contains_key("v"));
        assert!(obj.contains_key("kdf"));
        assert!(obj.contains_key("salt"));
        assert!(obj.contains_key("iv"));
        assert!(obj.contains_key("data"));
        assert!(obj.contains_key("authTag"));
        assert!(obj.contains_key("rootId"));
        assert!(obj.contains_key("createdAt"));
        assert!(obj.contains_key("updatedAt"));
        assert!(!obj.contains_key("publicKeyHex"), "不得含 publicKeyHex");
        assert!(!obj.contains_key("version"), "内层版本由 v 承担");
        assert!(!obj.contains_key("pk"), "pk 已砍掉");
        assert!(!obj.contains_key("nickname"), "None 不序列化");
    }

    #[test]
    fn decode_compact_backup_fails_closed() {
        let (identity, payload, password) = sample_identity();
        let compact = build_compact_backup(&identity, &payload, &password, None, 1).unwrap();

        // 非法 base64 → 失败
        let mut bad_salt = serde_json::to_value(&compact).unwrap();
        bad_salt["salt"] = serde_json::Value::String("!!!not-base64!!!".into());
        let parsed: CompactBackupFile = serde_json::from_value(bad_salt).unwrap();
        assert!(
            decode_compact_backup(&parsed).is_err(),
            "非法 base64 必须 fail-closed"
        );

        // 版本不符 → 失败
        let mut bad_v = serde_json::to_value(&compact).unwrap();
        bad_v["v"] = serde_json::Value::from(1u32);
        let parsed: CompactBackupFile = serde_json::from_value(bad_v).unwrap();
        assert!(
            decode_compact_backup(&parsed).is_err(),
            "版本不符必须 fail-closed"
        );

        // kdf 不符 → 失败
        let mut bad_kdf = serde_json::to_value(&compact).unwrap();
        bad_kdf["kdf"] = serde_json::Value::String("pbkdf2".into());
        let parsed: CompactBackupFile = serde_json::from_value(bad_kdf).unwrap();
        assert!(
            decode_compact_backup(&parsed).is_err(),
            "kdf 不符必须 fail-closed"
        );
    }

    #[test]
    fn decode_compact_backup_rejects_wrong_field_lengths() {
        // QR 扫码位翻转损坏 salt/iv/authTag 会改变其长度（base64 解码成功但字节数
        // 不符）——必须统一 fail-closed 为 MalformedFile，避免在解密/派 KDF 阶段
        // 误报「密码不正确」/Crypto。
        let (identity, payload, password) = sample_identity();
        let compact = build_compact_backup(&identity, &payload, &password, None, 1).unwrap();

        let mut patch = |field: &str, b64: &str| -> CompactBackupFile {
            let mut v = serde_json::to_value(&compact).unwrap();
            v[field] = serde_json::Value::String(b64.to_string());
            serde_json::from_value::<CompactBackupFile>(v).unwrap()
        };
        // 合法 base64 但字节数不符（15B / 11B / 15B 而非 16/12/16）。
        let bad_salt = patch("salt", &B64.encode([0u8; 15]));
        assert!(
            decode_compact_backup(&bad_salt).is_err(),
            "salt 长度不符必须 fail-closed"
        );
        let bad_iv = patch("iv", &B64.encode([0u8; 11]));
        assert!(
            decode_compact_backup(&bad_iv).is_err(),
            "iv 长度不符必须 fail-closed"
        );
        let bad_tag = patch("authTag", &B64.encode([0u8; 15]));
        assert!(
            decode_compact_backup(&bad_tag).is_err(),
            "authTag 长度不符必须 fail-closed"
        );
        // 正常长度仍通过
        assert!(
            decode_compact_backup(&compact).is_ok(),
            "正常字段长度应通过"
        );
    }
}
