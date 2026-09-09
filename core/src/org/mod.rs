//! 组织模块：邀请码、组织记录、同步快照合并、org-sync-state
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

pub mod access_key;
pub mod community_invite;
pub mod community_leave;
pub mod gateway;
pub mod genesis;
pub mod invite;
pub mod invite_record;
pub mod join_request;
pub mod mailbox;
pub mod mailbox_store;
pub mod meta_merge;
pub mod node_card;
pub mod org_address;
pub mod plugin_docs;
pub mod recovery;
pub mod replica;
pub mod roles;
pub mod service;
pub mod sigset;
pub mod snapshot;
pub mod sync_state;
pub mod tx;
pub mod types;

pub use community_invite::{
    COMMUNITY_ORG_INVITE_TYPE, COMMUNITY_ORG_JOIN_NOTICE_TYPE, CommunityInviteError,
    CommunityOrgInvitePayload, build_community_invite_mail_body, build_community_join_notice,
    community_domain, decode_community_org_invite_at, encode_community_org_invite,
};
pub use community_leave::{
    CommunityLeaveRecord, ORG_COMMUNITY_LEAVE_PREFIX, community_leave_key, community_leave_prefix,
};
pub use gateway::{ORG_MEMBERS_DHT_KEY_SUFFIX, OrgMemberHint, org_members_dht_key};
pub use genesis::{
    BornOf, GenesisPolicyRecord, ORG_GENESIS_PREFIX, ORG_POLICY_PREFIX, OrgDomainIdentity,
    PolicyRevisionRecord, PolicyVersion, SigningPolicy, TransitionDecl, VetoThreshold,
    default_transition_decl, enforce_member_kind, genesis_org_id, genesis_policy_hash,
    genesis_sign_payload, is_genesis_form_org_id, membership_would_cycle, org_genesis_key,
    org_policy_key, policy_revision_hash, policy_revision_payload, sign_genesis_record,
    verify_genesis_signature, verify_org_address_binding,
};
pub use invite::{
    ORG_INVITE_MAX_AGE_MS, OrgInviteError, OrgInviteInviter, OrgInvitePayload, decode_org_invite,
    decode_org_invite_at, encode_org_invite,
};
pub use invite_record::{OrgInviteDirection, OrgInviteRecord, OrgInviteStatus};
/// A17 免预录凭证入册（membership §4.5，org-join §8）：加入声明线形与
/// 合入侧双路径（预录-认领 / 免预录凭证）合一验证。
pub use join_request::{
    JOIN_REQUEST_V, JoinAdmission, JoinPath, JoinRejection, JoinRequest, ORG_JOIN_REQUEST_TYPE,
    adjudicate_join_request, join_request_sign_payload, validate_join_request,
};
pub use meta_merge::{merge_member_record, merge_org_meta_record};
pub use node_card::{
    NODE_CARD_MAX_AGE_MS, NODE_CARD_TYPE, NodeCard, NodeCardReject, build_node_card_payload,
    decode_node_card, encode_node_card, make_node_card, parse_and_verify_node_card, sign_node_card,
    verify_node_card,
};
pub use org_address::{
    ORG_ADDRESS_CACHE_PREFIX, ORG_ADDRESS_FUTURE_TOLERANCE_MS, ORG_ADDRESS_GOSSIP_TYPE,
    ORG_ADDRESS_LEN, ORG_ADDRESS_RECORD_DEFAULT_TTL_MS, ORG_ADDRESS_RECORD_MAX_TTL_MS,
    OrgAddressRecord, OrgAddressRecordUnsigned, OrgAddressVerification,
    build_org_address_record_payload, cache_org_address_record, decode_org_address,
    generate_org_root_signing_key, is_newer_org_address_record, is_valid_org_address,
    open_org_root_secret, org_address_cache_key, org_address_dht_key, org_address_from_public_key,
    org_address_record_expired, org_root_signing_key, read_cached_org_address_record,
    seal_org_root_secret, search_cached_org_address_records, sign_org_address_record,
    strip_org_root_secret, verify_org_address_record,
};
pub use plugin_docs::{
    PLUGIN_DOC_PREFIX, collect_org_plugin_domains, is_sync_disabled, migrate_plugin_docs,
    parse_plugin_doc_key, resolve_org_id,
};
pub use recovery::{
    RECOVERY_TIME_BUCKET_MS, RecoveryViewItem, active_recovery_tokens, recovery_time_bucket,
    recovery_token,
};
pub use replica::{
    DutyObservation, MemberReplicaOverview, MemberSyncOverview, ORG_NETWORK_LOST_DEBOUNCE_MS,
    ORG_REPLICA_FRESH_WINDOW_MS, ORG_REPLICA_TARGET, OrgNetworkStatus, OrgNetworkStatusInput,
    OrgSyncOverview, compute_org_sync_overview, covers_current, decide_org_network_status,
    member_ever_synced, member_replicas_sufficient, replica_sufficient,
};
pub use service::OrganizationService;
pub use sigset::{
    DEGRADED_LEGACY_ORG_ID, OrgSigSetVerdict, OrgSigSetVerifyContext, SigSetReject,
    component_sign_payload, roster_member_set_hash,
};
pub use snapshot::{
    ORGANIZATION_SYNC_RESERVED_KEYS, OrganizationSyncSnapshot, OrganizationSyncSummary,
    SnapshotMember, build_organization_sync_snapshot, build_organization_sync_versions,
    build_organization_sync_versions_default, is_organization_sync_stale,
    merge_organization_sync_snapshot, normalize_incoming_snapshot, pick_sync_sections_by_priority,
    resolve_local_versions,
};
pub use sync_state::{
    ORG_SYNC_STATE_MAX_AGE_MS, ORG_SYNC_STATE_PREFIX, OrgSyncState, is_org_sync_state_expired,
    org_sync_state_key,
};
pub use tx::{
    ORG_TX_PREFIX, OrganizationTransactionRecord, OrganizationTransactionType,
    append_organization_transaction, get_latest_organization_transaction_version,
    list_organization_transactions, organization_transaction_key,
};
pub use types::{
    DomainType, MemberKind, ORG_META_PREFIX, OrganizationDeviceSet, OrganizationMember,
    OrganizationNodeInfo, OrganizationRecord, OrganizationRole, OrganizationSyncSection,
    OrganizationSyncState, OrganizationSyncVersions, OrganizationView, generate_org_secret,
    generate_organization_id, generate_recovery_secret, is_valid_org_id, is_valid_root_id,
    normalize_node_info, normalize_optional_node_info, normalize_plugin_domain, normalize_root_id,
    normalize_text, organization_key, sort_members,
};

/// 组织模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    /// 邀请码错误（消息与 TS 一致，面向用户可读）。
    #[error("{0}")]
    Invite(#[from] OrgInviteError),

    /// 共同体邀请码错误（组织加入共同体流，面向用户可读）。
    #[error("{0}")]
    CommunityInvite(#[from] CommunityInviteError),

    /// 必填文本字段为空（`{label} is required`）。
    #[error("{0} is required")]
    Required(String),

    /// 基础插件域非法（未以 `plugin:` 开头或只有前缀）。
    #[error("Invalid base plugin domain")]
    InvalidBasePluginDomain,

    /// 组织 logo 非法（须 `data:image/` 前缀且序列化后不超限；消息沿用
    /// `identity::validate_avatar` 的文案）。
    #[error("{0}")]
    InvalidAvatar(String),

    /// 成员 rootId 非法。
    #[error("Invalid member rootId")]
    InvalidMemberRootId,

    /// 成员种类与域类型不匹配（org-genesis §3.2 内核硬规则）：共同体域只接受
    /// 组织成员、叶组织只接受个人成员。文案逐字稳定（用户可见）。
    #[error("{0}")]
    MemberKindNotAllowed(String),

    /// 加入操作将成环（org-genesis §3.3 禁止成环）：待加入组织已出现在
    /// 目标域的可达祖先集中（含目标域本身）。
    #[error("Membership would create a cycle")]
    MembershipCycle,

    /// 节点信息为空（peerId 与 addresses 至少其一）。
    #[error("Member node info is required: provide peerId or at least one address")]
    NodeInfoRequired,

    /// peerId 非法（trim 后不足 8 字符）。
    #[error("Invalid peerId")]
    InvalidPeerId,

    /// 组织签名策略非法（org-genesis §1：m-of-n 须满足 1 ≤ m ≤ n，且 n 不超过
    /// 当前快照内 admin 数——创建时为唯一初始 admin 即 n ≤ 1）。
    #[error("Invalid signing policy")]
    InvalidSigningPolicy,

    /// 组织不存在。
    #[error("Organization not found")]
    OrganizationNotFound,

    /// 成员不存在。
    #[error("Member not found")]
    MemberNotFound,

    /// 需要组织管理员权限。
    #[error("Organization admin required")]
    AdminRequired,

    /// 域不可删除（A13 全域口径，community-model：域不可解散，只可退出——
    /// 全体成员退出后域成为空域，只读历史档案，无人能写入）。删除本地组织
    /// 记录会抹掉档案，故内核层面拒绝（全部域类型）。
    #[error("域只可退出，不可解散；历史保留为只读档案")]
    CommunityDomainNotDeletable,

    /// 共同体域已是空域只读档案（community-model：最后一个成员组织退出后
    /// 无人能写入）：组织记录更新/成员变更/邀请与加入等写路径一律拒绝，
    /// 读路径（历史查询）不受影响。状态由「名册无 kind=org 成员 + 存在
    /// `org:cleave:` 留史记录」确定性推导。
    #[error("共同体域已是空域只读档案（全员已退出，历史保留，无法写入）")]
    CommunityDomainArchived,

    /// 退出组织不是该共同体成员（成员条目缺失、非 kind=org 条目，或公开
    /// 绑定与退出组织不符）。
    #[error("该组织不是此共同体成员")]
    NotCommunityMember,

    /// 组织必须保留至少一名管理员。
    #[error("Organization must keep at least one admin")]
    MustKeepAdmin,

    /// 成员身份字段 / 组织 logo 非法（校验口径复用 identity 资料校验）。
    #[error("{0}")]
    InvalidIdentityField(String),

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

    /// 插件声明错误（内建集合注册失败，O2b）。
    #[error(transparent)]
    Plugindata(#[from] crate::plugindata::PlugindataError),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 组织模块 Result 别名。
pub type Result<T> = std::result::Result<T, OrgError>;
