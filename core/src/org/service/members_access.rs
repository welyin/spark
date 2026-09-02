//! 成员组织身份访问密钥发布（O4 工作项 1）：`OrganizationMember.accessKey`。
//!
//! 组织身份字段仅本人可改（与 nickname/avatar 同路径，经快照 members 段全员
//! 传播）。`access_key` 由内核派生（`org-access:{orgId}` 域身份公钥 + 根密钥
//! 绑定签名）后传入——纯逻辑层不做密码学派生（派生在 `kernel::data_access`）。
//!
//! 独立成文件：从 `members` 拆出，避免 `members.rs` 超 650 行硬线（Z5）。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{OrganizationAccessKey, OrganizationRecord};
use super::super::{OrgError, Result};
use super::OrganizationService;

impl OrganizationService {
    /// 发布/更新本人组织身份访问密钥（O4）：`OrganizationMember.access_key`。
    ///
    /// 组织身份字段仅本人可改（与 nickname/avatar 同路径，经快照 members 段
    /// 全员传播）。`access_key` 由内核派生（`org-access:{orgId}` 域身份公钥 +
    /// 根密钥绑定签名）后传入——纯逻辑层不做密码学派生。
    ///
    /// - 幂等：与既有 accessKey 一致时不 bump 版本、不追加事务；
    /// - 变更后追加事务并重建 sync（同 update_my_identity 模式）。
    pub fn publish_access_key<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        access_key: &OrganizationAccessKey,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        if Self::publish_access_key_mutate(storage, &mut record, org_id, access_key, current_root_id, now_ms)? {
            Self::save_record(storage, &record)?;
        }
        Ok(record)
    }

    /// pdsync 感知的 [`Self::publish_access_key`]：落库走原子段原语
    /// [`Self::update_record_atomic`]（F8——本函数正是联调 F1/F8 的高频
    /// 并发写入口；`org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn publish_access_key_pdsync<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        org_id: &str,
        access_key: &OrganizationAccessKey,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::publish_access_key_mutate(storage, record, org_id, access_key, current_root_id, now_ms)
        })
    }

    /// F8 拆段的纯变更段：返回是否发生变更（accessKey 无变化 → Ok(false)
    /// 幂等无写）。
    fn publish_access_key_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        access_key: &OrganizationAccessKey,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(index) = record
            .members
            .iter()
            .position(|m| m.root_id == current_root_id)
        else {
            return Err(OrgError::MemberNotFound);
        };
        // 幂等：accessKey 无变化不 bump 版本
        if record.members[index].access_key.as_ref() == Some(access_key) {
            return Ok(false);
        }
        record.members[index].access_key = Some(access_key.clone());
        record.updated_at = now_ms;
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: org_id.to_string(),
                type_: OrganizationTransactionType::MemberUpdate,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: Some(current_root_id.to_string()),
                summary: "发布组织身份访问密钥".to_string(),
                // m1：不落完整公钥/签名（审计面记录变更事实，不含密码学材料）
                payload: Some(
                    [("accessKey".to_string(), Value::from(true))].into_iter().collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(
            record,
            previous_last_synced_at,
            transaction.created_at,
        );
        Ok(true)
    }
}
