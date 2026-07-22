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
    #[serde(rename = "targetRootId", default, skip_serializing_if = "Option::is_none")]
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
    Ok(serde_json::from_str::<OrganizationTransactionRecord>(&value)
        .map(|tx| tx.created_at)
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn tx(org_id: &str, created_at: i64) -> OrganizationTransactionRecord {
        OrganizationTransactionRecord {
            tx_id: String::new(),
            org_id: org_id.to_string(),
            type_: OrganizationTransactionType::MemberAdd,
            created_at,
            actor_root_id: rid('a'),
            target_root_id: Some(rid('b')),
            summary: "添加成员".to_string(),
            payload: None,
        }
    }

    #[test]
    fn append_generates_tx_id_and_persists() {
        let mut storage = MemoryStorage::new();
        let appended = append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
        assert_eq!(appended.tx_id.len(), 16);
        assert!(appended.tx_id.bytes().all(|b| b.is_ascii_hexdigit()));
        let key = organization_transaction_key("org_x", 1000, &appended.tx_id);
        assert_eq!(key, format!("org:tx:org_x:1000:{}", appended.tx_id));
        let raw = storage.get(&key).unwrap().unwrap();
        let parsed: OrganizationTransactionRecord = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, appended);
        // 无 targetRootId/payload 时丢键（对齐 TS 可选字段）
        let sparse = OrganizationTransactionRecord {
            target_root_id: None,
            ..tx("org_x", 1001)
        };
        let json = serde_json::to_string(&sparse).unwrap();
        assert!(!json.contains("targetRootId"));
        assert!(!json.contains("payload"));
    }

    #[test]
    fn list_reverse_chronological_with_limit() {
        let mut storage = MemoryStorage::new();
        for created_at in [1000, 3000, 2000, 5000, 4000] {
            append_organization_transaction(&mut storage, tx("org_x", created_at)).unwrap();
        }
        // 其他组织的事务不被扫到
        append_organization_transaction(&mut storage, tx("org_y", 9999)).unwrap();

        let all = list_organization_transactions(&storage, "org_x", 20).unwrap();
        let times: Vec<i64> = all.iter().map(|t| t.created_at).collect();
        assert_eq!(times, vec![5000, 4000, 3000, 2000, 1000]);

        let top2 = list_organization_transactions(&storage, "org_x", 2).unwrap();
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].created_at, 5000);
        assert_eq!(top2[1].created_at, 4000);
    }

    #[test]
    fn list_skips_corrupted_rows() {
        let mut storage = MemoryStorage::new();
        append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
        storage.put("org:tx:org_x:2000:bad", "{not json").unwrap();
        let list = list_organization_transactions(&storage, "org_x", 20).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].created_at, 1000);
    }

    #[test]
    fn latest_version() {
        let mut storage = MemoryStorage::new();
        assert_eq!(get_latest_organization_transaction_version(&storage, "org_x").unwrap(), 0);
        append_organization_transaction(&mut storage, tx("org_x", 1000)).unwrap();
        append_organization_transaction(&mut storage, tx("org_x", 3000)).unwrap();
        append_organization_transaction(&mut storage, tx("org_x", 2000)).unwrap();
        assert_eq!(get_latest_organization_transaction_version(&storage, "org_x").unwrap(), 3000);
        // 首条损坏 → 0
        storage.put("org:tx:org_x:9999:bad", "oops").unwrap();
        assert_eq!(get_latest_organization_transaction_version(&storage, "org_x").unwrap(), 0);
    }
}
