//! 插件后台运行时模块错误。

/// 插件后台运行时错误。
///
/// 面向用户的文案逐字稳定（壳层命令错误以 Display 透传）。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// 插件 id 不合规（白名单：小写字母/数字/连字符，首字符非连字符，最长 64）。
    #[error("Invalid plugin id: {0}")]
    InvalidId(String),

    /// 同一插件的后台运行时重复启动。
    #[error("Plugin background already running: {0}")]
    AlreadyRunning(String),

    /// 插件脚本加载或执行失败（JS 异常/超时中断）。
    #[error("Plugin script error: {0}")]
    Script(String),

    /// host 调用非法（未知 capability 或载荷缺字段/类型不符）。
    #[error("invalid plugin host call: {0}")]
    InvalidCall(String),

    /// capability 所需权限未授予（文案前缀对齐桥 dispatcher 的 Access denied）。
    #[error("Access denied: {0}")]
    PermissionDenied(String),

    /// 业务输入非法（用户可见文案与对应 kernel 门面分支逐字一致）。
    #[error("{0}")]
    InvalidInput(String),

    /// 会话不属于该插件（bot rootId 前缀不匹配）。
    #[error("plugin does not own conversation: {0}")]
    ConversationNotOwned(String),

    /// 存储未打开（无活动身份，尚未对齐任何数据库目录）。
    #[error("storage is not open: no active identity")]
    StorageNotReady,

    /// 消息模块错误。
    #[error(transparent)]
    Message(#[from] crate::message::MessageError),

    /// 集合模块错误（插件 docs 能力）。
    #[error(transparent)]
    Collection(#[from] crate::collection::CollectionError),

    /// 集合 schema 模块错误（插件 docs.defineCollection 能力）。
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),

    /// 通讯录模块错误。
    #[error(transparent)]
    Contact(#[from] crate::contact::ContactError),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 插件运行时模块 Result 别名。
pub type Result<T> = std::result::Result<T, PluginError>;
