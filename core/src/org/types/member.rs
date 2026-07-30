//! 成员角色、节点信息与成员记录（含 `sortMembers` 排序）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::{OrgError, Result};

/// 组织成员角色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrganizationRole {
    /// 管理员（创建者自动为唯一初始 admin）。
    Admin,
    /// 普通成员（addMember 新成员固定为该角色）。
    #[default]
    Member,
}

impl OrganizationRole {
    /// TS 字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

/// 成员节点信息（`{ peerId?, addresses }`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationNodeInfo {
    /// libp2p peerId（可省）。
    #[serde(rename = "peerId", default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// `normalizeNodeInfo`（service.ts:46-64）：peerId/addresses 各自 trim 滤空；
/// 两者皆空报错；peerId 非空但 < 8 字符报错。
pub fn normalize_node_info(node_info: &OrganizationNodeInfo) -> Result<OrganizationNodeInfo> {
    let peer_id = node_info
        .peer_id
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let addresses: Vec<String> = node_info
        .addresses
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();

    if peer_id.is_none() && addresses.is_empty() {
        return Err(OrgError::NodeInfoRequired);
    }
    if let Some(p) = &peer_id
        && p.len() < 8
    {
        return Err(OrgError::InvalidPeerId);
    }
    Ok(OrganizationNodeInfo { peer_id, addresses })
}

/// `normalizeOptionalNodeInfo`（service.ts:67-77）：未提供或全空视为 `None`
/// （成员地址可后续经 nodeInfoClaim 回填）。
pub fn normalize_optional_node_info(
    node_info: Option<&OrganizationNodeInfo>,
) -> Result<Option<OrganizationNodeInfo>> {
    let Some(info) = node_info else {
        return Ok(None);
    };
    let has_peer_id = info
        .peer_id
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    let has_addresses = info.addresses.iter().any(|a| !a.trim().is_empty());
    if !has_peer_id && !has_addresses {
        return Ok(None);
    }
    normalize_node_info(info).map(Some)
}

/// 组织成员。
///
/// `extra` 捕获 wire 上成员对象的非标准键（合并时随 existing 保留，对齐 TS 的
/// 对象展开语义 `{...existingMember, ...member}`）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationMember {
    /// 成员 rootId（64 hex 小写）。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 角色。
    pub role: OrganizationRole,
    /// 加入时间（ms）。
    #[serde(rename = "joinedAt")]
    pub joined_at: i64,
    /// 录入人 rootId。
    #[serde(rename = "addedBy")]
    pub added_by: String,
    /// 节点信息（可后续经 nodeInfoClaim 回填）。
    #[serde(rename = "nodeInfo", default, skip_serializing_if = "Option::is_none")]
    pub node_info: Option<OrganizationNodeInfo>,
    /// 组织内昵称（组织身份；全部身份字段仅本人可改，经快照 members 段传播）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 组织内头像（data URL）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// 个性签名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// 性别。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// 地区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 是否在组织内展示个人身份（`None` = 未设置，语义视同 false；
    /// `Some(false)` = 显式关闭——M1 墓碑语义：true→false 必须能经快照传播，
    /// 故升级为 `Option<bool>`；旧数据 true/false 读为 `Some`，缺键读为
    /// `None`（serde default 兼容）。
    #[serde(
        rename = "usePersonalIdentity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub use_personal_identity: Option<bool>,
    /// 非标准动态键。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `sortMembers`（service.ts:79-86）：admin 优先，其余按 joinedAt 升序。
///
/// 注意：TS `Array.prototype.sort` 稳定；Rust `sort_by` 同样稳定，逐键对齐。
pub fn sort_members(members: &[OrganizationMember]) -> Vec<OrganizationMember> {
    let mut sorted = members.to_vec();
    sorted.sort_by(|left, right| {
        if left.role != right.role {
            return if left.role == OrganizationRole::Admin {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        left.joined_at.cmp(&right.joined_at)
    });
    sorted
}
