//! 组织服务层（对齐 desktop/src/main/organization/service.ts）。
//!
//! 纯逻辑层：只操作 [`StorageBackend`]，不触碰网络。TS 中注入的
//! `syncContext`（推送快照）与 `inviteContext.connectAndPull`（连接拉取）
//! 属 p2p 模块职责，本层以返回值/参数形式对接：
//! - 成员变更后需要推送的接收方集合由 [`OrganizationService::sync_recipients`] 给出
//! - 邀请码接受的连接/拉取由调用方完成，随后用
//!   [`OrganizationService::check_invite_accepted`] 做落库确认
//!
//! 时间（`Date.now()`）一律以 `now_ms` 参数注入，保证纯函数可测。

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

use super::claim::{NodeInfoClaim, verify_node_info_claim};
use super::invite::{
    OrgInviteInviter, OrgInvitePayload, decode_org_invite_at, encode_org_invite,
};
use super::recovery::RecoveryViewItem;
use super::snapshot::{
    build_organization_sync_versions, merge_organization_sync_snapshot,
    normalize_incoming_snapshot, pick_sync_sections_by_priority,
};
use super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::types::{
    ORG_META_PREFIX, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
    OrganizationRole, OrganizationSyncState, OrganizationView,
    generate_organization_id, generate_recovery_secret,
    normalize_optional_node_info, normalize_plugin_domain, normalize_root_id, normalize_text,
    organization_key, sort_members,
};
use super::{OrgError, Result};

/// 创建组织输入（types.ts:95-99）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateOrganizationInput {
    /// 组织名（trim + 连续空白归一）。
    pub name: String,
    /// 描述（trim，可省）。
    pub description: Option<String>,
    /// 基础插件域（`plugin:` 前缀，必填）。
    pub base_plugin_domain: String,
}

/// `createOrgInvite` 的返回（service.ts:315-339）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedOrgInvite {
    /// 邀请码（base64url）。
    pub invite: String,
    /// 组织 id。
    pub org_id: String,
    /// 组织名。
    pub org_name: String,
}

/// `acceptOrgInvite` 成功确认后的返回（service.ts:373）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteAcceptance {
    /// 组织 id。
    pub org_id: String,
    /// 组织名。
    pub org_name: String,
    /// 成员数。
    pub member_count: usize,
}

/// 组织服务（无状态；全部方法以存储与参数为输入）。
pub struct OrganizationService;

impl OrganizationService {
    /// 读取单个组织记录；不存在返回 `Ok(None)`。
    pub fn get_record<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<Option<OrganizationRecord>> {
        let Some(raw) = storage.get(&organization_key(org_id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 读取全部组织记录（`org:meta:` 前缀扫描，键升序）。
    ///
    /// 对齐 TS `readAllOrganizations`：损坏 JSON 直接报错（不静默跳过）。
    pub fn read_all_organizations<S: StorageBackend>(
        storage: &S,
    ) -> Result<Vec<OrganizationRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(ORG_META_PREFIX))?;
        rows.into_iter()
            .map(|(_, value)| serde_json::from_str(&value).map_err(OrgError::from))
            .collect()
    }

    /// 持久化记录到 `org:meta:<orgId>`。
    pub fn save_record<S: StorageBackend>(storage: &mut S, record: &OrganizationRecord) -> Result<()> {
        storage.put(
            &organization_key(&record.org_id),
            &serde_json::to_string(record)?,
        )?;
        Ok(())
    }

    fn require_organization<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::get_record(storage, org_id)?.ok_or(OrgError::OrganizationNotFound)
    }

    fn require_admin(record: &OrganizationRecord, root_id: &str) -> Result<()> {
        if !record.is_admin(root_id) {
            return Err(OrgError::AdminRequired);
        }
        Ok(())
    }

    /// 成员变更后的 sync 重建（service.ts 各变更路径的公共收尾）：
    /// `versions = build(record, tx.createdAt)`、`sections = pickSyncSectionsByPriority`、
    /// `lastSyncedAt` 保留原值（无则 0）。
    fn rebuild_sync_after_mutation(
        record: &mut OrganizationRecord,
        previous_last_synced_at: i64,
        transaction_created_at: i64,
    ) {
        record.sync = Some(OrganizationSyncState {
            versions: build_organization_sync_versions(record, transaction_created_at),
            sections: pick_sync_sections_by_priority(),
            last_synced_at: previous_last_synced_at,
        });
    }

    /// `toView`（service.ts:573-587）：成员排序（admin 优先，joinedAt 升序）+
    /// 角色/计数；`basePluginDomain` 缺省归一为 `""`。
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
            record: super::types::OrganizationRecordFlattened {
                org_id: record.org_id.clone(),
                name: record.name.clone(),
                description: record.description.clone(),
                base_plugin_domain: record.base_plugin_domain.clone().unwrap_or_default(),
                created_at: record.created_at,
                created_by: record.created_by.clone(),
                updated_at: record.updated_at,
                sync: record.sync.clone(),
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

    /// `createOrganization`（service.ts:110-150）：创建者为唯一初始 admin，
    /// 生成 orgId/recoverySecret，追加 `create` 事务并落库。
    pub fn create_organization<S: StorageBackend>(
        storage: &mut S,
        input: &CreateOrganizationInput,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let name = normalize_text(&input.name, "Organization name")?;
        let description = input
            .description
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let base_plugin_domain = normalize_plugin_domain(&input.base_plugin_domain)?;

        let mut record = OrganizationRecord {
            org_id: generate_organization_id(),
            name: name.clone(),
            description: description.clone(),
            base_plugin_domain: Some(base_plugin_domain.clone()),
            created_at: now_ms,
            created_by: current_root_id.to_string(),
            updated_at: now_ms,
            members: vec![OrganizationMember {
                root_id: current_root_id.to_string(),
                role: OrganizationRole::Admin,
                joined_at: now_ms,
                added_by: current_root_id.to_string(),
                node_info: None,
                extra: Default::default(),
            }],
            sync: None,
            extra: Default::default(),
        };
        record.set_recovery_secret(generate_recovery_secret());

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
                payload: Some(
                    [
                        ("name".to_string(), Value::from(name)),
                        ("description".to_string(), Value::from(description)),
                        ("basePluginDomain".to_string(), Value::from(base_plugin_domain)),
                    ]
                    .into_iter()
                    .collect(),
                ),
            },
        )?;
        record.sync = Some(OrganizationSyncState {
            versions: build_organization_sync_versions(&record, transaction.created_at),
            sections: pick_sync_sections_by_priority(),
            last_synced_at: 0,
        });
        Self::save_record(storage, &record)?;
        Ok(record)
    }

    /// `deleteOrganization`（service.ts:199-214）：admin 校验 + `delete` 事务 + 删记录。
    pub fn delete_organization<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        now_ms: i64,
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
        storage.delete(&organization_key(org_id))?;
        Ok(())
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
        Self::rebuild_sync_after_mutation(&mut record, previous_last_synced_at, transaction.created_at);
        Self::save_record(storage, &record)?;
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
                    [(
                        "removedRole".to_string(),
                        Value::from(member.role.as_str()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            },
        )?;
        Self::rebuild_sync_after_mutation(&mut record, previous_last_synced_at, transaction.created_at);
        Self::save_record(storage, &record)?;
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
                    info.peer_id.as_deref().is_some_and(|p| !p.trim().is_empty())
                        || !info.addresses.is_empty()
                })
            })
            .collect()
    }

    /// `createOrgInvite`（service.ts:315-339）：仅 admin；邀请人节点信息归一化
    /// （peerId/addresses 至少其一，否则报"本机 P2P 节点尚未启动"）。
    pub fn create_org_invite<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        current_root_id: &str,
        local_peer_id: Option<&str>,
        local_addresses: &[String],
        now_ms: i64,
    ) -> Result<CreatedOrgInvite> {
        let record = Self::require_organization(storage, org_id)?;
        Self::require_admin(&record, current_root_id)?;

        let peer_id = local_peer_id
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let addresses: Vec<String> = local_addresses
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        if peer_id.is_none() && addresses.is_empty() {
            return Err(OrgError::NetworkUnavailable);
        }

        let payload = OrgInvitePayload::new(
            record.org_id.clone(),
            record.name.clone(),
            OrgInviteInviter {
                root_id: current_root_id.to_string(),
                peer_id,
                addresses,
            },
            now_ms,
        );
        Ok(CreatedOrgInvite {
            invite: encode_org_invite(&payload),
            org_id: record.org_id,
            org_name: record.name,
        })
    }

    /// `acceptOrgInvite` 的前半段（service.ts:345-351）：解码校验 + 拒绝自邀。
    ///
    /// 之后的 `connectAndPull`（连接邀请人并反熵拉取，可捎带自签 nodeInfoClaim）
    /// 属网络层；拉取完成后调 [`Self::check_invite_accepted`] 确认。
    pub fn prepare_accept_invite(
        code: &str,
        current_root_id: &str,
        now_ms: i64,
    ) -> Result<OrgInvitePayload> {
        let payload = decode_org_invite_at(code, now_ms)?;
        if payload.inviter.root_id == current_root_id {
            return Err(OrgError::SelfInvite);
        }
        Ok(payload)
    }

    /// `acceptOrgInvite` 的落库确认（service.ts:365-373）：记录存在且自己为
    /// 成员才算加入成功（邀请码本身不是加入凭证，成员资格在拉取侧校验）。
    pub fn check_invite_accepted<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        current_root_id: &str,
    ) -> Result<InviteAcceptance> {
        let record = Self::get_record(storage, org_id)?;
        let Some(record) = record else {
            return Err(OrgError::NotJoined);
        };
        if !record.members.iter().any(|m| m.root_id == current_root_id) {
            return Err(OrgError::NotJoined);
        }
        Ok(InviteAcceptance {
            org_id: record.org_id,
            org_name: record.name,
            member_count: record.members.len(),
        })
    }

    /// `applyNodeInfoClaim`（service.ts:381-458，网络推送部分除外）。
    ///
    /// 落库三条件（org.md §14.5）：claim 校验通过 + 本机当前用户是该组织
    /// admin + 声明者是该组织成员；不满足的组织静默跳过。
    /// 与现有 nodeInfo 完全一致时不 bump 版本。返回落库的组织 id 列表。
    ///
    /// 前置闸说明：TS 入口侧 org-pull-list 只在 requesterRootId 是本地某组织
    /// 已知成员时才处理其 claim（org-pull-sync.ts:165-184）——该判定在 p2p 层。
    pub fn apply_node_info_claim<S: StorageBackend>(
        storage: &mut S,
        claim: &NodeInfoClaim,
        current_root_id: &str,
        remote_peer_id: Option<&str>,
        now_ms: i64,
    ) -> Result<Vec<String>> {
        if !verify_node_info_claim(claim, now_ms).is_ok() {
            return Ok(Vec::new());
        }
        // 防代填他人地址：连接层 peerId 与声明 peerId 必须一致
        if let (Some(remote), Some(claimed)) = (remote_peer_id, claim.node_info.peer_id.as_deref())
            && claimed != remote
        {
            return Ok(Vec::new());
        }
        let claim_root_id = claim.root_id.trim().to_lowercase();
        // 对齐 TS：归一化失败（如 peerId < 8 字符）按异常上抛
        let Some(claimed_node_info) =
            normalize_optional_node_info(Some(&claim.node_info))?
        else {
            return Ok(Vec::new());
        };

        let mut applied = Vec::new();
        let records = Self::read_all_organizations(storage)?;
        for record in records {
            if !record.is_admin(current_root_id) {
                continue;
            }
            let Some(member) = record.find_member(&claim_root_id) else {
                continue;
            };
            let unchanged = member
                .node_info
                .as_ref()
                .and_then(|n| n.peer_id.as_deref())
                == claimed_node_info.peer_id.as_deref()
                && member
                    .node_info
                    .as_ref()
                    .map(|n| n.addresses.clone())
                    .unwrap_or_default()
                    == claimed_node_info.addresses;
            if unchanged {
                continue;
            }

            let mut updated = record.clone();
            for m in &mut updated.members {
                if m.root_id == claim_root_id {
                    m.node_info = Some(claimed_node_info.clone());
                }
            }
            updated.updated_at = now_ms;
            let previous_last_synced_at =
                updated.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);
            let transaction = append_organization_transaction(
                storage,
                OrganizationTransactionRecord {
                    tx_id: String::new(),
                    org_id: updated.org_id.clone(),
                    type_: OrganizationTransactionType::MemberUpdate,
                    created_at: now_ms,
                    actor_root_id: claim_root_id.clone(),
                    target_root_id: Some(claim_root_id.clone()),
                    summary: format!(
                        "成员节点地址自动回填 {}",
                        &claim_root_id[..8.min(claim_root_id.len())]
                    ),
                    payload: Some(
                        [
                            (
                                "nodeInfo".to_string(),
                                serde_json::to_value(&claimed_node_info)?,
                            ),
                            ("source".to_string(), Value::from("node-info-claim")),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                },
            )?;
            Self::rebuild_sync_after_mutation(
                &mut updated,
                previous_last_synced_at,
                transaction.created_at,
            );
            Self::save_record(storage, &updated)?;
            applied.push(updated.org_id.clone());
        }
        Ok(applied)
    }

    /// `getRecoveryView`（service.ts:158-197）：当前用户为成员的每个组织一条
    /// `{orgId, recoverySecret, memberNodeInfos}`（仅含 addresses 非空的成员）。
    ///
    /// 存量组织缺 recoverySecret 时由 **admin 惰性补齐**（随机 64 hex，bump
    /// updatedAt 后落库，经反熵扩散；非成员角色本轮跳过等待 gossip）。
    pub fn get_recovery_view<S: StorageBackend>(
        storage: &mut S,
        current_root_id: &str,
        now_ms: i64,
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
                Self::save_record(storage, &record)?;
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

    /// 接收侧快照落库（org.md §7.4：`normalizeIncomingSnapshot` → `merge` → 写
    /// `org:meta:<orgId>`）。接受两种线形（原始记录 / 重建快照）。
    ///
    /// 定向投递校验（targetRootId 匹配、本机在成员列表）在 p2p 层完成。
    pub fn apply_incoming_snapshot<S: StorageBackend>(
        storage: &mut S,
        organization: &Value,
        now_ms: i64,
    ) -> Result<OrganizationRecord> {
        let snapshot = normalize_incoming_snapshot(organization)?;
        let existing = Self::get_record(storage, &snapshot.org_id)?;
        let merged = merge_organization_sync_snapshot(existing.as_ref(), &snapshot, now_ms);
        Self::save_record(storage, &merged)?;
        Ok(merged)
    }
}

/// 事务 payload 的 `nodeInfo` 键：未提供时整个键缺省（对齐 TS
/// `{nodeInfo: undefined}` 被 `JSON.stringify` 丢弃的行为）。
fn node_info_payload(node_info: Option<&OrganizationNodeInfo>) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Some(info) = node_info
        && let Ok(value) = serde_json::to_value(info)
    {
        map.insert("nodeInfo".to_string(), value);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{derive_root_identity, parse_mnemonic};
    use crate::org::claim::sign_node_info_claim;
    use crate::org::types::OrganizationSyncVersions;
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_720_000_000_000;
    const MNEMONIC: &str = "与 祝 产 鸡 永 烂 施 师 蓝 荷 有 邓 朗 防 管 李 原 芳 饿 万 措 走 腰 旅";
    const MNEMONIC2: &str = "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will";

    fn root_id_of(mnemonic: &str) -> String {
        let parsed = parse_mnemonic(mnemonic).unwrap();
        derive_root_identity(&parsed.seed).id()
    }

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn input() -> CreateOrganizationInput {
        CreateOrganizationInput {
            name: "  星火   组织 ".to_string(),
            description: Some(" 描述 ".to_string()),
            base_plugin_domain: " plugin:chat ".to_string(),
        }
    }

    fn setup_org(storage: &mut MemoryStorage) -> (String, OrganizationRecord) {
        let admin = root_id_of(MNEMONIC);
        let record = OrganizationService::create_organization(storage, &input(), &admin, NOW).unwrap();
        (admin, record)
    }

    #[test]
    fn create_organization_normalizes_and_persists() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        assert_eq!(record.name, "星火 组织");
        assert_eq!(record.description, "描述");
        assert_eq!(record.base_plugin_domain.as_deref(), Some("plugin:chat"));
        assert!(record.org_id.starts_with("org_") && record.org_id.len() == 20);
        assert_eq!(record.recovery_secret().map(str::len), Some(64));
        assert_eq!(record.members.len(), 1);
        assert_eq!(record.members[0].role, OrganizationRole::Admin);
        assert_eq!(record.members[0].root_id, admin);
        assert_eq!(record.created_at, NOW);
        assert_eq!(record.updated_at, NOW);
        // sync：versions 的 transactionsVersion 取 create 事务 createdAt
        let sync = record.sync.as_ref().unwrap();
        assert_eq!(sync.versions.summary_version, NOW);
        assert_eq!(sync.versions.transactions_version, NOW);
        assert_eq!(sync.last_synced_at, 0);
        // 落库可读回（字节一致）
        let loaded = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        assert_eq!(loaded, record);
        // create 事务已写入
        let txs = crate::org::tx::list_organization_transactions(&storage, &record.org_id, 20).unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].type_, OrganizationTransactionType::Create);
        assert_eq!(txs[0].summary, "创建组织 星火 组织");
    }

    #[test]
    fn create_organization_validates_input() {
        let mut storage = MemoryStorage::new();
        let admin = rid('a');
        let mut bad = input();
        bad.name = "   ".to_string();
        assert!(matches!(
            OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
            Err(OrgError::Required(label)) if label == "Organization name"
        ));
        let mut bad = input();
        bad.base_plugin_domain = "chat".to_string();
        assert!(matches!(
            OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
            Err(OrgError::InvalidBasePluginDomain)
        ));
        let mut bad = input();
        bad.base_plugin_domain = "   ".to_string();
        assert!(matches!(
            OrganizationService::create_organization(&mut storage, &bad, &admin, NOW),
            Err(OrgError::Required(label)) if label == "Base plugin"
        ));
    }

    #[test]
    fn add_member_new_and_repeat_update() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        let member_id = root_id_of(MNEMONIC2);
        let node = OrganizationNodeInfo {
            peer_id: Some("12D3KooWMember".to_string()),
            addresses: vec!["/ip4/1.1.1.1/tcp/1".to_string()],
        };
        // 新成员：role 固定 member
        let updated = OrganizationService::add_member(
            &mut storage, &record.org_id, &member_id, Some(&node), &admin, NOW + 1,
        ).unwrap();
        assert_eq!(updated.members.len(), 2);
        let m = updated.find_member(&member_id).unwrap();
        assert_eq!(m.role, OrganizationRole::Member);
        assert_eq!(m.added_by, admin);
        assert_eq!(m.joined_at, NOW + 1);
        assert_eq!(updated.updated_at, NOW + 1);
        let sync = updated.sync.as_ref().unwrap();
        assert_eq!(sync.versions.members_version, NOW + 1);

        // 重复添加不带 nodeInfo → 保留原值，记 member-update
        let updated2 = OrganizationService::add_member(
            &mut storage, &record.org_id, &member_id, None, &admin, NOW + 2,
        ).unwrap();
        let m2 = updated2.find_member(&member_id).unwrap();
        assert_eq!(m2.node_info.as_ref().unwrap().peer_id.as_deref(), Some("12D3KooWMember"));
        assert_eq!(updated2.members.len(), 2);

        let txs = crate::org::tx::list_organization_transactions(&storage, &record.org_id, 20).unwrap();
        let types: Vec<_> = txs.iter().map(|t| t.type_).collect();
        assert_eq!(
            types,
            vec![
                OrganizationTransactionType::MemberUpdate,
                OrganizationTransactionType::MemberAdd,
                OrganizationTransactionType::Create,
            ]
        );
        assert_eq!(txs[0].summary, format!("更新成员节点信息 {member_id}"));
        assert_eq!(txs[1].summary, format!("添加成员 {member_id}"));
        // payload 的 nodeInfo 键：未提供时缺省
        assert!(!txs[0].payload.as_ref().unwrap().contains_key("nodeInfo"));
        assert!(txs[1].payload.as_ref().unwrap().contains_key("nodeInfo"));
    }

    #[test]
    fn add_member_requires_admin_and_valid_root() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        let member_id = root_id_of(MNEMONIC2);
        // 非 admin
        assert!(matches!(
            OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &rid('x'), NOW),
            Err(OrgError::AdminRequired)
        ));
        // rootId 非法
        assert!(matches!(
            OrganizationService::add_member(&mut storage, &record.org_id, "zz", None, &admin, NOW),
            Err(OrgError::InvalidMemberRootId)
        ));
        // 组织不存在
        assert!(matches!(
            OrganizationService::add_member(&mut storage, "org_nope", &member_id, None, &admin, NOW),
            Err(OrgError::OrganizationNotFound)
        ));
    }

    #[test]
    fn remove_member_admin_guard() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        // 移除唯一 admin → 拒绝
        assert!(matches!(
            OrganizationService::remove_member(&mut storage, &record.org_id, &admin, &admin, NOW),
            Err(OrgError::MustKeepAdmin)
        ));
        // 移除不存在的成员（rootId 合法但不在组织中）
        assert!(matches!(
            OrganizationService::remove_member(&mut storage, &record.org_id, &rid('f'), &admin, NOW),
            Err(OrgError::MemberNotFound)
        ));
        // 正常移除
        let member_id = root_id_of(MNEMONIC2);
        OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW + 1).unwrap();
        let updated = OrganizationService::remove_member(&mut storage, &record.org_id, &member_id, &admin, NOW + 2).unwrap();
        assert_eq!(updated.members.len(), 1);
        let txs = crate::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
        assert_eq!(txs[0].type_, OrganizationTransactionType::MemberRemove);
        assert_eq!(txs[0].payload.as_ref().unwrap()["removedRole"], "member");
    }

    #[test]
    fn delete_organization_flow() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        assert!(matches!(
            OrganizationService::delete_organization(&mut storage, &record.org_id, &rid('x'), NOW),
            Err(OrgError::AdminRequired)
        ));
        OrganizationService::delete_organization(&mut storage, &record.org_id, &admin, NOW + 1).unwrap();
        assert!(OrganizationService::get_record(&storage, &record.org_id).unwrap().is_none());
        let txs = crate::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
        assert_eq!(txs[0].type_, OrganizationTransactionType::Delete);
    }

    #[test]
    fn list_mine_filters_and_sorts() {
        let mut storage = MemoryStorage::new();
        let (admin, org1) = setup_org(&mut storage);
        // 第二个组织，updatedAt 更大
        let org2 = OrganizationService::create_organization(&mut storage, &input(), &admin, NOW + 100).unwrap();
        // 第三个组织：admin 不是成员（手工构造）
        let mut other = OrganizationRecord {
            org_id: "org_other".to_string(),
            name: "别人".to_string(),
            created_by: rid('z'),
            updated_at: NOW + 200,
            ..Default::default()
        };
        other.members.push(OrganizationMember {
            root_id: rid('z'),
            role: OrganizationRole::Admin,
            joined_at: NOW,
            added_by: rid('z'),
            node_info: None,
            extra: Default::default(),
        });
        OrganizationService::save_record(&mut storage, &other).unwrap();

        let mine = OrganizationService::list_mine(&storage, &admin).unwrap();
        let ids: Vec<&str> = mine.iter().map(|v| v.record.org_id.as_str()).collect();
        assert_eq!(ids, vec![org2.org_id.as_str(), org1.org_id.as_str()]);
        assert!(mine[0].is_current_user_admin);
        assert_eq!(mine[0].current_user_role, Some(OrganizationRole::Admin));
        assert_eq!(mine[0].member_count, 1);
        assert_eq!(mine[0].admin_count, 1);
    }

    #[test]
    fn invite_create_prepare_and_check() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        // 非 admin 不能生成
        assert!(matches!(
            OrganizationService::create_org_invite(&storage, &record.org_id, &rid('x'), None, &[], NOW),
            Err(OrgError::AdminRequired)
        ));
        // 无地址无 peerId
        assert!(matches!(
            OrganizationService::create_org_invite(&storage, &record.org_id, &admin, Some("  "), &[" ".to_string()], NOW),
            Err(OrgError::NetworkUnavailable)
        ));
        // 正常生成
        let created = OrganizationService::create_org_invite(
            &storage,
            &record.org_id,
            &admin,
            Some("12D3KooWAdmin"),
            &[" /ip4/1.2.3.4/tcp/15002/ws ".to_string()],
            NOW,
        ).unwrap();
        assert_eq!(created.org_id, record.org_id);
        assert_eq!(created.org_name, "星火 组织");

        // 自己接受自己的邀请 → 拒绝
        assert!(matches!(
            OrganizationService::prepare_accept_invite(&created.invite, &admin, NOW),
            Err(OrgError::SelfInvite)
        ));
        // 他人接受：decode 通过
        let member_id = root_id_of(MNEMONIC2);
        let payload = OrganizationService::prepare_accept_invite(&created.invite, &member_id, NOW).unwrap();
        assert_eq!(payload.inviter.root_id, admin);

        // 拉取前确认 → 未加入
        assert!(matches!(
            OrganizationService::check_invite_accepted(&storage, &record.org_id, &member_id),
            Err(OrgError::NotJoined)
        ));
        // 模拟拉取成功（成员已在记录中）
        OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW + 1).unwrap();
        let accepted = OrganizationService::check_invite_accepted(&storage, &record.org_id, &member_id).unwrap();
        assert_eq!(accepted.org_id, record.org_id);
        assert_eq!(accepted.member_count, 2);
        // 不存在的组织
        assert!(matches!(
            OrganizationService::check_invite_accepted(&storage, "org_nope", &member_id),
            Err(OrgError::NotJoined)
        ));
    }

    fn claim_for(mnemonic: &str, peer_id: Option<&str>, now: i64) -> NodeInfoClaim {
        let parsed = parse_mnemonic(mnemonic).unwrap();
        let identity = derive_root_identity(&parsed.seed);
        sign_node_info_claim(
            &identity.signing_key,
            OrganizationNodeInfo {
                peer_id: peer_id.map(str::to_string),
                addresses: vec!["/ip4/5.6.7.8/tcp/15002/ws".to_string()],
            },
            now,
        )
    }

    #[test]
    fn apply_node_info_claim_full_rules() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        let member_id = root_id_of(MNEMONIC2);
        OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW + 1).unwrap();

        let claim = claim_for(MNEMONIC2, Some("12D3KooWMember"), NOW + 2);
        // 落库：admin + 成员双条件满足
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &claim, &admin, Some("12D3KooWMember"), NOW + 2,
        ).unwrap();
        assert_eq!(applied, vec![record.org_id.clone()]);
        let updated = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        let m = updated.find_member(&member_id).unwrap();
        assert_eq!(m.node_info.as_ref().unwrap().peer_id.as_deref(), Some("12D3KooWMember"));
        assert_eq!(updated.updated_at, NOW + 2);
        let txs = crate::org::tx::list_organization_transactions(&storage, &record.org_id, 1).unwrap();
        assert_eq!(txs[0].type_, OrganizationTransactionType::MemberUpdate);
        assert_eq!(txs[0].actor_root_id, member_id);
        assert_eq!(txs[0].summary, format!("成员节点地址自动回填 {}", &member_id[..8]));
        assert_eq!(txs[0].payload.as_ref().unwrap()["source"], "node-info-claim");

        // 与现有 nodeInfo 完全一致 → 跳过，不 bump 版本
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &claim, &admin, Some("12D3KooWMember"), NOW + 5000,
        ).unwrap();
        assert!(applied.is_empty());
        let same = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        assert_eq!(same.updated_at, NOW + 2, "unchanged 不得 bump updatedAt");

        // remotePeerId 不匹配 → 静默丢弃
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &claim, &admin, Some("12D3KooWOther"), NOW + 6000,
        ).unwrap();
        assert!(applied.is_empty());

        // 非 admin 当前用户 → 静默跳过
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &claim, &rid('x'), None, NOW + 7000,
        ).unwrap();
        assert!(applied.is_empty());

        // 声明者不是成员 → 跳过
        let outsider_claim = claim_for(MNEMONIC, Some("12D3KooWAdmin2"), NOW);
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &outsider_claim, &member_id, None, NOW,
        ).unwrap();
        assert!(applied.is_empty());

        // 过期 claim → 不落库
        let stale_claim = claim_for(MNEMONIC2, Some("12D3KooWMember"), NOW - 20 * 60 * 1000);
        let applied = OrganizationService::apply_node_info_claim(
            &mut storage, &stale_claim, &admin, None, NOW,
        ).unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn recovery_view_admin_lazy_backfill() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        let member_id = root_id_of(MNEMONIC2);
        OrganizationService::add_member(&mut storage, &record.org_id, &member_id, None, &admin, NOW).unwrap();

        // 有 recoverySecret → 直接返回，只有含地址成员的 nodeInfo
        let view = OrganizationService::get_recovery_view(&mut storage, &admin, NOW).unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].org_id, record.org_id);
        assert_eq!(view[0].recovery_secret.len(), 64);
        assert!(view[0].member_node_infos.is_empty(), "成员均无地址");

        // 手工抹掉 recoverySecret 模拟存量组织：admin 惰性补齐
        let mut bare = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        bare.extra.remove(crate::org::types::OrganizationRecord::RECOVERY_SECRET_KEY);
        bare.sync = Some(crate::org::types::OrganizationSyncState {
            versions: OrganizationSyncVersions {
                summary_version: 1,
                members_version: 2,
                member_details_version: 3,
                transactions_version: 4,
            },
            sections: crate::org::snapshot::pick_sync_sections_by_priority(),
            last_synced_at: 777,
        });
        OrganizationService::save_record(&mut storage, &bare).unwrap();

        // 非 admin 成员本轮跳过
        let view = OrganizationService::get_recovery_view(&mut storage, &member_id, NOW + 10).unwrap();
        assert!(view.is_empty());
        // admin 补齐：生成盐、bump updatedAt、保留 transactionsVersion 与 lastSyncedAt
        let view = OrganizationService::get_recovery_view(&mut storage, &admin, NOW + 20).unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].recovery_secret.len(), 64);
        let patched = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        assert_eq!(patched.updated_at, NOW + 20);
        let sync = patched.sync.as_ref().unwrap();
        assert_eq!(sync.versions.summary_version, NOW + 20);
        assert_eq!(sync.versions.transactions_version, 4, "保留原 transactionsVersion");
        assert_eq!(sync.last_synced_at, 777, "保留原 lastSyncedAt");
        // 成员侧随后也能看到
        let view = OrganizationService::get_recovery_view(&mut storage, &member_id, NOW + 30).unwrap();
        assert_eq!(view.len(), 1);
    }

    #[test]
    fn apply_incoming_snapshot_accepts_both_shapes() {
        let mut storage = MemoryStorage::new();
        let (_admin, record) = setup_org(&mut storage);
        // 原始记录线形（org-share 推送）
        let value = serde_json::to_value(&record).unwrap();
        let merged = OrganizationService::apply_incoming_snapshot(&mut storage, &value, NOW + 1).unwrap();
        assert_eq!(merged.org_id, record.org_id);
        assert_eq!(merged.sync.as_ref().unwrap().last_synced_at, NOW + 1);

        // 快照线形（org-pull 响应）
        let snapshot = crate::org::snapshot::build_organization_sync_snapshot(&record, &[]);
        let value2 = serde_json::to_value(&snapshot).unwrap();
        let merged2 = OrganizationService::apply_incoming_snapshot(&mut storage, &value2, NOW + 2).unwrap();
        assert_eq!(merged2.members.len(), 1);
        assert_eq!(merged2.sync.as_ref().unwrap().last_synced_at, NOW + 2);
    }

    #[test]
    fn sync_recipients_filters() {
        let mut storage = MemoryStorage::new();
        let (admin, record) = setup_org(&mut storage);
        let with_peer = root_id_of(MNEMONIC2);
        let node = OrganizationNodeInfo {
            peer_id: Some("12D3KooWMember".to_string()),
            addresses: vec![],
        };
        OrganizationService::add_member(&mut storage, &record.org_id, &with_peer, Some(&node), &admin, NOW).unwrap();
        OrganizationService::add_member(&mut storage, &record.org_id, &rid('e'), None, &admin, NOW).unwrap();
        let record = OrganizationService::get_record(&storage, &record.org_id).unwrap().unwrap();
        let recipients = OrganizationService::sync_recipients(&record, &admin);
        assert_eq!(recipients.len(), 1, "排除 actor 与无 nodeInfo 成员");
        assert_eq!(recipients[0].root_id, with_peer);
    }
}
