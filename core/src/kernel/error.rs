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

    /// 通讯录模块错误。
    #[error(transparent)]
    Contact(#[from] crate::contact::ContactError),

    /// 消息模块错误。
    #[error(transparent)]
    Message(#[from] crate::message::MessageError),

    /// 插件后台运行时模块错误。
    #[error(transparent)]
    Plugin(#[from] crate::plugin::PluginError),

    /// 插件声明式数据模块错误。
    #[error(transparent)]
    Plugindata(#[from] crate::plugindata::PlugindataError),

    /// 内容面模块错误。
    #[error(transparent)]
    Content(#[from] crate::content::ContentError),

    /// 同步层错误（blob 层健康度/配额等 kernel 门面）。
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),

    /// 文件 IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Epoch 密钥轮换错误。
    #[error("epoch error: {0}")]
    Epoch(#[from] crate::epoch::EpochError),

    /// 口令校验器错误。
    #[error(transparent)]
    Pw(#[from] crate::pw::PwError),

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

    /// 候选口令与已发布 V 不匹配（`root_verify_password_ticket` / `root_unify_password`）。
    #[error("Ticket mismatch")]
    TicketMismatch,

    /// 本地没有可用 V（`root_verify_password_ticket` / `root_unify_password`）。
    #[error("Ticket unavailable")]
    TicketUnavailable,

    /// 密码长度不足（TS `Password must be at least 8 characters`）。
    #[error("Password must be at least 8 characters")]
    PasswordTooShort,

    /// orgq 写入被数据账号侧拒绝（denied：filtered 插件未运行 fail-closed
    /// 不可受理；O3 写路径映射）。
    #[error("Access denied")]
    AccessDenied,

    /// M5 延迟恢复通道专用错误（文案与前端映射一致）。
    #[error("TooEarly")]
    TooEarly,
    #[error("RecoveryVetoed")]
    RecoveryVetoed,
    #[error("RecoveryPending")]
    RecoveryPending,
    #[error("RecoveryNotFound")]
    RecoveryNotFound,
    #[error("VetoWindowExpired")]
    VetoWindowExpired,
    #[error("UnsupportedOp")]
    UnsupportedOp,
    #[error("Invalid input")]
    InvalidInput,
    /// 调用级限流（social-feed §9.2 `RateLimited`；每 (space, pluginId) 60s 内
    /// 10 次 feed.deliver）。对齐前端抛出的文案 `RateLimited`。
    #[error("RateLimited")]
    RateLimited,

    /// 其他流程错误（消息文本与 TS 对应分支一致）。
    #[error("{0}")]
    Internal(String),
}

/// kernel 门面 Result 别名。
pub type Result<T> = std::result::Result<T, KernelError>;
