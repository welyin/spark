//! 通讯录模块：朋友关系、好友申请、标签与分组（个人扁平/组织树形）的纯逻辑存储。
//!
//! 对齐设计文档 wiki/design/ui/ui-contacts.md §4（好友申请与双向确认）、§5（联系人
//! 资料字段）、§6（朋友权限 open/chatOnly）、§7（拉黑）、§8（标签与分组管理）；
//! 各变更函数语义逐一对齐前端 mock（app/src/mock/contacts.ts）的同名函数。
//!
//! 本模块为纯逻辑层：只操作 [`crate::storage::StorageBackend`]，不触碰网络。申请
//! 投递与双向确认的网络流程属 p2p 模块职责，本层只负责本地落库与状态机。
//!
//! 空间模型：`space` 为 `'personal'`（个人空间：朋友/申请/标签/扁平分组）或
//! `'org:<orgId>'`（组织空间：成员附加资料/标签/分组树 + 我发出的邀请记录；
//! 组织「新的成员」入站申请不入库，真实流程走邀请码）。存储已是「每身份一个
//! sled 库」，键不再带身份前缀。
//!
//! 存储键（值均为紧凑 JSON，serde camelCase）：
//! - `ct:friend:{rootId}` → [`FriendRecord`]（个人空间）
//! - `ct:req:in:{fromRootId}:{id}` / `ct:req:out:{id}` → [`FriendRequestRecord`]
//!   （个人空间；入站申请 id 带发送者命名空间防跨发送者撞 id）
//! - `ct:tags` → `Vec<ContactTag>`；`ct:groups` → `Vec<ContactGroup>`（数组序即显示序）
//! - `ct:blocked:{rootId}` → `"1"`（个人空间拉黑集合，独立于朋友记录）
//! - `ct:org:{orgId}:extra:{rootId}` → [`ContactProfileRecord`]（组织成员附加资料）
//! - `ct:org:{orgId}:tags` → 标签数组；`ct:org:{orgId}:tree` → `Vec<OrgGroupNode>`
//! - `ct:org:{orgId}:req:out:{id}` → [`FriendRequestRecord`]（组织空间我发出的
//!   邀请记录；邀请人本机数据，不随组织快照同步）
//!
//! 时间一律以 `now_ms` 参数注入，保证纯函数可测。[`FriendRequestRecord`] 带
//! `updatedAt`（新建 = createdAt，后续变更由写路径刷新）；其余记录尚无
//! updatedAt 字段，其变更方法保留 `now_ms` 形参（生成 id / 预留审计）。

mod service;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use service::ContactService;

/// 朋友记录键前缀（`ct:friend:{rootId}`）。
pub(crate) const FRIEND_PREFIX: &str = "ct:friend:";
/// 收到的好友申请键前缀（`ct:req:in:{fromRootId}:{requestId}`，复合 id：
/// 发送者 rootId 命名空间 + 原申请 id，防跨发送者撞 id 覆盖）。
pub(crate) const REQ_IN_PREFIX: &str = "ct:req:in:";
/// 发出的好友申请键前缀（`ct:req:out:{id}`）。
pub(crate) const REQ_OUT_PREFIX: &str = "ct:req:out:";

/// 个人空间标签数组键。
pub(crate) const TAGS_KEY: &str = "ct:tags";
/// 个人空间扁平分组数组键。
pub(crate) const GROUPS_KEY: &str = "ct:groups";

/// 个人空间拉黑集合键前缀（`ct:blocked:{rootId}`；独立于朋友记录——
/// 陌生人可拉黑，删除朋友不清拉黑）。
pub(crate) const BLOCKED_PREFIX: &str = "ct:blocked:";

/// 组织空间成员附加资料键前缀（`ct:org:{orgId}:extra:{rootId}`）。
pub(crate) fn org_extra_prefix(org_id: &str) -> String {
    format!("ct:org:{org_id}:extra:")
}

/// 组织空间我发出的邀请记录键前缀（`ct:org:{orgId}:req:out:{id}`）。
pub(crate) fn org_req_out_prefix(org_id: &str) -> String {
    format!("ct:org:{org_id}:req:out:")
}

/// 组织空间标签数组键。
pub(crate) fn org_tags_key(org_id: &str) -> String {
    format!("ct:org:{org_id}:tags")
}

/// 组织空间分组树键。
pub(crate) fn org_tree_key(org_id: &str) -> String {
    format!("ct:org:{org_id}:tree")
}

/// 对端网络引用（peerId + 地址），随朋友/申请记录落库。
///
/// 单一来源在 message 模块（会话寻址同型），此处再导出保持
/// `contact::PeerRef` 路径不变。
pub use crate::message::PeerRef;

/// 个人空间朋友记录（平铺：身份字段 + 本地资料字段，设计 §5）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRecord {
    pub root_id: String,
    #[serde(default)]
    pub nickname: String,
    /// 对方头像（data URL；无头像为 None，序列化时省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default)]
    pub signature: String,
    /// 对方性别（`"male"` / `"female"`；缺省不展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    pub added_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerRef>,
    /// 备注名（优先展示，仅自己可见）。
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    /// 所属分组 id；`""` = 未分组。
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub photos: Vec<String>,
    /// 朋友权限（`"open"` / `"chatOnly"`，设计 §6）。
    #[serde(default = "default_permission")]
    pub permission: String,
    #[serde(default)]
    pub blocked: bool,
}

/// 好友申请状态（设计 §4：pending → accepted / ignored）。
///
/// `Replied` 仅 outbox（我发出的申请）使用：对方回复询问（friend-reply
/// 信封），等待我回复；我回复后状态回 Pending。
/// `Failed` 仅 outbox 使用：投递无应答/失败时由投递任务置位，
/// 前端可据此展示「发送失败」并以同 id 重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FriendRequestStatus {
    Pending,
    Accepted,
    Ignored,
    Replied,
    Failed,
}

/// 申请回复线程消息方向（我 / 对方）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadFrom {
    Me,
    Peer,
}

/// 好友申请回复线程消息（来回询问/回答，设计 §4 的「回复询问」）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestThreadMessage {
    pub from: ThreadFrom,
    pub text: String,
    pub ts: i64,
}

/// 好友申请记录（`ct:req:in:` 收到 / `ct:req:out:` 发出）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequestRecord {
    pub id: String,
    pub root_id: String,
    #[serde(default)]
    pub nickname: String,
    /// 申请人头像（data URL；无头像为 None，序列化时省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default)]
    pub message: String,
    /// 来源展示文案（如「RootID 搜索」「扫码」「邀请码」）。
    #[serde(default)]
    pub source: String,
    pub status: FriendRequestStatus,
    pub created_at: i64,
    /// 最近变更时间（内容更新/状态流转；前端按 updatedAt 倒序）。
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerRef>,
    /// 来回回复线程（对方询问/我方回答；default 兼容旧数据无此字段）。
    #[serde(default)]
    pub thread: Vec<RequestThreadMessage>,
    /// 组织邀请码（仅组织空间我发出的邀请记录；camelCase `inviteCode`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
}

/// 组织成员本地附加资料（仅自己可见，设计 §5.4）。
///
/// `permission` 字段组织空间不使用，但与个人资料同形保留。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactProfileRecord {
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    /// 所属分组：组织空间为分组树节点 id；`""` = 未分组。
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub photos: Vec<String>,
    #[serde(default = "default_permission")]
    pub permission: String,
    #[serde(default)]
    pub blocked: bool,
}

impl Default for ContactProfileRecord {
    fn default() -> Self {
        Self {
            remark: String::new(),
            phones: Vec::new(),
            tag_ids: Vec::new(),
            group_id: String::new(),
            memo: String::new(),
            photos: Vec::new(),
            permission: default_permission(),
            blocked: false,
        }
    }
}

/// 通讯录标签（设计 §8）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactTag {
    pub id: String,
    pub name: String,
}

/// 个人空间扁平分组（数组顺序即显示顺序；「未分组」为虚拟组不入列）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactGroup {
    pub id: String,
    pub name: String,
}

/// 组织空间分组树节点（children 数组顺序即同级排序）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgGroupNode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub children: Vec<OrgGroupNode>,
}

/// `update_profile` 的资料补丁：`None` 字段保持不变（对齐 TS `Object.assign(profile, patch)`）。
///
/// serde camelCase（Tauri 命令入参直接反序列化）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePatch {
    pub remark: Option<String>,
    pub phones: Option<Vec<String>>,
    pub tag_ids: Option<Vec<String>>,
    pub group_id: Option<String>,
    pub memo: Option<String>,
    pub photos: Option<Vec<String>>,
    pub permission: Option<String>,
}

/// `overview` 返回的空间通讯录视图（对齐 TS `SpaceContacts`）：
/// 个人空间 `group_tree`/`member_extras` 为空；组织空间
/// `friends`/`requests`/`groups` 为空（`outgoing` 为我发出的邀请记录，
/// 对方凭码加入后由 org-pull-org 响应路径置 accepted）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceContactsView {
    pub friends: Vec<FriendRecord>,
    /// 收到的申请（「新的朋友」列表）。
    pub requests: Vec<FriendRequestRecord>,
    /// 我发出的申请（等待对方确认）。
    pub outgoing: Vec<FriendRequestRecord>,
    pub tags: Vec<ContactTag>,
    /// 个人空间扁平分组。
    pub groups: Vec<ContactGroup>,
    /// 组织空间分组树。
    pub group_tree: Vec<OrgGroupNode>,
    /// 组织空间：成员 rootId → 本地附加资料。
    pub member_extras: BTreeMap<String, ContactProfileRecord>,
}

fn default_permission() -> String {
    "open".to_string()
}

/// 通讯录模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum ContactError {
    /// 联系人不存在（个人空间对缺失 friend 打补丁/拉黑/分组时）。
    #[error("Contact not found")]
    ContactNotFound,

    /// 空间标识非法（既非 `personal` 也非 `org:<orgId>`），或组织空间操作传入了个人空间。
    #[error("Invalid contacts space")]
    InvalidSpace,

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 通讯录模块 Result 别名。
pub type Result<T> = std::result::Result<T, ContactError>;
