//! 组织记录类型与归一化规则（对齐 desktop/src/main/organization/types.ts 与
//! service.ts 的 normalize* 辅助函数）。
//!
//! 存储：键 `org:meta:<orgId>`（[`ORG_META_PREFIX`]），值 = 记录 JSON。
//!
//! ## 动态字段（extra）
//!
//! TS 的 `OrganizationRecord` 允许携带任意额外键（`recoverySecret` 就是其一）：
//! 快照构建时保留键之外的字段全部流入 `summary.metadata`（见 snapshot.rs）。
//! Rust 侧以 `#[serde(flatten)] extra` 捕获这些动态键——`recoverySecret`
//! 因此**不是**具名字段，而是经 [`OrganizationRecord::recovery_secret`] /
//! [`OrganizationRecord::set_recovery_secret`] 访问的动态键，与 TS 行为逐键一致。

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{OrgError, Result};

/// 组织记录存储键前缀（organization/constants.ts:1）。
pub const ORG_META_PREFIX: &str = "org:meta:";

/// 组织记录存储键：`org:meta:<orgId>`。
pub fn organization_key(org_id: &str) -> String {
    format!("{ORG_META_PREFIX}{org_id}")
}

/// rootId 合法性：`trim().toLowerCase()` 后匹配 `^[0-9a-f]{64}$`。
pub fn is_valid_root_id(root_id: &str) -> bool {
    let normalized = root_id.trim().to_lowercase();
    normalized.len() == 64 && normalized.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// `normalizeRootId`：trim + lowercase + 格式校验。
pub fn normalize_root_id(root_id: &str) -> Result<String> {
    let normalized = root_id.trim().to_lowercase();
    if !is_valid_root_id(&normalized) {
        return Err(OrgError::InvalidMemberRootId);
    }
    Ok(normalized)
}

/// `normalizeText`：trim + 连续空白归一为单空格；空串报错（`{label} is required`）。
pub fn normalize_text(value: &str, label: &str) -> Result<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(OrgError::Required(label.to_string()));
    }
    Ok(normalized)
}

/// `normalizePluginDomain`：trim，须以 `plugin:` 开头且前缀后非空。
pub fn normalize_plugin_domain(value: &str) -> Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(OrgError::Required("Base plugin".to_string()));
    }
    if !normalized.starts_with("plugin:") || normalized.len() <= "plugin:".len() {
        return Err(OrgError::InvalidBasePluginDomain);
    }
    Ok(normalized.to_string())
}

/// 生成组织 id：`org_` + 8 随机字节 hex（16 hex，service.ts:88-90）。
pub fn generate_organization_id() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("org_{}", hex::encode(bytes))
}

/// 生成组织恢复盐：32 随机字节 hex（64 hex，service.ts:124）。
pub fn generate_recovery_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

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
    let has_peer_id = info.peer_id.as_deref().is_some_and(|p| !p.trim().is_empty());
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
    /// 非标准动态键。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// 同步区段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizationSyncSection {
    /// 概要。
    #[serde(rename = "summary")]
    Summary,
    /// 成员列表。
    #[serde(rename = "members")]
    Members,
    /// 成员详情（nodeInfo）。
    #[serde(rename = "member-details")]
    MemberDetails,
    /// 事务记录。
    #[serde(rename = "transactions")]
    Transactions,
}

/// 四字段同步版本（实际口径：全部等于 `record.updatedAt`，仅
/// `transactionsVersion` 可独立取最近事务 createdAt，sync.ts:50-57）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationSyncVersions {
    /// 概要版本。
    #[serde(rename = "summaryVersion")]
    pub summary_version: i64,
    /// 成员列表版本。
    #[serde(rename = "membersVersion")]
    pub members_version: i64,
    /// 成员详情版本。
    #[serde(rename = "memberDetailsVersion")]
    pub member_details_version: i64,
    /// 事务版本。
    #[serde(rename = "transactionsVersion")]
    pub transactions_version: i64,
}

/// 记录内嵌的同步状态（`{versions, sections, lastSyncedAt}`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationSyncState {
    /// 四字段版本。
    pub versions: OrganizationSyncVersions,
    /// 已同步区段。
    pub sections: Vec<OrganizationSyncSection>,
    /// 最近同步时间（ms；本地新建未同步为 0）。
    #[serde(rename = "lastSyncedAt")]
    pub last_synced_at: i64,
}

/// 组织记录（`org:meta:<orgId>` 的值）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationRecord {
    /// `org_` + 16 hex。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 组织名（trim + 连续空白归一）。
    pub name: String,
    /// 描述。
    #[serde(default)]
    pub description: String,
    /// 基础插件域（`plugin:` 前缀；旧记录可缺省）。
    #[serde(rename = "basePluginDomain", default, skip_serializing_if = "Option::is_none")]
    pub base_plugin_domain: Option<String>,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// 创建者 rootId。
    #[serde(rename = "createdBy")]
    pub created_by: String,
    /// 最近更新时间（ms）；四版本字段的实际口径来源。
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    /// 成员列表。
    #[serde(default)]
    pub members: Vec<OrganizationMember>,
    /// 同步状态（本地新建后即存在）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<OrganizationSyncState>,
    /// 动态字段（含 `recoverySecret` 等非保留键，随快照 metadata 流动）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl OrganizationRecord {
    /// `recoverySecret` 动态键名。
    pub const RECOVERY_SECRET_KEY: &'static str = "recoverySecret";

    /// 读取组织恢复盐（动态键，64 hex；缺失返回 `None`）。
    ///
    /// 对齐 TS：`recoverySecret` 是记录上的普通（非保留）键，经
    /// `summary.metadata` 随快照 gossip 扩散（org.md §10）。
    pub fn recovery_secret(&self) -> Option<&str> {
        self.extra
            .get(Self::RECOVERY_SECRET_KEY)
            .and_then(Value::as_str)
    }

    /// 写入组织恢复盐（创建时生成 / admin 惰性补齐）。
    pub fn set_recovery_secret(&mut self, secret: impl Into<String>) {
        self.extra.insert(
            Self::RECOVERY_SECRET_KEY.to_string(),
            Value::String(secret.into()),
        );
    }

    /// 按 rootId 查成员。
    pub fn find_member(&self, root_id: &str) -> Option<&OrganizationMember> {
        self.members.iter().find(|m| m.root_id == root_id)
    }

    /// 某 rootId 是否为 admin。
    pub fn is_admin(&self, root_id: &str) -> bool {
        self.find_member(root_id)
            .is_some_and(|m| m.role == OrganizationRole::Admin)
    }

    /// admin 总数。
    pub fn admin_count(&self) -> usize {
        self.members
            .iter()
            .filter(|m| m.role == OrganizationRole::Admin)
            .count()
    }
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
    /// 动态字段。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    #[test]
    fn root_id_validation() {
        assert!(is_valid_root_id(&rid('a')));
        assert!(is_valid_root_id(&format!("  {} ", rid('F')))); // trim + lowercase
        assert!(!is_valid_root_id(&rid('g')));
        assert!(!is_valid_root_id(&rid('a')[..63]));
        assert!(!is_valid_root_id(""));
        assert_eq!(normalize_root_id(&format!(" {} ", rid('B'))).unwrap(), rid('b'));
        assert!(normalize_root_id("xyz").is_err());
    }

    #[test]
    fn text_and_domain_normalization() {
        assert_eq!(normalize_text("  hello   world \n ", "Name").unwrap(), "hello world");
        assert!(normalize_text("   ", "Name").is_err());
        assert_eq!(normalize_plugin_domain(" plugin:chat ").unwrap(), "plugin:chat");
        assert!(normalize_plugin_domain("chat").is_err());
        assert!(normalize_plugin_domain("plugin:").is_err());
        assert!(normalize_plugin_domain("  ").is_err());
    }

    #[test]
    fn node_info_normalization() {
        // 全空 → required 错误
        let empty = OrganizationNodeInfo::default();
        assert!(matches!(
            normalize_node_info(&empty),
            Err(OrgError::NodeInfoRequired)
        ));
        // peerId 过短
        let short = OrganizationNodeInfo {
            peer_id: Some("abc".to_string()),
            addresses: vec![],
        };
        assert!(matches!(
            normalize_node_info(&short),
            Err(OrgError::InvalidPeerId)
        ));
        // trim + 滤空
        let ok = OrganizationNodeInfo {
            peer_id: Some("  peer-12345  ".to_string()),
            addresses: vec![" /ip4/1.2.3.4/tcp/1 ".to_string(), "  ".to_string()],
        };
        let n = normalize_node_info(&ok).unwrap();
        assert_eq!(n.peer_id.as_deref(), Some("peer-12345"));
        assert_eq!(n.addresses, vec!["/ip4/1.2.3.4/tcp/1"]);
    }

    #[test]
    fn optional_node_info() {
        assert_eq!(normalize_optional_node_info(None).unwrap(), None);
        let all_blank = OrganizationNodeInfo {
            peer_id: Some("   ".to_string()),
            addresses: vec![" ".to_string()],
        };
        assert_eq!(normalize_optional_node_info(Some(&all_blank)).unwrap(), None);
        let valid = OrganizationNodeInfo {
            peer_id: Some("peer-12345".to_string()),
            addresses: vec![],
        };
        assert!(normalize_optional_node_info(Some(&valid)).unwrap().is_some());
    }

    #[test]
    fn sort_members_admin_first_then_joined_at() {
        let m = |root: char, role: OrganizationRole, joined: i64| OrganizationMember {
            root_id: rid(root),
            role,
            joined_at: joined,
            added_by: rid('f'),
            node_info: None,
            extra: Default::default(),
        };
        let members = vec![
            m('a', OrganizationRole::Member, 300),
            m('b', OrganizationRole::Member, 100),
            m('c', OrganizationRole::Admin, 500),
            m('d', OrganizationRole::Admin, 200),
        ];
        let sorted = sort_members(&members);
        let order: Vec<char> = sorted
            .iter()
            .map(|m| m.root_id.chars().next().unwrap())
            .collect();
        assert_eq!(order, vec!['d', 'c', 'b', 'a']);
    }

    #[test]
    fn org_id_and_secret_shapes() {
        let id = generate_organization_id();
        assert!(id.starts_with("org_") && id.len() == 4 + 16);
        assert!(id[4..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let secret = generate_recovery_secret();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn recovery_secret_via_dynamic_extra() {
        let mut record = OrganizationRecord::default();
        assert_eq!(record.recovery_secret(), None);
        record.set_recovery_secret("ab".repeat(32));
        assert_eq!(record.recovery_secret(), Some("ab".repeat(32).as_str()));
        // 动态键序列化为顶层键（与 TS 记录形状一致）
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["recoverySecret"], serde_json::json!("ab".repeat(32)));
        // 反序列化后仍在 extra 中（不会丢）
        let back: OrganizationRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back.recovery_secret(), Some("ab".repeat(32).as_str()));
    }
}
