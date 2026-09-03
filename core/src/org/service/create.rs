//! 组织创建与删除（service.ts `createOrganization`/`deleteOrganization`）。
//!
//! 创建：创建者为唯一初始 admin，生成 orgId/recoverySecret/orgSecret/组织根
//! 密钥对与 orgAddress（org.md §13/§15），追加 `create` 事务并落库。
//! 删除：admin 校验 + `delete` 事务 + 删记录。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    ORG_MEMBER_PREFIX, OrganizationMember, OrganizationRecord, OrganizationRole,
    OrganizationSyncState, generate_org_secret, generate_organization_id,
    generate_recovery_secret, normalize_plugin_domain, normalize_text, org_member_key,
    organization_key,
};
use super::super::{OrgError, Result, org_address};
use super::{CreateOrganizationInput, OrganizationService};

impl OrganizationService {
    /// `createOrganization`（service.ts:110-150）：创建者为唯一初始 admin，
    /// 生成 orgId/recoverySecret，追加 `create` 事务并落库。
    pub fn create_organization<S: StorageBackend>(
        storage: &mut S,
        input: &CreateOrganizationInput,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        Self::create_organization_impl(storage, input, current_root_id, now_ms, None)
    }

    /// pdsync 感知的 [`Self::create_organization`]：组织记录落库走
    /// [`Self::save_record_pdsync`]（`org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn create_organization_pdsync<S: StorageBackend>(
        storage: &mut S,
        input: &CreateOrganizationInput,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::create_organization_impl(storage, input, current_root_id, now_ms, Some(node_id))
    }

    fn create_organization_impl<S: StorageBackend>(
        storage: &mut S,
        input: &CreateOrganizationInput,
        current_root_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<OrganizationRecord> {
        let name = normalize_text(&input.name, "Organization name")?;
        let description = input
            .description
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        // 组织 logo 可省；空白等同未设置（与 description 归一口径一致），
        // 非空时按 identity::validate_avatar 同口径校验，非法拒绝
        let avatar = input
            .avatar
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                crate::identity::validate_avatar(value)
                    .map(|_| value.to_string())
                    .map_err(|e| OrgError::InvalidAvatar(e.to_string()))
            })
            .transpose()?
            .unwrap_or_default();
        // 基础插件域可省（组织与插件不再强关联，设计 §7.2）；
        // 空白等同未设置（与 description 归一口径一致），非空时校验 `plugin:` 前缀
        let base_plugin_domain = input
            .base_plugin_domain
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(normalize_plugin_domain)
            .transpose()?;

        let mut record = OrganizationRecord {
            org_id: generate_organization_id(),
            name: name.clone(),
            description: description.clone(),
            avatar: avatar.clone(),
            base_plugin_domain: base_plugin_domain.clone(),
            created_at: now_ms,
            created_by: current_root_id.to_string(),
            updated_at: now_ms,
            members: vec![OrganizationMember {
                root_id: current_root_id.to_string(),
                role: OrganizationRole::Admin,
                joined_at: now_ms,
                added_by: current_root_id.to_string(),
                node_info: None,
                nickname: None,
                avatar: None,
                signature: None,
                gender: None,
                region: None,
                use_personal_identity: None,
                access_key: None,
                extra: Default::default(),
            }],
            sync: None,
            // O1：创建时不显式指定角色——缺省推导生效（网关=全员候选、
            // 数据=全体管理员即创建者）
            gateways: Vec::new(),
            data_accounts: Vec::new(),
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        record.set_recovery_secret(generate_recovery_secret());
        // orgSecret（org.md §13）：创建时生成，经 extra 动态键随快照在成员间流动
        record.set_org_secret(generate_org_secret());
        // 组织根密钥对与 orgAddress（org.md §15）：创建时生成独立 Ed25519 密钥对，
        // orgAddress 落记录（保留键）；根私钥加密存 extra（不进快照、不同步出本机）
        let org_root_key = org_address::generate_org_root_signing_key();
        record.org_address = Some(org_address::org_address_from_public_key(
            &org_root_key.verifying_key().to_bytes(),
        ));
        record.set_org_root_secret(org_address::seal_org_root_secret(
            &org_root_key,
            record.org_secret().expect("orgSecret just set"),
        ));

        let mut tx_payload = serde_json::Map::from_iter([
            ("name".to_string(), Value::from(name.clone())),
            ("description".to_string(), Value::from(description.clone())),
        ]);
        if let Some(domain) = base_plugin_domain {
            tx_payload.insert("basePluginDomain".to_string(), Value::from(domain));
        }
        if !avatar.is_empty() {
            tx_payload.insert("avatar".to_string(), Value::from(avatar));
        }
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: record.org_id.clone(),
                type_: OrganizationTransactionType::Create,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: None,
                summary: format!("创建组织 {name}"),
                payload: Some(tx_payload),
            },
        )?;
        record.sync = Some(OrganizationSyncState {
            versions: build_organization_sync_versions(&record, transaction.created_at),
            sections: pick_sync_sections_by_priority(),
            last_synced_at: 0,
        });
        match node_id {
            Some(node_id) => {
                Self::save_record_pdsync(storage, &record, now_ms, node_id)?;
                // O2b 工作项 1：注册内建 all-members 集合（org:structure/
                // org:contacts），声明记录随 orgsync 声明先行同步。（F7：
                // org:invites 已退出 orgsync，不再注册。）
                crate::plugindata::declare_builtin_org_collections(
                    storage,
                    &record.org_id,
                    current_root_id,
                    now_ms,
                    node_id,
                )?;
            }
            None => Self::save_record(storage, &record)?,
        }
        // P1-a 双写（阶段四A分拆）：初始成员（creator）落 org:member 条目。
        // 版本化句柄逐键记账；raw 句柄裸写（对齐 save_record 口径）。
        for member in &record.members {
            storage.put(
                &org_member_key(&record.org_id, &member.root_id),
                &serde_json::to_string(member)?,
            )?;
        }
        Ok(record)
    }

    /// `deleteOrganization`（service.ts:199-214）：admin 校验 + `delete` 事务 + 删记录。
    pub fn delete_organization<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::delete_organization_impl(storage, org_id, current_root_id, now_ms, None)
    }

    /// pdsync 感知的 [`Self::delete_organization`]：删除走
    /// [`Self::delete_record_pdsync`]（tombstone pmeta，删除可经自设备 pdsync 传播）。
    pub fn delete_organization_pdsync<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        Self::delete_organization_impl(storage, org_id, current_root_id, now_ms, Some(node_id))
    }

    fn delete_organization_impl<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<()> {
        let record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;
        append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: org_id.to_string(),
                type_: OrganizationTransactionType::Delete,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: None,
                summary: format!("删除组织 {}", record.name),
                payload: Some(
                    [("orgId".to_string(), Value::from(org_id))]
                        .into_iter()
                        .collect(),
                ),
            },
        )?;
        match node_id {
            Some(node_id) => Self::delete_record_pdsync(storage, org_id, now_ms, node_id)?,
            None => storage.delete(&organization_key(org_id))?,
        }
        // P1-a 双写对称（阶段四A分拆）：成员条目一并删除。版本化句柄上删除
        // 自动墓碑 + org 域 dlog 传播（组织删除对端可收）；raw 句柄裸删。
        let prefix = format!("{ORG_MEMBER_PREFIX}{org_id}:");
        let keys: Vec<String> = storage
            .scan(&crate::storage::ScanOptions::prefix(&prefix))?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        for key in keys {
            storage.delete(&key)?;
        }
        Ok(())
    }
}
