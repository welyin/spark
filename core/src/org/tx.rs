//! 组织事务记录（对齐 desktop/src/main/organization/transaction-store.ts）。
//!
//! ⚠️ **事务是纯本地审计日志，不跨节点传播**（org.md §3.3/§14.6）：快照构建时
//! transactions 实参为 `[]`，接收侧 merge 也不写 `org:tx:` 键。
//!
//! 键：`org:tx:<orgId>:<createdAt>:<txId>`；`txId` = 8 随机字节 hex（16 hex）。

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

use super::Result;

/// 事务记录存储键前缀。
pub const ORG_TX_PREFIX: &str = "org:tx:";

/// 事务类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizationTransactionType {
    /// 创建组织。
    #[serde(rename = "create")]
    Create,
    /// 添加成员。
    #[serde(rename = "member-add")]
    MemberAdd,
    /// 更新成员（含 nodeInfoClaim 回填）。
    #[serde(rename = "member-update")]
    MemberUpdate,
    /// 移除成员。
    #[serde(rename = "member-remove")]
    MemberRemove,
    /// 删除组织。
    #[serde(rename = "delete")]
    Delete,
}

/// 事务记录（types.ts:4-12）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizationTransactionRecord {
    /// 8 随机字节 hex（16 hex）。
    #[serde(rename = "txId")]
    pub tx_id: String,
    /// 组织 id。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 事务类型。
    #[serde(rename = "type")]
    pub type_: OrganizationTransactionType,
    /// 事务时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// 操作者 rootId。
    #[serde(rename = "actorRootId")]
    pub actor_root_id: String,
    /// 目标成员 rootId（成员相关事务）。
    #[serde(
        rename = "targetRootId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub target_root_id: Option<String>,
    /// 人类可读摘要。
    pub summary: String,
    /// 附加载荷。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Map<String, Value>>,
}

/// 事务存储键：`org:tx:<orgId>:<createdAt>:<txId>`。
pub fn organization_transaction_key(org_id: &str, created_at: i64, tx_id: &str) -> String {
    format!("{ORG_TX_PREFIX}{org_id}:{created_at}:{tx_id}")
}

/// `appendOrganizationTransaction`：生成 txId、写入并返回记录。
///
/// `created_at` 由调用方注入（对齐 TS `record.createdAt ?? Date.now()`）。
pub fn append_organization_transaction<S: StorageBackend>(
    storage: &mut S,
    mut record: OrganizationTransactionRecord,
) -> Result<OrganizationTransactionRecord> {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    record.tx_id = hex::encode(bytes);
    let key = organization_transaction_key(&record.org_id, record.created_at, &record.tx_id);
    storage.put(&key, &serde_json::to_string(&record)?)?;
    Ok(record)
}

/// `listOrganizationTransactions`：按 createdAt 倒序取前 `limit` 条
/// （TS `reverse: true`；损坏行静默跳过）。
pub fn list_organization_transactions<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    limit: usize,
) -> Result<Vec<OrganizationTransactionRecord>> {
    let rows = storage.scan(&ScanOptions {
        prefix: format!("{ORG_TX_PREFIX}{org_id}:"),
        limit: Some(limit),
        reverse: true,
        ..Default::default()
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|(_, value)| serde_json::from_str(&value).ok())
        .collect())
}

/// `getLatestOrganizationTransactionVersion`：最近一条事务的 `createdAt`，
/// 无事务或首条损坏时返回 0。
pub fn get_latest_organization_transaction_version<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Result<i64> {
    let rows = storage.scan(&ScanOptions {
        prefix: format!("{ORG_TX_PREFIX}{org_id}:"),
        limit: Some(1),
        reverse: true,
        ..Default::default()
    })?;
    let Some((_, value)) = rows.into_iter().next() else {
        return Ok(0);
    };
    Ok(
        serde_json::from_str::<OrganizationTransactionRecord>(&value)
            .map(|tx| tx.created_at)
            .unwrap_or(0),
    )
}
