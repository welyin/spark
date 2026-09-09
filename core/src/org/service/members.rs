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

use super::super::genesis::enforce_member_kind;
use super::super::recovery::RecoveryViewItem;
use super::super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    MemberKind, OrganizationDeviceSet, OrganizationMember, OrganizationNodeInfo,
    OrganizationRecord, OrganizationRole, OrganizationSyncState, OrganizationView,
    generate_recovery_secret, normalize_optional_node_info, normalize_root_id, sort_members,
};
use super::super::{OrgError, Result};
use super::{
    OrgIdentityPatch, OrganizationService, clearable_audit, node_info_payload, tri_state_audit,
};

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

    /// pdsync 感知的 [`Self::add_member`]：组织记录落库走原子段原语
    /// [`Self::update_record_atomic`]（F8：提交前重读校验 + 三路合并，
    /// `org:meta` 写 pmeta，可经自设备 pdsync 同步）。
    pub fn add_member_pdsync<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::add_member_mutate(
                storage,
                record,
                org_id,
                member_root_id,
                node_info,
                current_root_id,
                now_ms,
            )?;
            Ok(true)
        })
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
        let mut record = Self::require_organization(storage, org_id)?;
        Self::add_member_mutate(
            storage,
            &mut record,
            org_id,
            member_root_id,
            node_info,
            current_root_id,
            now_ms,
        )?;
        Self::save_record(storage, &record)?;
        Ok(record)
    }

    /// addMember 的纯变更段（F8 拆段）：在调用方给定的记录上校验 + 变更 +
    /// 追加事务 + 重建 sync；不读不写 org:meta 记录本身（读基线/落库由
    /// 调用方——原子段原语或裸写包装——负责）。
    #[allow(clippy::too_many_arguments)]
    fn add_member_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::require_admin(record, current_root_id)?;

        // 成员种类硬规则（org-genesis §3.2）：本路径只录入个人成员——目标域为
        // 共同体域（community）时拒绝；组织成员加入走
        // [`OrganizationService::validate_org_join`] 的跨组织路径。
        enforce_member_kind(record.domain_type.unwrap_or_default(), MemberKind::Person)?;

        let normalized_root_id = normalize_root_id(member_root_id)?;
        let normalized_node_info = normalize_optional_node_info(node_info)?;
        // 端点化：addMember 的入参是单端点，聚合成端点集（按 deviceUid 键）。
        let normalized_set = normalized_node_info
            .as_ref()
            .map(|info| OrganizationDeviceSet::from_single(info.clone()));
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);

        let existing = record
            .members
            .iter()
            .any(|m| m.root_id == normalized_root_id);
        let tx_type;
        let tx_summary;
        if existing {
            // 重复添加 = 更新/合并端点；未提供 nodeInfo 时保留原值（service.ts:223-266）
            if let Some(member) = record
                .members
                .iter_mut()
                .find(|m| m.root_id == normalized_root_id)
                && let Some(incoming) = normalized_node_info.as_ref()
            {
                // 按 deviceUid 聚合：同设备旧 peerId 墓碑化替换（成员表端点化）。
                match member.node_info.as_mut() {
                    Some(set) => {
                        set.upsert(incoming);
                    }
                    None => {
                        member.node_info =
                            Some(OrganizationDeviceSet::from_single(incoming.clone()));
                    }
                }
            }
            tx_type = OrganizationTransactionType::MemberUpdate;
            tx_summary = format!("更新成员节点信息 {normalized_root_id}");
        } else {
            record.members.push(OrganizationMember {
                root_id: normalized_root_id.clone(),
                role: OrganizationRole::Member,
                joined_at: now_ms,
                added_by: current_root_id.to_string(),
                node_info: normalized_set,
                nickname: None,
                avatar: None,
                signature: None,
                gender: None,
                region: None,
                use_personal_identity: None,
                access_key: None,
                kind: None,
                org_binding: None,
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
        Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
        Ok(())
    }

    /// A16（membership §4.4）：发布本机成员的 accessKey（org_user_id 地基）。
    ///
    /// 写一次语义：已存在不覆盖（accessKey 由 `org-access:{orgId}` 域从 seed
    /// 确定性派生，同账号恒同值，重写无意义）；非本组织成员 → `Ok(false)`。
    /// 与 `update_my_identity` 同模式：变更后 bump `updatedAt` 落库，经既有
    /// 快照同步扩散（仅本人可写字段，无需 admin）。
    pub fn publish_access_key<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        access_key: crate::org::types::OrganizationAccessKey,
        now_ms: i64,
    ) -> Result<bool> {
        let mut record = Self::require_organization(storage, org_id)?;
        let Some(index) = record
            .members
            .iter()
            .position(|m| m.root_id == current_root_id)
        else {
            return Ok(false);
        };
        if record.members[index].access_key.is_some() {
            return Ok(false);
        }
        record.members[index].access_key = Some(access_key);
        record.updated_at = now_ms;
        // 双写：whole org:meta（混跑期旧端读）+ per-member 条目（P1 装配视图
        // 的权威源——只写 whole 会被条目覆盖读回 None）。
        Self::save_record(storage, &record)?;
        storage.put(
            &crate::org::types::org_member_key(org_id, current_root_id),
            &serde_json::to_string(&record.members[index])?,
        )?;
        Ok(true)
    }

    /// `updateMyIdentity`：成员更新自己的组织内身份字段（昵称/头像/签名/性别/
    /// 地区/usePersonalIdentity）。仅改调用者本人的成员记录——他人记录不可改；
    /// 无需 admin（任何成员可改自己的身份）。
    ///
    /// - 字段校验复用 identity 资料口径（昵称 24 字符、头像 data URL +
    ///   序列化 200KB、性别 16/地区 64/签名 128 字符）
    /// - 无变化时幂等返回（不 bump 版本），与 `updateOrgInfo` 同口径
    /// - 变更后追加事务并重建 sync，经既有快照同步广播扩散（与 addMember 同模式）
    /// pdsync 感知的 [`Self::update_my_identity`]：组织记录落库走原子段原语
    /// [`Self::update_record_atomic`]（F8；`org:meta` 写 pmeta，可经自设备
    /// pdsync 同步）。
    pub fn update_my_identity_pdsync<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::update_my_identity_mutate(storage, record, org_id, patch, current_root_id, now_ms)
        })
    }

    pub fn update_my_identity<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        if Self::update_my_identity_mutate(
            storage,
            &mut record,
            org_id,
            patch,
            current_root_id,
            now_ms,
        )? {
            Self::save_record(storage, &record)?;
        }
        Ok(record)
    }

    /// `updateMyIdentity` 的纯变更段（F8 拆段）：返回是否发生变更（false =
    /// 幂等无写，不 bump 版本）。
    fn update_my_identity_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        patch: &OrgIdentityPatch,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        // 空域只读档案：共同体全员退出后无人能写入（含成员自写身份字段）。
        Self::require_community_writable(storage, record)?;
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
            return Ok(false);
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
                            patch
                                .nickname
                                .as_deref()
                                .map(Value::from)
                                .unwrap_or(Value::Null),
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
                            patch
                                .use_personal_identity
                                .map(Value::from)
                                .unwrap_or(Value::Null),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
        Ok(true)
    }

    /// `removeMember`（service.ts:460-498）：移除 admin 时若 admin 总数 ≤ 1 拒绝。
    pub fn remove_member<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let mut record = Self::require_organization(storage, org_id)?;
        Self::remove_member_mutate(
            storage,
            &mut record,
            org_id,
            member_root_id,
            current_root_id,
            now_ms,
        )?;
        Self::save_record(storage, &record)?;
        Ok(record)
    }

    /// pdsync 感知的 [`Self::remove_member`]：组织记录落库走原子段原语
    /// [`Self::update_record_atomic`]（F8；`org:meta` 写 pmeta）。
    pub fn remove_member_pdsync<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::remove_member_mutate(
                storage,
                record,
                org_id,
                member_root_id,
                current_root_id,
                now_ms,
            )?;
            Ok(true)
        })
    }

    /// `removeMember` 的纯变更段（F8 拆段）：不读不写 org:meta 记录本身。
    fn remove_member_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        member_root_id: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::require_admin(record, current_root_id)?;
        // 空域只读档案：共同体全员退出后无人能写入（成员移除也是写路径；
        // 组织成员的退出走 leave_community 的留史路径，不经本函数）。
        Self::require_community_writable(storage, record)?;

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
        Self::member_removal_cleanup(storage, record, org_id, &normalized_root_id)?;
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
        Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
        Ok(())
    }

    /// 成员出册后的清理链（remove/leave 共用）：org dlog 水位/已收键清理 +
    /// 管理面邀请投影墓碑化。（A9/A14：gateways/dataAccounts 指定通路已移除，
    /// 活跃集/数据节点推导天然随成员表变化，无需角色列表清理。）
    fn member_removal_cleanup<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        removed_root_id: &str,
    ) -> Result<()> {
        let _ = record;
        // 卫生批项3：已退出成员的 org dlog 水位/已收键（wm/seen，设备粒度）
        // 随出册清理——残留键虽已被 GC 阈值计算排除（F8 修正：min 只取等待
        // 集合），但随成员更替累积。清理失败不阻断（残留仅积累噪音）。
        if let Err(e) =
            crate::sync::orgsync::org_dlog_remove_member_marks(storage, org_id, removed_root_id)
        {
            log::warn!("[ORG] member removal dlog marks cleanup failed: {e}");
        }
        // batch3 §2：成员出册 → 其相关管理面邀请投影（inviter 或 invitee
        // 维度）墓碑化（org 域 dlog 既有机制传播；版本化句柄删除即自动墓碑，
        // raw 句柄为裸删）
        let invpub_prefix = format!("org:invpub:{org_id}:");
        for (key, _) in storage.scan(&crate::storage::ScanOptions::prefix(&invpub_prefix))? {
            let Some(rest) = key.strip_prefix(&invpub_prefix) else {
                continue;
            };
            // 键形 {inviterRoot}:{inviteeRoot}（rootId 无冒号）
            if rest.split(':').any(|seg| seg == removed_root_id) {
                storage.delete(&key)?;
            }
        }
        Ok(())
    }

    /// `leaveOrganization`（A13 / community-model §4.3）：**成员自退出**——
    /// 操作者即目标成员（不需 admin 校验；语义是「我退出」而非「移除他人」）。
    /// 唯一 admin 且还有其他成员时仍拒绝（MustKeepAdmin：须先晋升他人）；
    /// **最后一名成员退出放行**——组织成为空域：成员表为空、历史保留为
    /// 只读档案（无成员即无 admin，写路径自然封死；「域只可退出，不可
    /// 解散」的退出侧落点）。清理链与移除一致（[`Self::member_removal_cleanup`]），
    /// 事务审计 `member-leave`，落库走原子段原语（F8）。
    pub fn leave_organization_pdsync<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<OrganizationRecord> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::leave_organization_mutate(storage, record, org_id, current_root_id, now_ms)?;
            Ok(true)
        })
    }

    /// [`Self::leave_organization_pdsync`] 的纯变更段（F8 拆段）。
    fn leave_organization_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut OrganizationRecord,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        // 空域只读档案：共同体全员退出后无人能写入（自退出也是写路径）
        Self::require_community_writable(storage, record)?;
        let normalized_root_id = normalize_root_id(current_root_id)?;
        let Some(index) = record
            .members
            .iter()
            .position(|m| m.root_id == normalized_root_id)
        else {
            return Err(OrgError::MemberNotFound);
        };
        let member = record.members[index].clone();
        // 唯一 admin 且组织还有其他成员：须先晋升他人（与移除同口径）；
        // 但最后一名成员（单成员组织）退出放行 → 空域
        if member.role == OrganizationRole::Admin
            && record.admin_count() <= 1
            && record.members.len() > 1
        {
            return Err(OrgError::MustKeepAdmin);
        }

        record.members.remove(index);
        Self::member_removal_cleanup(storage, record, org_id, &normalized_root_id)?;
        record.updated_at = now_ms;
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: org_id.to_string(),
                type_: OrganizationTransactionType::MemberLeave,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: Some(normalized_root_id.clone()),
                summary: format!("退出组织 {}", record.name),
                payload: Some(
                    [("leftRole".to_string(), Value::from(member.role.as_str()))]
                        .into_iter()
                        .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
        Ok(())
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
                // 端点化：任一端点有 peerId/address 即算可同步收件人。
                member.node_info.as_ref().is_some_and(|set| {
                    set.iter().any(|info| {
                        info.peer_id
                            .as_deref()
                            .is_some_and(|p| !p.trim().is_empty())
                            || !info.addresses.is_empty()
                    })
                })
            })
            .collect()
    }

    /// `getRecoveryView`（service.ts:158-197）：当前用户为成员的每个组织一条
    /// `{orgId, recoverySecret, memberNodeInfos}`（仅含 addresses 非空的成员）。
    ///
    /// 存量组织缺 recoverySecret 时由 **admin 惰性补齐**（随机 64 hex，bump
    /// updatedAt 后落库，经反熵扩散；非成员角色本轮跳过等待 gossip）。
    /// 落库走原子段原语 [`Self::update_record_atomic`]（F8；`org:meta` 写
    /// pmeta，补齐结果可经自设备 pdsync 同步）。
    pub fn get_recovery_view<S: StorageBackend>(
        storage: &mut S,
        io_lock: &super::OrgMetaWriteLock,
        current_root_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<Vec<RecoveryViewItem>> {
        let _ = node_id; // 记账由中间件完成，参数保留以稳定签名
        let records = Self::read_all_organizations(storage)?;
        let mut view = Vec::new();
        for record in records {
            let Some(self_member) = record.find_member(current_root_id) else {
                continue;
            };
            let self_is_admin = self_member.role == OrganizationRole::Admin;
            let record = if record.recovery_secret().is_none() && self_is_admin {
                // 原子段内重判（并发下可能已被补齐）→ 幂等无写；
                // 非管理员等管理员补齐后经 gossip 获得，本轮跳过
                let org_id = record.org_id.clone();
                Self::update_record_atomic(storage, io_lock, &org_id, |_storage, rec| {
                    if rec.recovery_secret().is_some() {
                        return Ok(false);
                    }
                    rec.set_recovery_secret(generate_recovery_secret());
                    rec.updated_at = now_ms;
                    let previous = rec.sync.clone();
                    rec.sync = Some(OrganizationSyncState {
                        versions: build_organization_sync_versions(
                            rec,
                            previous
                                .as_ref()
                                .map(|s| s.versions.transactions_version)
                                .unwrap_or(rec.updated_at),
                        ),
                        sections: pick_sync_sections_by_priority(),
                        last_synced_at: previous.as_ref().map(|s| s.last_synced_at).unwrap_or(0),
                    });
                    Ok(true)
                })?
            } else {
                if record.recovery_secret().is_none() {
                    continue;
                }
                record
            };
            view.push(RecoveryViewItem {
                org_id: record.org_id.clone(),
                recovery_secret: record.recovery_secret().unwrap_or_default().to_string(),
                // 端点化：展平成员端点集，收集所有带地址的端点。
                member_node_infos: record
                    .members
                    .iter()
                    .filter_map(|m| m.node_info.clone())
                    .flat_map(|set| set.endpoints.into_iter())
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
