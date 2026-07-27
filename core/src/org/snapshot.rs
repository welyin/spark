//! 组织同步快照与合并（对齐 desktop/src/main/organization/sync.ts 与
//! p2p/org-share-snapshot.ts）。
//!
//! 线形（org.md §4.1）：
//! `{ orgId, summary{...固定字段, memberCount, adminCount, metadata?},
//!    members[...], transactions[...], sync: <仅四字段 versions> }`
//!
//! 两种上线线形（org.md §4.5）：org-share 推送发**原始 OrganizationRecord**、
//! org-pull 响应发**重建快照**；接收侧统一经 [`normalize_incoming_snapshot`]
//! 分派，两者都必须接受。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
    OrganizationSyncSection, OrganizationSyncState, OrganizationSyncVersions,
};
use super::{OrgError, Result};

/// 快照构建时的保留键（sync.ts:26-36 + org.md §4.1 的 gateways + §15/§16 的
/// orgAddress/isPublic）：其余键全部流入 `summary.metadata`。
///
/// ⚠️ `orgRootSecret`（组织根私钥密文）**不在**此表——本表同时用于合并时剔除
/// extra 保留键，会把本机持有的私钥抹掉；其"不进快照"由
/// [`extract_metadata`] 显式剔除 + 推送侧 `strip_org_root_secret` + 合并侧
/// [`merge_organization_sync_snapshot`] 插入处跳过 三处共同保证（§15）。
pub const ORGANIZATION_SYNC_RESERVED_KEYS: [&str; 12] = [
    "orgId",
    "name",
    "description",
    "basePluginDomain",
    "createdAt",
    "createdBy",
    "updatedAt",
    "members",
    "sync",
    "gateways",
    "orgAddress",
    "isPublic",
];

/// 快照中的成员条目（仅五字段；构建快照时成员对象的动态键被丢弃）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMember {
    /// 成员 rootId。
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
    /// 节点信息。
    #[serde(rename = "nodeInfo", default, skip_serializing_if = "Option::is_none")]
    pub node_info: Option<OrganizationNodeInfo>,
}

impl From<&OrganizationMember> for SnapshotMember {
    fn from(member: &OrganizationMember) -> Self {
        Self {
            root_id: member.root_id.clone(),
            role: member.role,
            joined_at: member.joined_at,
            added_by: member.added_by.clone(),
            node_info: member.node_info.clone(),
        }
    }
}

/// 快照 summary 段。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationSyncSummary {
    /// 组织 id。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 组织名。
    pub name: String,
    /// 描述。
    #[serde(default)]
    pub description: String,
    /// 基础插件域。
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
    /// 最近更新时间（ms）。
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    /// 成员总数。
    #[serde(rename = "memberCount")]
    pub member_count: i64,
    /// admin 总数。
    #[serde(rename = "adminCount")]
    pub admin_count: i64,
    /// 组织网关 rootId 列表（org.md §14 保留键：不进 metadata，作为 summary
    /// 显式字段随快照传播；缺省 = 发送方未设置，接收方保留本地值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateways: Option<Vec<String>>,
    /// 自认证组织地址（org.md §15 保留键：与 gateways 同口径传播/回退）。
    #[serde(
        rename = "orgAddress",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_address: Option<String>,
    /// 公开组织标志（org.md §16 保留键：仅在 true 时显式传播；缺省保留本地值——
    /// 关闭公开本期不跨节点传播，发布行为只发生在持钥节点，本地关闭即停止
    /// 新签，DHT 旧记录随 8h TTL 与记录 ttl 自然过期）。
    #[serde(rename = "isPublic", default, skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
    /// 非保留键的剩余字段（`recoverySecret`/`orgSecret` 借此随快照流动；
    /// `orgRootSecret` 被显式剔除——§15 不同步出本机）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// 组织同步快照（sync.ts:12-24）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationSyncSnapshot {
    /// 组织 id。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 概要段。
    pub summary: OrganizationSyncSummary,
    /// 成员段。
    #[serde(default)]
    pub members: Vec<SnapshotMember>,
    /// 事务段（当前恒为空——事务不跨节点传播，org.md §3.3）。
    #[serde(default)]
    pub transactions: Vec<super::tx::OrganizationTransactionRecord>,
    /// 版本段（仅四字段 versions，无 sections/lastSyncedAt）。
    pub sync: OrganizationSyncVersions,
}

/// `buildOrganizationSyncVersions`（sync.ts:50-57）：四字段全部等于
/// `record.updatedAt`，仅 `transactionsVersion` 可独立（实际取最近事务 createdAt）。
pub fn build_organization_sync_versions(
    record: &OrganizationRecord,
    transactions_version: i64,
) -> OrganizationSyncVersions {
    OrganizationSyncVersions {
        summary_version: record.updated_at,
        members_version: record.updated_at,
        member_details_version: record.updated_at,
        transactions_version,
    }
}

/// `transactionsVersion` 缺省 = `record.updatedAt` 的便捷版本。
pub fn build_organization_sync_versions_default(
    record: &OrganizationRecord,
) -> OrganizationSyncVersions {
    build_organization_sync_versions(record, record.updated_at)
}

/// `pickSyncSectionsByPriority`（sync.ts:179-181）：常量数组。
pub fn pick_sync_sections_by_priority() -> Vec<OrganizationSyncSection> {
    vec![
        OrganizationSyncSection::Transactions,
        OrganizationSyncSection::Summary,
        OrganizationSyncSection::Members,
        OrganizationSyncSection::MemberDetails,
    ]
}

/// 提取非保留键的动态字段（`extractOrganizationSyncMetadata`，sync.ts:38-48）。
///
/// Rust 侧记录的 `extra` 经 serde flatten 天然不含保留键，与 TS 的
/// 「全部键 − 保留键」结果一致；为空时返回 `None`（TS 返回 undefined → 丢键）。
///
/// 例外剔除：`orgRootSecret`（组织根私钥密文，org.md §15 不同步出本机）——
/// 它是 extra 动态键会被 flatten 捕获，但**绝不**进快照 metadata。
fn extract_metadata(record: &OrganizationRecord) -> Option<serde_json::Map<String, Value>> {
    let mut extra = record.extra.clone();
    extra.remove(OrganizationRecord::ORG_ROOT_SECRET_KEY);
    if extra.is_empty() { None } else { Some(extra) }
}

/// `buildOrganizationSyncSnapshot`（sync.ts:59-91）。
///
/// `transactions_version`：TS 取 `transactions[0]?.createdAt ?? record.updatedAt`
/// （调用方传入的列表按时间倒序，首条即最近）。
pub fn build_organization_sync_snapshot(
    record: &OrganizationRecord,
    transactions: &[super::tx::OrganizationTransactionRecord],
) -> OrganizationSyncSnapshot {
    let transactions_version = transactions
        .first()
        .map(|tx| tx.created_at)
        .unwrap_or(record.updated_at);
    OrganizationSyncSnapshot {
        org_id: record.org_id.clone(),
        summary: OrganizationSyncSummary {
            org_id: record.org_id.clone(),
            name: record.name.clone(),
            description: record.description.clone(),
            base_plugin_domain: record.base_plugin_domain.clone(),
            created_at: record.created_at,
            created_by: record.created_by.clone(),
            updated_at: record.updated_at,
            member_count: record.members.len() as i64,
            admin_count: record.admin_count() as i64,
            gateways: if record.gateways.is_empty() {
                None
            } else {
                Some(record.gateways.clone())
            },
            org_address: record.org_address.clone(),
            is_public: record.is_public.then_some(true),
            metadata: extract_metadata(record),
        },
        members: record.members.iter().map(SnapshotMember::from).collect(),
        transactions: transactions.to_vec(),
        sync: build_organization_sync_versions(record, transactions_version),
    }
}

/// `mergeOrganizationSyncSnapshot`（sync.ts:93-164）。
///
/// - 成员按 rootId 合并：incoming 覆盖同名字段，`nodeInfo` 为 `None` 时保留 existing
/// - 动态字段：`{...existingExtra, ...incomingMetadata}` 合并后删除全部保留键；
///   `orgRootSecret`（本机根私钥密文）在插入处显式跳过，绝不接受对端注入（§15）
/// - 固定字段以 incoming 快照为准；`updatedAt = max(existing, incoming)`；
///   `basePluginDomain` 快照缺失时保留 existing
/// - `sync = { versions: snapshot.sync, sections: [summary,members,member-details,transactions],
///   lastSyncedAt: now }`（注意此处的 sections 顺序与
///   [`pick_sync_sections_by_priority`] 不同，如实复刻 sync.ts:156-160）
/// - 成员顺序对齐 JS Map 插入序：existing 成员保持原位，新 incoming 成员追加尾部
pub fn merge_organization_sync_snapshot(
    existing: Option<&OrganizationRecord>,
    snapshot: &OrganizationSyncSnapshot,
    now_ms: i64,
) -> OrganizationRecord {
    // 成员合并（保持 JS Map 的插入序语义）
    let mut merged_members: Vec<OrganizationMember> = Vec::new();
    let mut index_by_root_id: HashMap<String, usize> = HashMap::new();
    if let Some(existing) = existing {
        for member in &existing.members {
            index_by_root_id.insert(member.root_id.clone(), merged_members.len());
            merged_members.push(member.clone());
        }
    }
    for incoming in &snapshot.members {
        let node_info = incoming.node_info.clone().or_else(|| {
            index_by_root_id
                .get(&incoming.root_id)
                .and_then(|&i| merged_members[i].node_info.clone())
        });
        match index_by_root_id.get(&incoming.root_id) {
            Some(&i) => {
                // {...existingMember, ...incoming, nodeInfo: incoming ?? existing}：
                // 五字段以 incoming 为准，existing 的动态键保留
                let existing_member = &merged_members[i];
                merged_members[i] = OrganizationMember {
                    root_id: incoming.root_id.clone(),
                    role: incoming.role,
                    joined_at: incoming.joined_at,
                    added_by: incoming.added_by.clone(),
                    node_info,
                    extra: existing_member.extra.clone(),
                };
            }
            None => {
                index_by_root_id.insert(incoming.root_id.clone(), merged_members.len());
                merged_members.push(OrganizationMember {
                    root_id: incoming.root_id.clone(),
                    role: incoming.role,
                    joined_at: incoming.joined_at,
                    added_by: incoming.added_by.clone(),
                    node_info,
                    extra: Default::default(),
                });
            }
        }
    }

    // 动态字段合并：existing.extra ∪ snapshot.metadata，删除保留键
    // （Rust flatten 保证两侧本就不含保留键，此处删除为防御性对齐 TS 的 delete 调用）
    let reserved: HashSet<&str> = ORGANIZATION_SYNC_RESERVED_KEYS.into_iter().collect();
    let mut merged_extra = existing.map(|e| e.extra.clone()).unwrap_or_default();
    if let Some(metadata) = &snapshot.summary.metadata {
        for (key, value) in metadata {
            // 入站防御：`orgRootSecret`（本机根私钥密文）绝不接受对端注入——
            // 它不在保留键表内（见 [`OrganizationRecord::ORG_ROOT_SECRET_KEY`] 注释），
            // 下方 retain 剔不掉，必须在插入处显式跳过（org.md §15 不同步出本机）；
            // `orgSecret`/`recoverySecret` 等非敏感动态键不受影响，照常随快照流动（§13）
            if key == OrganizationRecord::ORG_ROOT_SECRET_KEY {
                continue;
            }
            merged_extra.insert(key.clone(), value.clone());
        }
    }
    merged_extra.retain(|key, _| !reserved.contains(key.as_str()));

    OrganizationRecord {
        org_id: snapshot.summary.org_id.clone(),
        name: snapshot.summary.name.clone(),
        description: snapshot.summary.description.clone(),
        base_plugin_domain: snapshot
            .summary
            .base_plugin_domain
            .clone()
            .or_else(|| existing.and_then(|e| e.base_plugin_domain.clone())),
        created_at: snapshot.summary.created_at,
        created_by: snapshot.summary.created_by.clone(),
        updated_at: existing
            .map(|e| e.updated_at)
            .unwrap_or(0)
            .max(snapshot.summary.updated_at),
        // gateways（保留键）：incoming 显式携带则以其为准，缺省保留 existing
        // （对齐 basePluginDomain 的缺省回退口径；org.md §14 经快照扩散）
        gateways: snapshot
            .summary
            .gateways
            .clone()
            .or_else(|| existing.map(|e| e.gateways.clone()))
            .unwrap_or_default(),
        // orgAddress / isPublic（保留键，org.md §15/§16）：同 gateways 回退口径
        org_address: snapshot
            .summary
            .org_address
            .clone()
            .or_else(|| existing.and_then(|e| e.org_address.clone())),
        is_public: snapshot
            .summary
            .is_public
            .unwrap_or_else(|| existing.is_some_and(|e| e.is_public)),
        members: merged_members,
        sync: Some(OrganizationSyncState {
            versions: snapshot.sync,
            sections: vec![
                OrganizationSyncSection::Summary,
                OrganizationSyncSection::Members,
                OrganizationSyncSection::MemberDetails,
                OrganizationSyncSection::Transactions,
            ],
            last_synced_at: now_ms,
        }),
        extra: merged_extra,
    }
}

/// `isOrganizationSyncStale`（sync.ts:166-177）：local 缺失 → true；
/// 否则 incoming 任一字段严格大于 local 对应字段 → true。
/// 两个方向可同时为 true（分叉）。
pub fn is_organization_sync_stale(
    local: Option<&OrganizationSyncVersions>,
    incoming: &OrganizationSyncVersions,
) -> bool {
    let Some(local) = local else {
        return true;
    };
    incoming.summary_version > local.summary_version
        || incoming.members_version > local.members_version
        || incoming.member_details_version > local.member_details_version
        || incoming.transactions_version > local.transactions_version
}

/// `normalizeIncomingSnapshot`（org-share-snapshot.ts:4-23）：兼容两种线形。
///
/// - 有 `summary` 且有 `sync` 且 `members` 为数组 → 原样视为快照（pull 响应路径）
/// - 否则按原始 OrganizationRecord 处理并重建快照：
///   ⚠️ **版本塌缩**——四字段全部重建为 `record.updatedAt`，发送方记录里的
///   `transactionsVersion` 丢失（org.md §4.4；线形兼容行为，如实复刻）
pub fn normalize_incoming_snapshot(value: &Value) -> Result<OrganizationSyncSnapshot> {
    let has_snapshot_shape = value.get("summary").is_some()
        && value.get("sync").is_some()
        && value.get("members").is_some_and(Value::is_array);
    if has_snapshot_shape {
        return serde_json::from_value(value.clone())
            .map_err(|e| OrgError::Malformed(format!("snapshot shape: {e}")));
    }

    let record: OrganizationRecord = serde_json::from_value(value.clone())
        .map_err(|e| OrgError::Malformed(format!("raw record shape: {e}")))?;
    let transactions: Vec<super::tx::OrganizationTransactionRecord> = value
        .get("transactions")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(build_organization_sync_snapshot(&record, &transactions))
}
