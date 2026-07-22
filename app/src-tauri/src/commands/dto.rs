//! 壳侧 DTO：spark-core 的不少领域类型刻意不带 serde 派生（字段顺序敏感或纯内部），
//! 跨越命令边界的入参/出参在此定义 serde 形状（camelCase，与 TS preload 类型对齐）。
//!
//! 出参字段名一律对齐 TS 侧既有类型（preload.ts），保证适配层零加工透传。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spark_core::collection::{CollectionConfig, FilterOp, QueryFilter, QueryOptions, QueryResult};
use spark_core::org::service::{CreateOrganizationInput, CreatedOrgInvite, InviteAcceptance};
use spark_core::org::{MemberSyncOverview, OrgSyncOverview};
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
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrgInputDto {
    pub name: String,
    pub description: Option<String>,
    pub base_plugin_domain: String,
}

impl From<CreateOrgInputDto> for CreateOrganizationInput {
    fn from(dto: CreateOrgInputDto) -> Self {
        Self {
            name: dto.name,
            description: dto.description,
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

/// 组织 K 副本概览（TS `getSyncOverview` 返回）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgSyncOverviewDto {
    pub org_id: String,
    pub replica_target: u32,
    pub synced_peers: u32,
    pub total_members: u32,
    pub members: Vec<MemberSyncOverviewDto>,
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
}

impl P2pInfoDto {
    pub fn stopped() -> Self {
        Self {
            initialized: true,
            started: false,
            peer_id: None,
            addresses: Vec::new(),
            connected_peers: Vec::new(),
            spark_sync_subscribers: Vec::new(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_config_dto_defaults_and_strategy_mapping() {
        let dto: CollectionConfigDto = serde_json::from_str("{}").unwrap();
        let config = dto.into_config().unwrap();
        assert!(config.indexed_fields.is_empty());
        assert_eq!(config.sync_strategy, None);

        let dto: CollectionConfigDto =
            serde_json::from_str(r#"{"indexedFields":["author.id"],"syncStrategy":"lww"}"#).unwrap();
        let config = dto.into_config().unwrap();
        assert_eq!(config.indexed_fields, vec!["author.id".to_string()]);
        assert_eq!(config.sync_strategy, Some(SyncStrategy::Lww));

        let dto: CollectionConfigDto =
            serde_json::from_str(r#"{"syncStrategy":"bogus"}"#).unwrap();
        assert!(dto.into_config().is_err());
    }

    #[test]
    fn query_options_dto_maps_ops() {
        let dto: QueryOptionsDto = serde_json::from_str(
            r#"{"limit":10,"reverse":true,"filter":[{"field":"kind","value":"post"},{"field":"ts","value":5,"op":"gte"}]}"#,
        )
        .unwrap();
        let options = dto.into_options().unwrap();
        assert_eq!(options.limit, Some(10));
        assert!(options.reverse);
        assert_eq!(options.filter.len(), 2);
        assert_eq!(options.filter[0].op, FilterOp::Eq);
        assert_eq!(options.filter[1].op, FilterOp::Gte);

        let bad: QueryOptionsDto =
            serde_json::from_str(r#"{"filter":[{"field":"a","value":1,"op":"nope"}]}"#).unwrap();
        assert!(bad.into_options().is_err());
    }

    #[test]
    fn query_result_dto_uses_camel_case_cursor() {
        let dto = QueryResultDto {
            items: vec![DocItemDto {
                id: "a".into(),
                data: serde_json::json!({"x": 1}),
            }],
            next_cursor: Some("a".into()),
        };
        let text = serde_json::to_string(&dto).unwrap();
        assert!(text.contains("\"nextCursor\":\"a\""));
        assert!(text.contains("\"items\""));
    }

    #[test]
    fn p2p_info_dto_stopped_shape() {
        let text = serde_json::to_value(P2pInfoDto::stopped()).unwrap();
        assert_eq!(text["started"], serde_json::json!(false));
        assert_eq!(text["peerId"], serde_json::Value::Null);
    }
}
