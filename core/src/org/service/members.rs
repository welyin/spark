//! 成员增删与各类视图（service.ts `addMember`/`removeMember`/`toView`/
//! `listMine`/`getRecoveryView`/`syncOrganizationToKnownMembers`）。
//!
//! 变更路径统一收尾：追加事务 → [`OrganizationService::rebuild_sync_after_mutation`]
//! → 落库；推送接收方筛选（`sync_recipients`）只读不落库。

use serde_json::Value;

use crate::identity::{
    GENDER_MAX_CHARS, REGION_MAX_CHARS, SIGNATURE_MAX_CHARS, patch_extra_field, validate_avatar,
    validate_nickname,
};
use crate::storage::StorageBackend;

use super::super::recovery::RecoveryViewItem;
use super::super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRecord, OrganizationRole,
    OrganizationSyncState, OrganizationView, generate_recovery_secret,
    normalize_optional_node_info, normalize_root_id, sort_members,
};
use super::super::{OrgError, Result};
use super::{OrganizationService, OrgIdentityPatch, clearable_audit, node_info_payload, tri_state_audit};

impl OrganizationService {
    /// `toView`（service.ts:573-587）：成员排序（admin 优先，joinedAt 升序）+
    /// 角色/计数。
    pub fn to_view(record: &OrganizationRecord, current_root_id: &str) -> OrganizationView {
        let members = sort_members(&record.members);
        let current_role = members
            .iter()
            .find(|m| m.root_id == current_root_id)
            .map(|m| m.role);
        OrganizationView {
            members,
            current_user_role: current_role,
            is_current_user_admin: current_role == Some(OrganizationRole::Admin),
            member_count: record.members.len(),
            admin_count: record.admin_count(),
            record: super::super::types::OrganizationRecordFlattened {
                org_id: record.org_id.clone(),
                name: record.name.clone(),
                description: record.description.clone(),
                avatar: record.avatar.clone(),
                base_plugin_domain: record.base_plugin_domain.clone().unwrap_or_default(),
                created_at: record.created_at,
                created_by: record.created_by.clone(),
                updated_at: record.updated_at,
                sync: record.sync.clone(),
                gateways: record.gateways.clone(),
                org_address: record.org_address.clone(),
                is_public: record.is_public,
                extra: record.extra.clone(),
            },
        }
    }

    /// `listMine`：当前用户为成员的组织视图，按 `updatedAt` 降序。
    pub fn list_mine<S: StorageBackend>(
        storage: &S,
        current_root_id: &str,
    ) -> Result<Vec<OrganizationView>> {
        let records = Self::read_all_organizations(storage)?;
        let mut views: Vec<OrganizationView> = records
            .iter()
            .filter(|r| r.members.iter().any(|m| m.root_id == current_root_id))
            .map(|r| Self::to_view(r, current_root_id))
            .collect();
        views.sort_by_key(|view| std::cmp::Reverse(view.record.updated_at));
        Ok(views)
    }

    /// `addMember`（service.ts:216-309，网络推送部分除外）：
    /// - rootId 规范化后查重；重复添加视为"更新 nodeInfo"（未提供时保留原值）
    /// - 新成员 role 固定 `member`
    /// - 需要当前用户为 admin
    ///
    /// 与 TS 的差异：TS 要求 syncContext 已配置（否则抛错）且先推送后落库；
    /// 本层只落库，推送由调用方用 [`Self::sync_recipients`] 的结果执行。
    pub fn add_member<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        Self::add_member_impl(
            storage,
            org_id,
            member_root_id,
            node_info,
            current_root_id,
            now_ms,
            None,
        )
    }

    /// pdsync 感知的 [`Self::add_member`]：组织记录落库走
    /// [`Self::save_record_pdsync`]（`org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn add_member_pdsync<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::add_member_impl(
            storage,
            org_id,
            member_root_id,
            node_info,
            current_root_id,
            now_ms,
            Some(node_id),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_member_impl<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
        current_root_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let normalized_root_id = normalize_root_id(member_root_id)?;
        let normalized_node_info = normalize_optional_node_info(node_info)?;
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);

        let existing = record
            .members
            .iter()
            .any(|m| m.root_id == normalized_root_id);
        let tx_type;
        let tx_summary;
        if existing {
            // 重复添加 = 更新 nodeInfo；未提供 nodeInfo 时保留原值（service.ts:223-266）
            if let Some(member) = record
                .members
                .iter_mut()
                .find(|m| m.root_id == normalized_root_id)
                && normalized_node_info.is_some()
            {
                member.node_info = normalized_node_info.clone();
            }
            tx_type = OrganizationTransactionType::MemberUpdate;
            tx_summary = format!("更新成员节点信息 {normalized_root_id}");
        } else {
            record.members.push(OrganizationMember {
                root_id: normalized_root_id.clone(),
                role: OrganizationRole::Member,
                joined_at: now_ms,
                added_by: current_root_id.to_string(),
                node_info: normalized_node_info.clone(),
                nickname: None,
                avatar: None,
                signature: None,
                gender: None,
                region: None,
                use_personal_identity: None,
                extra: Default::default(),
            });
            tx_type = OrganizationTransactionType::MemberAdd;
            tx_summary = format!("添加成员 {normalized_root_id}");
        }
        record.updated_at = now_ms;

        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: org_id.to_string(),
                type_: tx_type,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: Some(normalized_root_id),
                summary: tx_summary,
                payload: Some(node_info_payload(normalized_node_info.as_ref())),
            },
        )?;
        Self::rebuild_sync_after_mutation(
            &mut record,
            previous_last_synced_at,
            transaction.created_at,
        );
        match node_id {
            Some(node_id) => Self::save_record_pdsync(storage, &record, now_ms, node_id)?,
            None => Self::save_record(storage, &record)?,
        }
        Ok(record)
    }

    /// `updateMyIdentity`：成员更新自己的组织内身份字段（昵称/头像/签名/性别/
    /// 地区/usePersonalIdentity）。仅改调用者本人的成员记录——他人记录不可改；
    /// 无需 admin（任何成员可改自己的身份）。
    ///
    /// - 字段校验复用 identity 资料口径（昵称 24 字符、头像 data URL +
    ///   序列化 200KB、性别 16/地区 64/签名 128 字符）
    /// - 无变化时幂等返回（不 bump 版本），与 `updateOrgInfo` 同口径
    /// - 变更后追加事务并重建 sync，经既有快照同步广播扩散（与 addMember 同模式）
    pub fn update_my_identity<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        Self::update_my_identity_impl(storage, org_id, patch, current_root_id, now_ms, None)
    }

    /// pdsync 感知的 [`Self::update_my_identity`]：组织记录落库走
    /// [`Self::save_record_pdsync`]（`org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn update_my_identity_pdsync<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::update_my_identity_impl(storage, org_id, patch, current_root_id, now_ms, Some(node_id))
    }

    fn update_my_identity_impl<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        let Some(index) = record
            .members
            .iter()
            .position(|m| m.root_id == current_root_id)
        else {
            return Err(OrgError::MemberNotFound);
        };
        let member = &record.members[index];
        let values = resolve_identity_patch(member, patch)?;

        // 幂等：全部字段无变化时不 bump 版本
        let unchanged = values.nickname == member.nickname
            && values.avatar == member.avatar
            && values.gender == member.gender
            && values.region == member.region
            && values.signature == member.signature
            && values.use_personal_identity == member.use_personal_identity;
        if unchanged {
            return Ok(record);
        }

        let member = &mut record.members[index];
        member.nickname = values.nickname;
        member.avatar = values.avatar;
        member.gender = values.gender;
        member.region = values.region;
        member.signature = values.signature;
        member.use_personal_identity = values.use_personal_identity;
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
                summary: "更新组织身份信息".to_string(),
                payload: Some(
                    [
                        (
                            "nickname".to_string(),
                            patch.nickname.as_deref().map(Value::from).unwrap_or(Value::Null),
                        ),
                        // avatar/gender/region/signature 变更纳入审计，但只记
                        // 摘要（不变 Null / 清除 false / 设置记长度），不落完整内容
                        (
                            "avatar".to_string(),
                            tri_state_audit(patch.avatar.as_ref().map(|inner| inner.as_deref())),
                        ),
                        (
                            "gender".to_string(),
                            clearable_audit(patch.gender.as_deref()),
                        ),
                        (
                            "region".to_string(),
                            clearable_audit(patch.region.as_deref()),
                        ),
                        (
                            "signature".to_string(),
                            clearable_audit(patch.signature.as_deref()),
                        ),
                        (
                            "usePersonalIdentity".to_string(),
                            patch.use_personal_identity.map(Value::from).unwrap_or(Value::Null),
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
        match node_id {
            Some(node_id) => Self::save_record_pdsync(storage, &record, now_ms, node_id)?,
            None => Self::save_record(storage, &record)?,
        }
        Ok(record)
    }

    /// `removeMember`（service.ts:460-498）：移除 admin 时若 admin 总数 ≤ 1 拒绝。
    pub fn remove_member<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        Self::remove_member_impl(storage, org_id, member_root_id, current_root_id, now_ms, None)
    }

    /// pdsync 感知的 [`Self::remove_member`]：组织记录落库走
    /// [`Self::save_record_pdsync`]（`org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn remove_member_pdsync<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::remove_member_impl(
            storage,
            org_id,
            member_root_id,
            current_root_id,
            now_ms,
            Some(node_id),
        )
    }

    fn remove_member_impl<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let normalized_root_id = normalize_root_id(member_root_id)?;
        let Some(index) = record
            .members
            .iter()
            .position(|m| m.root_id == normalized_root_id)
        else {
            return Err(OrgError::MemberNotFound);
        };
        let member = record.members[index].clone();
        if member.role == OrganizationRole::Admin && record.admin_count() <= 1 {
            return Err(OrgError::MustKeepAdmin);
        }

        record.members.remove(index);
        record.updated_at = now_ms;
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: org_id.to_string(),
                type_: OrganizationTransactionType::MemberRemove,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: Some(normalized_root_id.clone()),
                summary: format!("移除成员 {normalized_root_id}"),
                payload: Some(
                    [("removedRole".to_string(), Value::from(member.role.as_str()))]
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
        match node_id {
            Some(node_id) => Self::save_record_pdsync(storage, &record, now_ms, node_id)?,
            None => Self::save_record(storage, &record)?,
        }
        Ok(record)
    }

    /// 变更后需要推送快照的接收方（`syncOrganizationToKnownMembers` 的筛选逻辑，
    /// service.ts:537-551）：排除操作者本人，要求 nodeInfo 有 peerId 或 addresses。
    pub fn sync_recipients<'a>(
        record: &'a OrganizationRecord,
        actor_root_id: &str,
    ) -> Vec<&'a OrganizationMember> {
        record
            .members
            .iter()
            .filter(|member| {
                if member.root_id == actor_root_id {
                    return false;
                }
                member.node_info.as_ref().is_some_and(|info| {
                    info.peer_id
                        .as_deref()
                        .is_some_and(|p| !p.trim().is_empty())
                        || !info.addresses.is_empty()
                })
            })
            .collect()
    }

    /// `getRecoveryView`（service.ts:158-197）：当前用户为成员的每个组织一条
    /// `{orgId, recoverySecret, memberNodeInfos}`（仅含 addresses 非空的成员）。
    ///
    /// 存量组织缺 recoverySecret 时由 **admin 惰性补齐**（随机 64 hex，bump
    /// updatedAt 后落库，经反熵扩散；非成员角色本轮跳过等待 gossip）。
    /// 落库走 [`Self::save_record_pdsync`]（`org:meta` 写 pmeta，补齐结果可经
    /// 自设备 pdsync 同步）。
    pub fn get_recovery_view<S: StorageBackend>(
        storage: &mut S,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<Vec<RecoveryViewItem>> {
        let records = Self::read_all_organizations(storage)?;
        let mut view = Vec::new();
        for mut record in records {
            let Some(self_member) = record.find_member(current_root_id) else {
                continue;
            };
            let self_is_admin = self_member.role == OrganizationRole::Admin;
            if record.recovery_secret().is_none() {
                // 非管理员等管理员补齐后经 gossip 获得；本轮先跳过
                if !self_is_admin {
                    continue;
                }
                record.set_recovery_secret(generate_recovery_secret());
                record.updated_at = now_ms;
                let previous = record.sync.clone();
                record.sync = Some(OrganizationSyncState {
                    versions: build_organization_sync_versions(
                        &record,
                        previous
                            .as_ref()
                            .map(|s| s.versions.transactions_version)
                            .unwrap_or(record.updated_at),
                    ),
                    sections: pick_sync_sections_by_priority(),
                    last_synced_at: previous.as_ref().map(|s| s.last_synced_at).unwrap_or(0),
                });
                Self::save_record_pdsync(storage, &record, now_ms, node_id)?;
            }
            view.push(RecoveryViewItem {
                org_id: record.org_id.clone(),
                recovery_secret: record.recovery_secret().unwrap_or_default().to_string(),
                member_node_infos: record
                    .members
                    .iter()
                    .filter_map(|m| m.node_info.clone())
                    .filter(|info| !info.addresses.is_empty())
                    .collect(),
            });
        }
        Ok(view)
    }
}

/// `update_my_identity` 的字段计算结果（校验已全部通过）。
struct IdentityPatchValues {
    nickname: Option<String>,
    avatar: Option<String>,
    gender: Option<String>,
    region: Option<String>,
    signature: Option<String>,
    use_personal_identity: Option<bool>,
}

/// 按补丁语义计算成员身份新值（复用 identity 的校验函数与常量；
/// 全部校验通过后才返回，不落库）。
fn resolve_identity_patch(
    member: &OrganizationMember,
    patch: &OrgIdentityPatch,
) -> Result<IdentityPatchValues> {
    let nickname = match &patch.nickname {
        Some(value) => Some(
            validate_nickname(value).map_err(|e| OrgError::InvalidIdentityField(e.to_string()))?,
        ),
        None => member.nickname.clone(),
    };
    let avatar = match &patch.avatar {
        Some(Some(value)) => {
            validate_avatar(value).map_err(|e| OrgError::InvalidIdentityField(e.to_string()))?;
            Some(value.clone())
        }
        Some(None) => None,
        None => member.avatar.clone(),
    };
    let map_field_err =
        |e: crate::identity::IdentityError| OrgError::InvalidIdentityField(e.to_string());
    let gender = patch_extra_field(
        member.gender.clone(),
        patch.gender.as_deref(),
        "gender",
        GENDER_MAX_CHARS,
    )
    .map_err(map_field_err)?;
    let region = patch_extra_field(
        member.region.clone(),
        patch.region.as_deref(),
        "region",
        REGION_MAX_CHARS,
    )
    .map_err(map_field_err)?;
    let signature = patch_extra_field(
        member.signature.clone(),
        patch.signature.as_deref(),
        "signature",
        SIGNATURE_MAX_CHARS,
    )
    .map_err(map_field_err)?;
    let use_personal_identity = patch.use_personal_identity.or(member.use_personal_identity);
    Ok(IdentityPatchValues {
        nickname,
        avatar,
        gender,
        region,
        signature,
        use_personal_identity,
    })
}
