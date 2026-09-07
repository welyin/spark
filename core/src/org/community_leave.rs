//! 组织退出共同体的留史记录（community-model「退出留史」+ org-genesis §3.2）：
//! 退出 = 名册移除（成员条目墓碑化）+ **追加留史记录**——历史内容原样保留，
//! 成员关系的历史不抹除。
//!
//! 键：`org:cleave:{communityOrgId}:{memberIdentity}:{leftAt}`（append-only，
//! 逐次退出一条；同一组织重新加入再退出产生新键，更早留史不被覆盖）。
//! 键域并入 org:structure@v1 内建集合（builtin.rs），随 orgsync 全员流动——
//! 各节点由「当前名册无 kind=org 成员 + 存在留史记录」**确定性推导**空域
//! 只读档案状态（见 [`crate::org::service::OrganizationService::is_community_archived`]），
//! 不依赖任何乱序敏感的易失标志。

use serde::{Deserialize, Serialize};

use super::types::OrgBinding;

/// 留史记录存储键前缀。
pub const ORG_COMMUNITY_LEAVE_PREFIX: &str = "org:cleave:";

/// 某共同体域的留史记录扫描前缀：`org:cleave:{communityOrgId}:`。
pub fn community_leave_prefix(community_org_id: &str) -> String {
    format!("{ORG_COMMUNITY_LEAVE_PREFIX}{community_org_id}:")
}

/// 留史记录键：`org:cleave:{communityOrgId}:{memberIdentity}:{leftAt}`。
/// （memberIdentity 为 64hex 域身份 id、leftAt 为 ms 时间戳，均不含冒号。）
pub fn community_leave_key(community_org_id: &str, member_identity: &str, left_at: i64) -> String {
    format!("{}{}:{}", community_leave_prefix(community_org_id), member_identity, left_at)
}

/// 组织退出共同体的留史记录（append-only，写后不删不改——重新加入再退出
/// 以新 `leftAt` 键追加新条）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityLeaveRecord {
    /// 线形版本，恒 1。
    #[serde(rename = "leaveV")]
    pub leave_v: u32,
    /// 共同体域 orgId。
    #[serde(rename = "communityOrgId")]
    pub community_org_id: String,
    /// 退出组织在本共同体的域身份 id（名册成员条目 rootId 槽位，64hex）。
    #[serde(rename = "memberIdentity")]
    pub member_identity: String,
    /// 退出时名册上的公开组织绑定快照（org-genesis §3.2 opt-in；未公开绑定
    /// 则丢键——留史不扩大披露面）。
    #[serde(
        rename = "orgBinding",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_binding: Option<OrgBinding>,
    /// 退出时间（ms，调用方注入）。
    #[serde(rename = "leftAt")]
    pub left_at: i64,
    /// 操作者个人 rootId（退出组织的在任管理员，代表组织行事）。
    #[serde(rename = "actorRootId")]
    pub actor_root_id: String,
}
