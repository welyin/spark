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

/// 快照构建时的保留键（sync.ts:26-36）：其余键全部流入 `summary.metadata`。
pub const ORGANIZATION_SYNC_RESERVED_KEYS: [&str; 9] = [
    "orgId",
    "name",
    "description",
    "basePluginDomain",
    "createdAt",
    "createdBy",
    "updatedAt",
    "members",
    "sync",
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
    #[serde(rename = "basePluginDomain", default, skip_serializing_if = "Option::is_none")]
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
    /// 非保留键的剩余字段（`recoverySecret` 借此随快照流动）。
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
fn extract_metadata(record: &OrganizationRecord) -> Option<serde_json::Map<String, Value>> {
    if record.extra.is_empty() {
        None
    } else {
        Some(record.extra.clone())
    }
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
/// - 动态字段：`{...existingExtra, ...incomingMetadata}` 合并后删除全部保留键
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
    let mut merged_extra = existing
        .map(|e| e.extra.clone())
        .unwrap_or_default();
    if let Some(metadata) = &snapshot.summary.metadata {
        for (key, value) in metadata {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::tx::OrganizationTransactionType;

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn member(root: char, role: OrganizationRole, joined: i64) -> OrganizationMember {
        OrganizationMember {
            root_id: rid(root),
            role,
            joined_at: joined,
            added_by: rid('z'),
            node_info: None,
            extra: Default::default(),
        }
    }

    fn sample_record() -> OrganizationRecord {
        let mut record = OrganizationRecord {
            org_id: "org_0123456789abcdef".to_string(),
            name: "星火".to_string(),
            description: "desc".to_string(),
            base_plugin_domain: Some("plugin:chat".to_string()),
            created_at: 1000,
            created_by: rid('a'),
            updated_at: 2000,
            members: vec![
                member('a', OrganizationRole::Admin, 1000),
                member('b', OrganizationRole::Member, 1500),
            ],
            sync: None,
            extra: Default::default(),
        };
        record.set_recovery_secret("cd".repeat(32));
        record
    }

    fn versions(v: i64) -> OrganizationSyncVersions {
        OrganizationSyncVersions {
            summary_version: v,
            members_version: v,
            member_details_version: v,
            transactions_version: v,
        }
    }

    #[test]
    fn versions_all_equal_updated_at() {
        let record = sample_record();
        let v = build_organization_sync_versions(&record, 1234);
        assert_eq!(v.summary_version, 2000);
        assert_eq!(v.members_version, 2000);
        assert_eq!(v.member_details_version, 2000);
        assert_eq!(v.transactions_version, 1234);
        let d = build_organization_sync_versions_default(&record);
        assert_eq!(d, versions(2000));
    }

    #[test]
    fn build_snapshot_metadata_carries_recovery_secret() {
        let record = sample_record();
        let snapshot = build_organization_sync_snapshot(&record, &[]);
        assert_eq!(snapshot.summary.member_count, 2);
        assert_eq!(snapshot.summary.admin_count, 1);
        let metadata = snapshot.summary.metadata.as_ref().unwrap();
        assert_eq!(
            metadata.get("recoverySecret").and_then(Value::as_str),
            Some("cd".repeat(32).as_str())
        );
        // 保留键不进 metadata
        for key in ORGANIZATION_SYNC_RESERVED_KEYS {
            assert!(!metadata.contains_key(key), "reserved key {key} leaked");
        }
        // 版本塌缩：空事务 → transactionsVersion = updatedAt
        assert_eq!(snapshot.sync, versions(2000));
    }

    #[test]
    fn build_snapshot_transactions_version_from_first_tx() {
        let record = sample_record();
        let txs = vec![super::super::tx::OrganizationTransactionRecord {
            tx_id: "t1".to_string(),
            org_id: record.org_id.clone(),
            type_: OrganizationTransactionType::MemberAdd,
            created_at: 7777,
            actor_root_id: rid('a'),
            target_root_id: None,
            summary: "s".to_string(),
            payload: None,
        }];
        let snapshot = build_organization_sync_snapshot(&record, &txs);
        assert_eq!(snapshot.sync.transactions_version, 7777);
        assert_eq!(snapshot.transactions.len(), 1);
    }

    #[test]
    fn stale_rules() {
        let local = versions(100);
        // local 缺失 → stale
        assert!(is_organization_sync_stale(None, &local));
        // 完全等价 → 不 stale
        assert!(!is_organization_sync_stale(Some(&local), &versions(100)));
        // 任一字段严格更大 → stale
        let mut incoming = versions(100);
        incoming.transactions_version = 101;
        assert!(is_organization_sync_stale(Some(&local), &incoming));
        // 双向可同时为 true（分叉）
        let mut fork_a = versions(100);
        fork_a.summary_version = 200;
        let mut fork_b = versions(100);
        fork_b.members_version = 200;
        assert!(is_organization_sync_stale(Some(&fork_a), &fork_b));
        assert!(is_organization_sync_stale(Some(&fork_b), &fork_a));
        // 全字段落后 → 不 stale
        assert!(!is_organization_sync_stale(Some(&versions(200)), &versions(100)));
    }

    #[test]
    fn merge_into_empty() {
        let record = sample_record();
        let snapshot = build_organization_sync_snapshot(&record, &[]);
        let merged = merge_organization_sync_snapshot(None, &snapshot, 5555);
        assert_eq!(merged.org_id, record.org_id);
        assert_eq!(merged.name, "星火");
        assert_eq!(merged.members.len(), 2);
        assert_eq!(merged.updated_at, 2000);
        let sync = merged.sync.as_ref().unwrap();
        assert_eq!(sync.versions, versions(2000));
        assert_eq!(sync.last_synced_at, 5555);
        assert_eq!(
            sync.sections,
            vec![
                OrganizationSyncSection::Summary,
                OrganizationSyncSection::Members,
                OrganizationSyncSection::MemberDetails,
                OrganizationSyncSection::Transactions,
            ]
        );
        // recoverySecret 经 metadata 落到 merged.extra
        assert_eq!(merged.recovery_secret(), Some("cd".repeat(32).as_str()));
    }

    #[test]
    fn merge_member_nodeinfo_fallback_and_order() {
        let mut existing = sample_record();
        existing.members[1].node_info = Some(OrganizationNodeInfo {
            peer_id: Some("peer-b-123".to_string()),
            addresses: vec!["/ip4/9.9.9.9/tcp/1".to_string()],
        });
        existing.updated_at = 3000; // 本地 updatedAt 更大 → max 保留

        // incoming：b 不带 nodeInfo（应保留 existing），新成员 c，且 a 角色被覆盖
        let mut incoming_record = sample_record();
        incoming_record.members = vec![
            {
                let mut m = member('a', OrganizationRole::Member, 1000);
                m.added_by = rid('y');
                m
            },
            member('b', OrganizationRole::Member, 1500),
            member('c', OrganizationRole::Member, 2500),
        ];
        let snapshot = build_organization_sync_snapshot(&incoming_record, &[]);

        let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 9999);
        // 成员顺序：existing 原位（a, b），新成员 c 追加
        let order: Vec<char> = merged
            .members
            .iter()
            .map(|m| m.root_id.chars().next().unwrap())
            .collect();
        assert_eq!(order, vec!['a', 'b', 'c']);
        // a 的字段被 incoming 覆盖
        assert_eq!(merged.members[0].role, OrganizationRole::Member);
        assert_eq!(merged.members[0].added_by, rid('y'));
        // b 的 nodeInfo 保留 existing 值
        assert_eq!(
            merged.members[1].node_info.as_ref().unwrap().peer_id.as_deref(),
            Some("peer-b-123")
        );
        // updatedAt = max(3000, 2000)
        assert_eq!(merged.updated_at, 3000);
    }

    #[test]
    fn merge_base_plugin_domain_fallback() {
        let mut existing = sample_record();
        existing.base_plugin_domain = Some("plugin:keep".to_string());
        let mut snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
        snapshot.summary.base_plugin_domain = None;
        let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 1);
        assert_eq!(merged.base_plugin_domain.as_deref(), Some("plugin:keep"));
        // existing 缺失时则为 None
        let merged = merge_organization_sync_snapshot(None, &snapshot, 1);
        assert_eq!(merged.base_plugin_domain, None);
    }

    #[test]
    fn merge_dynamic_metadata_overrides_and_strips_reserved() {
        let mut existing = sample_record();
        existing
            .extra
            .insert("customKey".to_string(), Value::from("old"));
        let mut snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
        let mut metadata = serde_json::Map::new();
        metadata.insert("customKey".to_string(), Value::from("new"));
        metadata.insert("anotherKey".to_string(), Value::from(42));
        // 恶意/异常 metadata 携带保留键 → 合并后必须被剔除
        metadata.insert("name".to_string(), Value::from("hijack"));
        snapshot.summary.metadata = Some(metadata);

        let merged = merge_organization_sync_snapshot(Some(&existing), &snapshot, 1);
        assert_eq!(
            merged.extra.get("customKey").and_then(Value::as_str),
            Some("new")
        );
        assert_eq!(merged.extra.get("anotherKey").cloned(), Some(Value::from(42)));
        assert_eq!(merged.name, snapshot.summary.name, "保留键不得经 metadata 注入");
        assert!(!merged.extra.contains_key("name"));
    }

    #[test]
    fn normalize_snapshot_shape_passthrough() {
        let snapshot = build_organization_sync_snapshot(&sample_record(), &[]);
        let value = serde_json::to_value(&snapshot).unwrap();
        let normalized = normalize_incoming_snapshot(&value).unwrap();
        assert_eq!(normalized, snapshot);
    }

    #[test]
    fn normalize_raw_record_collapses_versions() {
        // 原始记录线形（org-share 推送路径）：sync 带 sections/lastSyncedAt
        let mut record = sample_record();
        record.sync = Some(OrganizationSyncState {
            versions: OrganizationSyncVersions {
                summary_version: 2000,
                members_version: 2000,
                member_details_version: 2000,
                transactions_version: 8888, // 独立事务版本——重建后丢失
            },
            sections: pick_sync_sections_by_priority(),
            last_synced_at: 4321,
        });
        let value = serde_json::to_value(&record).unwrap();
        let normalized = normalize_incoming_snapshot(&value).unwrap();
        // 版本塌缩：四字段全部 = updatedAt（spec §4.4 线形兼容行为）
        assert_eq!(normalized.sync, versions(2000));
        assert_eq!(normalized.summary.name, "星火");
        assert_eq!(normalized.members.len(), 2);
        // recoverySecret 经 record extra → metadata 保留
        assert_eq!(
            normalized
                .summary
                .metadata
                .as_ref()
                .unwrap()
                .get("recoverySecret")
                .and_then(Value::as_str),
            Some("cd".repeat(32).as_str())
        );
    }

    #[test]
    fn normalize_rejects_garbage() {
        assert!(normalize_incoming_snapshot(&Value::Null).is_err());
        assert!(normalize_incoming_snapshot(&serde_json::json!({"foo": 1})).is_err());
        assert!(normalize_incoming_snapshot(&serde_json::json!("str")).is_err());
    }
}
