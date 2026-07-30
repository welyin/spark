//! 壳侧 DTO：spark-core 的不少领域类型刻意不带 serde 派生（字段顺序敏感或纯内部），
//! 跨越命令边界的入参/出参在此定义 serde 形状（camelCase，与 TS preload 类型对齐）。
//!
//! 出参字段名一律对齐 TS 侧既有类型（preload.ts），保证适配层零加工透传。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spark_core::collection::{CollectionConfig, FilterOp, QueryFilter, QueryOptions, QueryResult};
use spark_core::org::service::{CreateOrganizationInput, CreatedOrgInvite, InviteAcceptance};
use spark_core::org::{MemberSyncOverview, OrgAddressRecord, OrgSyncOverview};
use spark_core::p2p::LocalP2PNodeInfo;
use spark_core::schema::SyncStrategy;

// ------------------------------------------------------------------
// 通用结果
// ------------------------------------------------------------------

/// `{ success }` 形状（TS 多个 IPC 的返回约定）。
#[derive(Clone, Debug, Serialize)]
pub struct SuccessResult {
    pub success: bool,
}

impl SuccessResult {
    pub fn ok() -> Self {
        Self { success: true }
    }
}

// ------------------------------------------------------------------
// 文档
// ------------------------------------------------------------------

/// `CollectionConfig` 入参（TS `CollectionConfig`；全部字段可省）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CollectionConfigDto {
    pub indexed_fields: Vec<String>,
    pub enable_evidence: Option<bool>,
    pub sync_strategy: Option<String>,
    pub governance: Option<bool>,
}

impl CollectionConfigDto {
    pub fn into_config(self) -> Result<CollectionConfig, String> {
        let sync_strategy = self
            .sync_strategy
            .as_deref()
            .map(|raw| match raw {
                "append-only" => Ok(SyncStrategy::AppendOnly),
                "lww" => Ok(SyncStrategy::Lww),
                other => Err(format!(
                    "syncStrategy must be 'append-only' or 'lww', got {other:?}"
                )),
            })
            .transpose()?;
        Ok(CollectionConfig {
            indexed_fields: self.indexed_fields,
            enable_evidence: self.enable_evidence,
            sync_strategy,
            governance: self.governance,
        })
    }
}

/// 单个查询条件（TS `CollectionQueryFilter`）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryFilterDto {
    pub field: String,
    pub value: Value,
    pub op: Option<String>,
}

impl QueryFilterDto {
    fn into_filter(self) -> Result<QueryFilter, String> {
        let op = match self.op.as_deref().unwrap_or("eq") {
            "eq" => FilterOp::Eq,
            "startsWith" => FilterOp::StartsWith,
            "gt" => FilterOp::Gt,
            "lt" => FilterOp::Lt,
            "gte" => FilterOp::Gte,
            "lte" => FilterOp::Lte,
            other => return Err(format!("unsupported filter op: {other:?}")),
        };
        Ok(QueryFilter {
            field: self.field,
            value: self.value,
            op,
        })
    }
}

/// `QueryOptions` 入参（TS `CollectionQueryOptions`；字段均可省）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QueryOptionsDto {
    pub index_name: Option<String>,
    pub index_value: Option<Value>,
    #[serde(default)]
    pub index_prefix: bool,
    pub start_after_id: Option<String>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub filter: Vec<QueryFilterDto>,
}

impl QueryOptionsDto {
    pub fn into_options(self) -> Result<QueryOptions, String> {
        let filter = self
            .filter
            .into_iter()
            .map(QueryFilterDto::into_filter)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueryOptions {
            index_name: self.index_name,
            index_value: self.index_value,
            index_prefix: self.index_prefix,
            start_after_id: self.start_after_id,
            limit: self.limit,
            reverse: self.reverse,
            filter,
        })
    }
}

/// 查询结果（TS `CollectionQueryResult`）。
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueryResultDto {
    pub items: Vec<DocItemDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 查询结果项。
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DocItemDto {
    pub id: String,
    pub data: Value,
}

impl From<QueryResult> for QueryResultDto {
    fn from(result: QueryResult) -> Self {
        Self {
            items: result
                .items
                .into_iter()
                .map(|item| DocItemDto {
                    id: item.id,
                    data: item.data,
                })
                .collect(),
            next_cursor: result.next_cursor,
        }
    }
}

// ------------------------------------------------------------------
// 组织
// ------------------------------------------------------------------

/// 创建组织入参（TS `CreateOrganizationInput`）。
/// `basePluginDomain` 可选：组织与插件不再强关联（设计 §7.2）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrgInputDto {
    pub name: String,
    pub description: Option<String>,
    pub avatar: Option<String>,
    pub base_plugin_domain: Option<String>,
}

impl From<CreateOrgInputDto> for CreateOrganizationInput {
    fn from(dto: CreateOrgInputDto) -> Self {
        Self {
            name: dto.name,
            description: dto.description,
            avatar: dto.avatar,
            base_plugin_domain: dto.base_plugin_domain,
        }
    }
}

/// 添加成员入参（TS `addMember` 的 input：`{rootId, nodeInfo?}`）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOrgMemberInputDto {
    pub root_id: String,
    pub node_info: Option<OrgNodeInfoDto>,
}

/// 成员节点信息（TS `OrganizationNodeInfo`）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgNodeInfoDto {
    pub peer_id: Option<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
}

impl From<OrgNodeInfoDto> for spark_core::org::OrganizationNodeInfo {
    fn from(dto: OrgNodeInfoDto) -> Self {
        Self {
            peer_id: dto.peer_id,
            addresses: dto.addresses,
        }
    }
}

/// 邀请码创建结果（TS `createInvite` 返回）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreatedOrgInviteDto {
    pub invite: String,
    pub org_id: String,
    pub org_name: String,
}

impl From<CreatedOrgInvite> for CreatedOrgInviteDto {
    fn from(invite: CreatedOrgInvite) -> Self {
        Self {
            invite: invite.invite,
            org_id: invite.org_id,
            org_name: invite.org_name,
        }
    }
}

/// 加入确认结果（TS `acceptInvite` 返回）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InviteAcceptanceDto {
    pub org_id: String,
    pub org_name: String,
    pub member_count: usize,
}

impl From<InviteAcceptance> for InviteAcceptanceDto {
    fn from(acceptance: InviteAcceptance) -> Self {
        Self {
            org_id: acceptance.org_id,
            org_name: acceptance.org_name,
            member_count: acceptance.member_count,
        }
    }
}

/// 组织 K 副本概览（TS `getSyncOverview` 返回）+ 网络状态扩展（Phase 5）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgSyncOverviewDto {
    pub org_id: String,
    pub replica_target: u32,
    pub synced_peers: u32,
    pub total_members: u32,
    pub members: Vec<MemberSyncOverviewDto>,
    /// 已连接的组织成员节点数（不含本机；含本机副本数 = connectedPeers + 1）。
    pub connected_peers: u32,
    /// 恢复模式状态：idle / recovering / failed。
    pub recovery_state: String,
    /// 恢复查询发起时间（idle 时为 null）。
    pub recovery_started_at: Option<i64>,
    /// 最近一次与组织成员建立连接的时间（无记录为 null）。
    pub last_connected_at: Option<i64>,
    /// DHT 模式：off / client / server。
    pub dht_mode: String,
    /// 组织网络状态：good / unstable / lost / recovering / localOnly。
    pub status: String,
}

/// 单成员副本状态。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemberSyncOverviewDto {
    pub root_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    pub is_self: bool,
    pub ever_synced: bool,
    pub last_synced_at: Option<i64>,
}

impl From<MemberSyncOverview> for MemberSyncOverviewDto {
    fn from(member: MemberSyncOverview) -> Self {
        Self {
            root_id: member.root_id,
            peer_id: member.peer_id,
            is_self: member.is_self,
            ever_synced: member.ever_synced,
            last_synced_at: member.last_synced_at,
        }
    }
}

impl From<OrgSyncOverview> for OrgSyncOverviewDto {
    fn from(overview: OrgSyncOverview) -> Self {
        Self {
            org_id: overview.org_id,
            replica_target: overview.replica_target,
            synced_peers: overview.synced_peers,
            total_members: overview.total_members,
            members: overview
                .members
                .into_iter()
                .map(MemberSyncOverviewDto::from)
                .collect(),
            connected_peers: overview.connected_peers,
            recovery_state: overview.recovery_state.as_str().to_string(),
            recovery_started_at: overview.recovery_state.since(),
            last_connected_at: overview.last_connected_at,
            dht_mode: overview.dht_mode.as_str().to_string(),
            status: overview.status.as_str().to_string(),
        }
    }
}

/// 组织地址记录（org.md §16 线形；`displayName` 缺省丢键与 core 一致）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgAddressRecordDto {
    pub org_address: String,
    pub org_id: String,
    pub org_public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub gateways: Vec<String>,
    pub seq: u64,
    pub published_at: i64,
    pub ttl: i64,
    pub signature: String,
}

impl From<OrgAddressRecord> for OrgAddressRecordDto {
    fn from(record: OrgAddressRecord) -> Self {
        Self {
            org_address: record.org_address,
            org_id: record.org_id,
            org_public_key: record.org_public_key,
            display_name: record.display_name,
            gateways: record.gateways,
            seq: record.seq,
            published_at: record.published_at,
            ttl: record.ttl,
            signature: record.signature,
        }
    }
}

// ------------------------------------------------------------------
// P2P
// ------------------------------------------------------------------

/// 节点诊断信息（TS `LocalP2PNodeInfo` 形状；`initialized` 恒 true——
/// 能调到此命令说明内核已 init，对齐 TS ipc/p2p.ts 的语义）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct P2pInfoDto {
    pub initialized: bool,
    pub started: bool,
    pub peer_id: Option<String>,
    pub addresses: Vec<String>,
    pub connected_peers: Vec<String>,
    pub spark_sync_subscribers: Vec<String>,
    /// 登录链路自动启动 p2p 的失败原因（无则 `None`）。
    pub error: Option<String>,
}

impl P2pInfoDto {
    pub fn stopped(error: Option<String>) -> Self {
        Self {
            initialized: true,
            started: false,
            peer_id: None,
            addresses: Vec::new(),
            connected_peers: Vec::new(),
            spark_sync_subscribers: Vec::new(),
            error,
        }
    }
}

impl From<LocalP2PNodeInfo> for P2pInfoDto {
    fn from(info: LocalP2PNodeInfo) -> Self {
        Self {
            initialized: true,
            started: info.started,
            peer_id: info.peer_id,
            addresses: info.addresses,
            connected_peers: info.connected_peers,
            spark_sync_subscribers: info.spark_sync_subscribers,
            error: None,
        }
    }
}


/// IPC 边界 avatar 口径（B1 修复）：serde_json 对 present-but-null 的键反序列化
/// `Option<Option<String>>` 会坍塌为 `None`，「清除」永远到不了内核；故命令层参数
/// 用扁平的 `Option<String>`，约定 `Some("")` = 清除（与 gender/region/signature
/// 的 `''` = 清除同口径），在此映射回内核三态 `Option<Option<_>>`。
/// 泛型兼容 `Option<&str>`（org_update_info）与 `Option<String>`（OrgIdentityPatch/
/// update_profile_session）两类调用方。
pub(crate) fn avatar_patch<S: AsRef<str>>(avatar: Option<S>) -> Option<Option<S>> {
    avatar.map(|value| (!value.as_ref().is_empty()).then_some(value))
}
