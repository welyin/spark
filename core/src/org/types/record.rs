//! 组织记录与同步版本/状态类型（含 extra 动态键访问器）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::member::{OrganizationMember, OrganizationRole};

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
    /// 组织 logo（`data:image/` data URL；空串 = 无 logo，旧记录缺省为空串，
    /// 经快照 summary 在成员间同步）。空串丢键：保持与 TS golden 向量及旧线形
    /// 的字节一致（对齐 gateways/isPublic 的缺省丢键口径）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub avatar: String,
    /// 基础插件域（`plugin:` 前缀；旧记录可缺省）。
    #[serde(
        rename = "basePluginDomain",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
    /// 组织网关 rootId 列表（org.md §14，保留键：管理员指定 2–3 个，经快照同步）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gateways: Vec<String>,
    /// 自认证组织地址（org.md §15，保留键：创建时由组织根公钥派生，经快照同步）。
    #[serde(
        rename = "orgAddress",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_address: Option<String>,
    /// 公开组织标志（org.md §16，保留键：公开后网关节点发布组织地址记录）。
    #[serde(rename = "isPublic", default, skip_serializing_if = "is_false")]
    pub is_public: bool,
    /// 动态字段（含 `recoverySecret`/`orgSecret` 等非保留键，随快照 metadata 流动）。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `isPublic` 缺省（false）时丢键的 serde 辅助（对齐 gateways 的缺省丢键口径）。
pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

impl OrganizationRecord {
    /// `recoverySecret` 动态键名。
    pub const RECOVERY_SECRET_KEY: &'static str = "recoverySecret";

    /// `orgSecret` 动态键名（org.md §13：不展示给用户，仅随成员同步链路流动）。
    pub const ORG_SECRET_KEY: &'static str = "orgSecret";

    /// `orgRootSecret` 动态键名（org.md §15：组织根私钥密文）。
    ///
    /// **保留键的例外**：不进快照 metadata（snapshot.rs `extract_metadata` 显式
    /// 剔除 + org-share 推送前 `strip_org_root_secret` 剥除），不同步出本机；
    /// 不展示给用户。注意**不能**加入 `ORGANIZATION_SYNC_RESERVED_KEYS`——
    /// 该表同时用于合并时剔除 extra 保留键，会把本机持有的私钥一并抹掉。
    pub const ORG_ROOT_SECRET_KEY: &'static str = "orgRootSecret";

    /// `orgDisplayName` 动态键名（组织地址记录的展示名覆盖，org.md §16；
    /// 非敏感，经 `summary.metadata` 随快照流动）。缺省时地址记录用组织名。
    pub const ORG_DISPLAY_NAME_KEY: &'static str = "orgDisplayName";

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

    /// 读取组织私有 DHT 派生密钥（动态键，64 hex；缺失返回 `None`）。
    ///
    /// org.md §13：与 `recoverySecret` 同一 extra 模式，经 `summary.metadata`
    /// 随快照在成员间流动；不进入任何面向非成员的协议面，UI 不渲染。
    pub fn org_secret(&self) -> Option<&str> {
        self.extra.get(Self::ORG_SECRET_KEY).and_then(Value::as_str)
    }

    /// 写入组织私有 DHT 派生密钥（组织创建时生成）。
    pub fn set_org_secret(&mut self, secret: impl Into<String>) {
        self.extra.insert(
            Self::ORG_SECRET_KEY.to_string(),
            Value::String(secret.into()),
        );
    }

    /// 读取组织根私钥密文（动态键，base64；缺失返回 `None`）。
    ///
    /// org.md §15：密文永不进快照/不出本机；UI 不渲染。
    pub fn org_root_secret(&self) -> Option<&str> {
        self.extra
            .get(Self::ORG_ROOT_SECRET_KEY)
            .and_then(Value::as_str)
    }

    /// 写入组织根私钥密文（组织创建时 / admin 懒补齐）。
    pub fn set_org_root_secret(&mut self, sealed: impl Into<String>) {
        self.extra.insert(
            Self::ORG_ROOT_SECRET_KEY.to_string(),
            Value::String(sealed.into()),
        );
    }

    /// 读取组织地址记录的展示名覆盖（动态键；缺失返回 `None`）。
    pub fn display_name_override(&self) -> Option<&str> {
        self.extra
            .get(Self::ORG_DISPLAY_NAME_KEY)
            .and_then(Value::as_str)
    }

    /// 写入/清除组织地址记录的展示名覆盖（`None` 删除该键）。
    pub fn set_display_name_override(&mut self, display_name: Option<&str>) {
        match display_name {
            Some(name) => {
                self.extra.insert(
                    Self::ORG_DISPLAY_NAME_KEY.to_string(),
                    Value::String(name.to_string()),
                );
            }
            None => {
                self.extra.remove(Self::ORG_DISPLAY_NAME_KEY);
            }
        }
    }

    /// 某 rootId 是否为组织网关（org.md §14）。
    pub fn is_gateway(&self, root_id: &str) -> bool {
        self.gateways.iter().any(|g| g == root_id)
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
