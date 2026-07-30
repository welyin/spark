//! 组织设置：名称/描述/logo（`updateOrgInfo`）、网关列表（`setOrgGateways`，
//! org.md §14）与公开标志（`setOrgPublic`，org.md §16）。
//!
//! 两者同口径：admin 校验、无变化时幂等返回（不 bump 版本）、变更后追加
//! 事务并重建 sync；写入结果经既有快照同步广播扩散，推送由调用方以
//! [`OrganizationService::sync_recipients`] 执行（与 addMember 同模式）。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    OrganizationRecord, generate_org_secret, normalize_root_id, normalize_text,
};
use super::super::{OrgError, Result, org_address};
use super::OrganizationService;

impl OrganizationService {
    /// `updateOrgInfo`：管理员更新组织名称/描述/logo。
    ///
    /// - `name` 提供时按 `normalize_text` 同口径 trim 且不可为空；`description`
    ///   提供时 trim 后覆盖（空串 = 清除描述）；未提供的字段不变
    /// - `avatar` 提供时 trim 后覆盖：空串 = 清除 logo，非空按
    ///   `identity::validate_avatar` 同口径校验，非法拒绝
    /// - 无变化时幂等返回（不 bump 版本），与 [`Self::set_org_gateways`] 同口径
    pub fn update_org_info<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        avatar: Option<&str>,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let name = name
            .map(|value| normalize_text(value, "Organization name"))
            .transpose()?;
        let description = description.map(str::trim);
        let avatar = avatar
            .map(str::trim)
            .map(|value| {
                if value.is_empty() {
                    // 空串 = 清除 logo
                    Ok(String::new())
                } else {
                    crate::identity::validate_avatar(value)
                        .map(|_| value.to_string())
                        .map_err(|e| OrgError::InvalidAvatar(e.to_string()))
                }
            })
            .transpose()?;
        let name_unchanged = name.as_deref().is_none_or(|value| value == record.name);
        let description_unchanged = description.is_none_or(|value| value == record.description);
        let avatar_unchanged = avatar.as_deref().is_none_or(|value| value == record.avatar);
        if name_unchanged && description_unchanged && avatar_unchanged {
            return Ok(record);
        }

        if let Some(value) = &name {
            record.name = value.clone();
        }
        if let Some(value) = description {
            record.description = value.to_string();
        }
        if let Some(value) = &avatar {
            record.avatar = value.clone();
        }
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
                target_root_id: None,
                summary: "更新组织信息".to_string(),
                payload: Some(
                    [
                        (
                            "name".to_string(),
                            name.as_deref().map(Value::from).unwrap_or(Value::Null),
                        ),
                        (
                            "description".to_string(),
                            description.map(Value::from).unwrap_or(Value::Null),
                        ),
                        (
                            "avatar".to_string(),
                            avatar.as_deref().map(Value::from).unwrap_or(Value::Null),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(
            &mut record,
            previous_last_synced_at,
            transaction.created_at,
        );
        Self::save_record(storage, &record)?;
        Ok(record)
    }

    /// `setOrgGateways`（org.md §14）：管理员指定 2–3 名本组织成员作为组织网关。
    ///
    /// - 每个 rootId 规范化后查重；数量须为 2–3；必须是本组织成员
    /// - 网关角色是记录字段而非成员 role；写入后经既有快照同步广播扩散
    ///   （推送由调用方以 [`Self::sync_recipients`] 执行，与 addMember 同模式）
    pub fn set_org_gateways<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        gateways: &[String],
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let mut normalized: Vec<String> = Vec::new();
        for gateway in gateways {
            let root_id = normalize_root_id(gateway).map_err(|_| OrgError::InvalidGateways)?;
            if !normalized.contains(&root_id) {
                normalized.push(root_id);
            }
        }
        if !(2..=3).contains(&normalized.len())
            || normalized.iter().any(|g| record.find_member(g).is_none())
        {
            return Err(OrgError::InvalidGateways);
        }
        if record.gateways == normalized {
            return Ok(record);
        }

        record.gateways = normalized.clone();
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
                target_root_id: None,
                summary: format!("更新组织网关（{} 个）", normalized.len()),
                payload: Some(
                    [("gateways".to_string(), Value::from(normalized.clone()))]
                        .into_iter()
                        .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(
            &mut record,
            previous_last_synced_at,
            transaction.created_at,
        );
        Self::save_record(storage, &record)?;
        Ok(record)
    }

    /// `setOrgPublic`（org.md §16）：管理员开关组织公开标志，可选更新地址记录
    /// 展示名（`displayName`）。
    ///
    /// - 公开组织的网关节点在随后的 keepalive tick 开始发布组织地址记录
    ///   （DHT + gossip，p2p-messages.md §16）
    /// - 存量组织缺组织根密钥对时，开启公开会**懒补齐**（与 recoverySecret 的
    ///   admin 惰性补齐同模式）：生成密钥对、orgAddress 落记录、私钥密文存 extra
    /// - `display_name` 提供时 trim 后覆盖 `orgDisplayName`；空串视为清除
    /// - 无变化时幂等返回（不 bump 版本），与 [`Self::set_org_gateways`] 同口径
    pub fn set_org_public<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        public: bool,
        display_name: Option<&str>,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        // 三态：None = 不更新；Some(trim 后空串) = 清除；Some(非空) = 覆盖
        let display_name = display_name.map(str::trim);
        let display_name_unchanged = match display_name {
            None => true, // 未提供 = 不更新
            Some(name) => record.display_name_override() == (!name.is_empty()).then_some(name),
        };
        if record.is_public == public && display_name_unchanged {
            return Ok(record);
        }

        // 懒补齐组织根密钥对（存量组织开启公开时；org.md §15）
        if public && record.org_address.is_none() {
            if record.org_secret().is_none() {
                record.set_org_secret(generate_org_secret());
            }
            let org_root_key = org_address::generate_org_root_signing_key();
            record.org_address = Some(org_address::org_address_from_public_key(
                &org_root_key.verifying_key().to_bytes(),
            ));
            record.set_org_root_secret(org_address::seal_org_root_secret(
                &org_root_key,
                record.org_secret().expect("orgSecret ensured above"),
            ));
        }

        record.is_public = public;
        if let Some(name) = display_name {
            // trim 后空串 = 清除（删除 orgDisplayName 键，回退用组织名）
            record.set_display_name_override((!name.is_empty()).then_some(name));
        }
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
                target_root_id: None,
                summary: if public {
                    "开启组织公开".to_string()
                } else {
                    "关闭组织公开".to_string()
                },
                payload: Some(
                    [
                        ("isPublic".to_string(), Value::from(public)),
                        (
                            "displayName".to_string(),
                            display_name
                                .filter(|s| !s.is_empty())
                                .map(|s| Value::from(s.to_string()))
                                .unwrap_or(Value::Null),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(
            &mut record,
            previous_last_synced_at,
            transaction.created_at,
        );
        Self::save_record(storage, &record)?;
        Ok(record)
    }
}
