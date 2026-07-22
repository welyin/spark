//! kernel 门面统一错误。

/// kernel 门面统一错误。
///
/// 面向用户的文案与 TS 抛出的 `Error.message` 逐字一致（identity/org/data-mgmt
/// 各模块错误原样透传；门面自身的流程错误见后几个变体）。
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// 身份模块错误。
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),

    /// 集合模块错误。
    #[error(transparent)]
    Collection(#[from] crate::collection::CollectionError),

    /// 组织模块错误。
    #[error(transparent)]
    Org(#[from] crate::org::OrgError),

    /// 数据治理模块错误。
    #[error(transparent)]
    DataMgmt(#[from] crate::data_mgmt::DataMgmtError),

    /// 存证模块错误。
    #[error(transparent)]
    Evidence(#[from] crate::evidence::EvidenceError),

    /// schema 模块错误。
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),

    /// p2p 模块错误。
    #[error(transparent)]
    P2p(#[from] crate::p2p::P2pError),

    /// 文件 IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// 身份已锁定（需要解锁的操作）。
    #[error("Root identity is locked")]
    Locked,

    /// 身份未初始化（无任何身份或活动指针）。
    #[error("Root identity is not initialized")]
    NotInitialized,

    /// 存储未打开（无活动身份，尚未对齐任何数据库目录）。
    #[error("storage is not open: no active identity")]
    StorageNotReady,

    /// 密码错误（unlock / reveal_mnemonic / update_profile 解密失败；TS `Invalid password`）。
    #[error("Invalid password")]
    InvalidPassword,

    /// 密码长度不足（TS `Password must be at least 8 characters`）。
    #[error("Password must be at least 8 characters")]
    PasswordTooShort,

    /// 其他流程错误（消息文本与 TS 对应分支一致）。
    #[error("{0}")]
    Message(String),
}

/// kernel 门面 Result 别名。
pub type Result<T> = std::result::Result<T, KernelError>;
