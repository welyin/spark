//! 组织视图（`toView`）：记录 + 当前用户角色/计数。

use serde::Serialize;
use serde_json::Value;

use super::member::{OrganizationMember, OrganizationRole};
use super::record::{OrganizationSyncState, is_false};

/// 组织视图（`toView`，service.ts:573-587）：记录 + 当前用户角色/计数。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrganizationView {
    /// 排好序的成员列表（admin 优先）。
    pub members: Vec<OrganizationMember>,
    /// 当前用户角色（非成员为 `None`）。
    #[serde(rename = "currentUserRole")]
    pub current_user_role: Option<OrganizationRole>,
    /// 当前用户是否 admin。
    #[serde(rename = "isCurrentUserAdmin")]
    pub is_current_user_admin: bool,
    /// 成员总数。
    #[serde(rename = "memberCount")]
    pub member_count: usize,
    /// admin 总数。
    #[serde(rename = "adminCount")]
    pub admin_count: usize,
    /// 底层记录（`basePluginDomain` 缺省归一为 `""`，对齐 TS toView）。
    #[serde(flatten)]
    pub record: OrganizationRecordFlattened,
}

/// `OrganizationView` 的记录部分（`basePluginDomain` 归一为非可选字符串）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrganizationRecordFlattened {
    /// `org_` + 16 hex。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 组织名。
    pub name: String,
    /// 描述。
    pub description: String,
    /// 基础插件域（缺省 `""`）。
    #[serde(rename = "basePluginDomain")]
    pub base_plugin_domain: String,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// 创建者 rootId。
    #[serde(rename = "createdBy")]
    pub created_by: String,
    /// 最近更新时间（ms）。
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    /// 同步状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<OrganizationSyncState>,
    /// 组织网关 rootId 列表（org.md §14）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gateways: Vec<String>,
    /// 自认证组织地址（org.md §15）。
    #[serde(
        rename = "orgAddress",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_address: Option<String>,
    /// 公开组织标志（org.md §16）。
    #[serde(rename = "isPublic", default, skip_serializing_if = "is_false")]
    pub is_public: bool,
    /// 动态字段。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
