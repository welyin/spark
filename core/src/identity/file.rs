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
    /// 昵称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 头像 data URL。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// 性别（扩展字段；缺省字段按 `None` 反序列化，旧文件向后兼容）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// 地区（扩展字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 个性签名（扩展字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
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
        nickname: Some(nickname.clone()),
        avatar: avatar.map(str::to_string),
        gender: None,
        region: None,
        signature: None,
        created_at: Some(now),
    };
    let file = seal_v2(
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
    Ok((file, identity))
}

/// 解锁身份文件（v2 / v1 均可），返回 `(payload, 按 payload.path 派生的身份)`。
pub fn unlock_identity(file: &IdentityFile, password: &str) -> Result<(IdentityPayload, Identity)> {
    let payload = decrypt_payload(file, password)?;
    let parsed = parse_mnemonic(&payload.mnemonic)?;
    let identity = derive_identity_at_path(&parsed.seed, &payload.path)?;
    Ok((payload, identity))
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
    let (nickname, avatar) = sanitize_profile(
        file.nickname.as_deref().or(payload.nickname.as_deref()),
        file.avatar.as_deref().or(payload.avatar.as_deref()),
    );
    let now = now_ms();
    let new_payload = IdentityPayload {
        mnemonic: payload.mnemonic,
        path: payload.path,
        version: FILE_VERSION_V2,
        wordlist: payload
            .wordlist
            .or_else(|| Some(Wordlist::English.as_str().to_string())),
        nickname: nickname.clone(),
        avatar: avatar.clone(),
        // v1 时代无扩展字段，解密结果中必然为 None，原样保留
        gender: payload.gender,
        region: payload.region,
        signature: payload.signature,
        created_at: Some(file.created_at),
    };
    seal_v2(
        &new_payload,
        password,
        file.public_key_hex.clone(),
        file.root_id.clone(),
        nickname,
        avatar,
        new_payload.gender.clone(),
        new_payload.region.clone(),
        new_payload.signature.clone(),
        file.created_at,
        now,
    )
}

/// 更新资料（昵称/头像 + 扩展字段性别/地区/签名）：payload 重新加密，文件层
/// 字段同步，updatedAt 刷新。
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
    if file.version != FILE_VERSION_V2 {
        return Err(IdentityError::UnsupportedVersion(file.version));
    }
    let mut payload = decrypt_payload(file, password)?;

    if let Some(n) = nickname {
        payload.nickname = Some(validate_nickname(n)?);
    }
    if let Some(a) = avatar {
        payload.avatar = match a {
            Some(a) => {
                validate_avatar(a)?;
                Some(a.to_string())
            }
            None => None,
        };
    }
    payload.gender = patch_extra_field(payload.gender, gender, "gender", GENDER_MAX_CHARS)?;
    payload.region = patch_extra_field(payload.region, region, "region", REGION_MAX_CHARS)?;
    payload.signature =
        patch_extra_field(payload.signature, signature, "signature", SIGNATURE_MAX_CHARS)?;

    let updated = seal_v2(
        &payload,
        password,
        file.public_key_hex.clone(),
        file.root_id.clone(),
        payload.nickname.clone(),
        payload.avatar.clone(),
        payload.gender.clone(),
        payload.region.clone(),
        payload.signature.clone(),
        file.created_at,
        now_ms(),
    )?;
    *file = updated;
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
    let salt = hex::decode(&file.salt)?;
    let iv = hex::decode(&file.iv)?;
    let data = hex::decode(&file.data)?;
    let plaintext = match file.version {
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
            crypto::decrypt_v2(&data, &auth_tag, password, &salt, &iv)?
        }
        FILE_VERSION_V1 => {
            if file.kdf != KDF_PBKDF2 {
                return Err(IdentityError::MalformedFile(format!(
                    "v1 file with kdf `{}`",
                    file.kdf
                )));
            }
            crypto::decrypt_v1(&data, password, &salt, &iv)?
        }
        other => return Err(IdentityError::UnsupportedVersion(other)),
    };
    Ok(serde_json::from_slice(&plaintext)?)
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
    let mut salt = [0u8; 16];
    let mut iv = [0u8; crypto::GCM_IV_LEN];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut iv);
    let plaintext = serde_json::to_vec(payload)?;
    let (data, auth_tag) = crypto::encrypt_v2(&plaintext, password, &salt, &iv)?;
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
