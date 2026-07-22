//! 组织模块：邀请码、组织记录、nodeInfoClaim、同步快照合并、org-sync-state
//! 记账、K 副本统计、recovery token、pluginDocs 随组织同步、本地事务审计。
//!
//! 算法精确规格见 `core/spec/org.md`（网络消息格式见 `core/spec/p2p-messages.md`），
//! 验收向量见 `core/spec/vectors/org.json`。
//!
//! 本模块为纯逻辑层：不涉及网络传输（属 p2p 模块），服务层
//! （[`service::OrganizationService`]）只操作 [`crate::storage::StorageBackend`]。
//!
//! ## 有意修复（非逐 bug 对齐，见 org.md §14.3 / p2p-messages.md §13.2）
//!
//! TS 实现中 org-share 推送路径把 `{versions, sections, lastSyncedAt}` 外壳对象当作
//! versions 写入 org-sync-state（org-share-sync.ts:393,439,464），导致：
//! 1. 推送前 stale 检查的 incoming 四字段全为 undefined → 恒判不 stale →
//!    **存在历史 sync-state 后对该 peer 的推送恒被跳过**；
//! 2. K 副本统计 `coversCurrent` 对污染记录恒为 true → 成员永久计入 everSynced。
//!
//! Rust 内核按**正确语义**实现：三个写入时机一律写规范 versions 形状
//! （见 [`sync_state`]），stale 检查严格按四版本字段比较，coversCurrent 按
//! 「sync-state 版本不落后于当前组织版本」判定。读取侧对 TS 遗留的污染形状做
//! 兼容解包（[`sync_state::OrgSyncState`] 的反序列化），不会把 bug 传播回来。

pub mod claim;
pub mod invite;
pub mod plugin_docs;
pub mod pull;
pub mod recovery;
pub mod replica;
pub mod service;
pub mod snapshot;
pub mod sync_state;
pub mod tx;
pub mod types;

pub use claim::{
    ClaimVerification, NODE_INFO_CLAIM_MAX_AGE_MS, NodeInfoClaim, NodeInfoClaimUnsigned,
    build_node_info_claim_payload, sign_node_info_claim, verify_node_info_claim,
};
pub use invite::{
    ORG_INVITE_MAX_AGE_MS, OrgInviteError, OrgInviteInviter, OrgInvitePayload, decode_org_invite,
    decode_org_invite_at, encode_org_invite,
};
pub use plugin_docs::{
    PLUGIN_DOC_PREFIX, PluginDocSyncItem, apply_plugin_doc_sync_items,
    collect_syncable_plugin_docs, is_sync_disabled, parse_plugin_doc_key, resolve_org_id,
};
pub use pull::{
    PullOrgOutcome, classify_pull_org_response, handle_pull_list_request,
    handle_pull_org_request, member_auth_status, parse_pull_list_organizations,
    resolve_local_versions, validate_incoming_share_payload,
};
pub use recovery::{
    RECOVERY_TIME_BUCKET_MS, RecoveryViewItem, active_recovery_tokens, recovery_time_bucket,
    recovery_token,
};
pub use replica::{
    MemberSyncOverview, ORG_REPLICA_FRESH_WINDOW_MS, ORG_REPLICA_TARGET, OrgSyncOverview,
    compute_org_sync_overview, covers_current, member_ever_synced, replica_sufficient,
};
pub use service::OrganizationService;
pub use snapshot::{
    ORGANIZATION_SYNC_RESERVED_KEYS, OrganizationSyncSnapshot, OrganizationSyncSummary,
    SnapshotMember, build_organization_sync_snapshot, build_organization_sync_versions,
    build_organization_sync_versions_default, is_organization_sync_stale,
    merge_organization_sync_snapshot, normalize_incoming_snapshot, pick_sync_sections_by_priority,
};
pub use sync_state::{
    ORG_SYNC_STATE_MAX_AGE_MS, ORG_SYNC_STATE_PREFIX, OrgSyncState, is_org_sync_state_expired,
    org_sync_state_key, should_skip_share_push, sync_state_after_pull_synced,
    sync_state_after_share_acked, sync_state_after_share_delivered,
};
pub use tx::{
    ORG_TX_PREFIX, OrganizationTransactionRecord, OrganizationTransactionType,
    append_organization_transaction, get_latest_organization_transaction_version,
    list_organization_transactions, organization_transaction_key,
};
pub use types::{
    ORG_META_PREFIX, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
    OrganizationRole, OrganizationSyncSection, OrganizationSyncState, OrganizationSyncVersions,
    OrganizationView, generate_organization_id, generate_recovery_secret, is_valid_root_id,
    normalize_node_info, normalize_optional_node_info, normalize_plugin_domain,
    normalize_root_id, normalize_text, organization_key, sort_members,
};

/// 组织模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    /// 邀请码错误（消息与 TS 一致，面向用户可读）。
    #[error("{0}")]
    Invite(#[from] OrgInviteError),

    /// 必填文本字段为空（`{label} is required`）。
    #[error("{0} is required")]
    Required(String),

    /// 基础插件域非法（未以 `plugin:` 开头或只有前缀）。
    #[error("Invalid base plugin domain")]
    InvalidBasePluginDomain,

    /// 成员 rootId 非法。
    #[error("Invalid member rootId")]
    InvalidMemberRootId,

    /// 节点信息为空（peerId 与 addresses 至少其一）。
    #[error("Member node info is required: provide peerId or at least one address")]
    NodeInfoRequired,

    /// peerId 非法（trim 后不足 8 字符）。
    #[error("Invalid peerId")]
    InvalidPeerId,

    /// 组织不存在。
    #[error("Organization not found")]
    OrganizationNotFound,

    /// 成员不存在。
    #[error("Member not found")]
    MemberNotFound,

    /// 需要组织管理员权限。
    #[error("Organization admin required")]
    AdminRequired,

    /// 组织必须保留至少一名管理员。
    #[error("Organization must keep at least one admin")]
    MustKeepAdmin,

    /// 不能接受自己发出的邀请码（service.ts:349-351）。
    #[error("不能接受自己发出的邀请码")]
    SelfInvite,

    /// 生成邀请码时本机无任何可用节点地址（service.ts:326-328）。
    #[error("本机 P2P 节点尚未启动，请先启动网络后再生成邀请码")]
    NetworkUnavailable,

    /// 邀请拉取完成后本地仍无成员记录（service.ts:369-371）。
    #[error("未能加入组织：请确认管理员已先将你的 RootID 录入组织成员")]
    NotJoined,

    /// 快照/记录形状非法。
    #[error("malformed organization data: {0}")]
    Malformed(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 组织模块 Result 别名。
pub type Result<T> = std::result::Result<T, OrgError>;
