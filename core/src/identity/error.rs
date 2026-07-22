//! 身份模块错误类型。

/// 身份模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// 助记词无效（词表不匹配或校验和错误）。
    #[error("invalid mnemonic: {0}")]
    InvalidMnemonic(String),

    /// 派生路径无效。
    #[error("invalid derivation path: {0}")]
    InvalidPath(String),

    /// 解密失败（密码错误或密文被篡改）。
    #[error("decryption failed")]
    DecryptionFailed,

    /// 加密或 KDF 参数错误。
    #[error("crypto error: {0}")]
    Crypto(String),

    /// 昵称非法。
    #[error("invalid nickname: {0}")]
    InvalidNickname(String),

    /// 头像非法。
    #[error("invalid avatar: {0}")]
    InvalidAvatar(String),

    /// 不支持的身份文件版本。
    #[error("unsupported identity file version: {0}")]
    UnsupportedVersion(u32),

    /// 身份文件字段缺失或格式错误。
    #[error("malformed identity file: {0}")]
    MalformedFile(String),

    /// hex 解码错误。
    #[error("hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 身份模块 Result 别名。
pub type Result<T> = std::result::Result<T, IdentityError>;
