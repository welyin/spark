//! 组织邀请记录（DM 邀约流程的持久化状态）。
//!
//! 管理员经 DM 向目标用户发出组织邀请（`org-invite` 信封），双方各落一条
//! 记录：邀请方出站（`org:inv:out:{orgId}:{peerRootId}`）、受邀方入站
//! （`org:inv:in:{orgId}:{peerRootId}`）。同一对 `(orgId, peer)` 只留一条，
//! 重复邀请/投递原地更新（幂等）；状态机 `pending → accepted | declined`，
//! 终态不可逆（重复回执/重放入站不重置）。
//!
//! 入站记录保存 `inviteCode`（[`super::invite::encode_org_invite`] 产物），
//! 供重启后仍能走 `accept_invite` 编排；出站记录不存邀请码（邀请码可随时
//! 重新生成）。
//!
//! CRUD 服务函数见 [`super::service::OrganizationService`]（`invite_records`
//! 子模块）；本文件只放记录类型与存储键构造。

use serde::{Deserialize, Serialize};

/// 出站邀请记录键前缀（`org:inv:out:{orgId}:{peerRootId}`）。
pub(crate) const ORG_INV_OUT_PREFIX: &str = "org:inv:out:";
/// 入站邀请记录键前缀（`org:inv:in:{orgId}:{peerRootId}`）。
pub(crate) const ORG_INV_IN_PREFIX: &str = "org:inv:in:";

/// 邀请方向（相对本机）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgInviteDirection {
    /// 我发出的邀请（我是邀请方/管理员）。
    Outgoing,
    /// 我收到的邀请（我是被邀请人）。
    Incoming,
}

/// 邀请状态（终态不可逆）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrgInviteStatus {
    /// 待回应。
    Pending,
    /// 被邀请人已接受（加入完成）。
    Accepted,
    /// 被邀请人已拒绝。
    Declined,
}

/// 组织邀请记录（serde camelCase，壳层/前端直接消费）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgInviteRecord {
    /// 邀请 id（`inv-{ms}-{count}` 风格；入站记录沿用信封 `inviteId`，
    /// 回执按它对账）。
    pub id: String,
    /// 组织 id。
    pub org_id: String,
    /// 组织名（展示用快照；出站取本地记录，入站为邀请人自报）。
    #[serde(default)]
    pub org_name: String,
    /// 组织 logo（data URL；可省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_avatar: Option<String>,
    /// 对端 rootId（出站 = 被邀请人；入站 = 邀请人）。
    pub peer_root_id: String,
    /// 对端昵称（展示用；入站为邀请人自报，回执后可被刷新）。
    #[serde(default)]
    pub peer_nickname: String,
    /// 方向。
    pub direction: OrgInviteDirection,
    /// 状态。
    pub status: OrgInviteStatus,
    /// 邀请码（仅入站记录保存，供重启后仍能 accept）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
    /// 创建时间（ms；首次落库时间，原地更新保留）。
    pub created_at: i64,
    /// 最近更新时间（ms）。
    pub updated_at: i64,
}

/// 出站邀请记录键。
pub(crate) fn org_invite_out_key(org_id: &str, peer_root_id: &str) -> String {
    format!("{ORG_INV_OUT_PREFIX}{org_id}:{peer_root_id}")
}

/// 入站邀请记录键。
pub(crate) fn org_invite_in_key(org_id: &str, peer_root_id: &str) -> String {
    format!("{ORG_INV_IN_PREFIX}{org_id}:{peer_root_id}")
}
