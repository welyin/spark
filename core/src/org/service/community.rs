//! 组织加入共同体域的加入验证（org-genesis §3.2/§3.3 内核硬规则的存储接线）：
//! 成员种类约束 + DAG 成环检查。纯逻辑判定在 [`crate::org::genesis`]，本文件
//! 只做存储侧的图遍历（名册的公开成员关系 → 上级域反向闭包）。
//!
//! 共同体加入是跨组织异步交互（网关邮箱 accept 路径）的落库前校验；
//! 个人加入叶组织走 [`OrganizationService::add_member`]（其内已按域类型强制
//! kind = person）。
//!
//! 邀请/接受流（`community_invite.rs` 载荷 + 本文件服务面 + kernel
//! `community_ops` 编排）：共同体管理员创建邀请码（org-mail
//! `community-org-invite` 载荷投递）→ 接受方（待加入组织的管理员，持组织根
//! 私钥）解码 → **落库前 [`Self::validate_org_join`]** → 名册写入
//! `kind = org` 成员条目（rootId 槽位 = 组织根密钥对派生的共同体域身份 id，
//! orgBinding 公开绑定 opt-in）。
//!
//! 退出流（[`Self::leave_community`]，community-model「退出留史 / 域不可
//! 解散，只可退出」）：退出组织的在任管理员代表组织行事 → 追加 `org:cleave:`
//! 留史记录（append-only，历史不抹除）→ 名册移除成员条目 → 最后一个成员
//! 组织退出后域进入**空域只读档案**（[`Self::is_community_archived`] 由
//! 既有记录确定性推导：名册无 kind=org 成员 + 存在留史记录），此后
//! [`Self::require_community_writable`] 在各写路径拒绝写入。

use std::collections::HashMap;

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

use super::super::community_invite::{CommunityOrgInvitePayload, encode_community_org_invite};
use super::super::community_leave::{
    CommunityLeaveRecord, community_leave_key, community_leave_prefix,
};
use super::super::genesis::{enforce_member_kind, membership_would_cycle};
use super::super::invite::OrgInviteInviter;
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    DomainType, MemberKind, OrgBinding, OrganizationMember, OrganizationRecord, OrganizationRole,
    is_valid_root_id, org_member_key, sort_members,
};
use super::super::{OrgError, Result};
use super::OrganizationService;

/// `communityCreateInvite` 的返回：邀请码（base64url）+ 共同体标识。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedCommunityOrgInvite {
    /// 邀请码（base64url 编码的 [`CommunityOrgInvitePayload`]）。
    pub code: String,
    /// 共同体域 orgId。
    pub community_org_id: String,
    /// 共同体名。
    pub community_org_name: String,
}

/// `communityAcceptInvite` 落库确认结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityJoinOutcome {
    /// 共同体域 orgId。
    pub community_org_id: String,
    /// 共同体名（本地记录口径）。
    pub community_org_name: String,
    /// 本组织在该共同体的域身份 id（成员条目 rootId 槽位，org-genesis §3.2/§4）。
    pub member_identity: String,
    /// 是否已是成员（重复接受幂等，未产生新写入）。
    pub already_member: bool,
    /// 本次接受是否自举了 stub 共同体记录（kernel 侧据此压 pmeta.ts=0——
    /// 零信息占位在任何裁决面皆输，同 `org_join_ops` stub 口径）。
    pub stub_bootstrapped: bool,
}

/// `communityLeave` 落库结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityLeaveOutcome {
    /// 共同体域 orgId。
    pub community_org_id: String,
    /// 共同体名（本地记录口径）。
    pub community_org_name: String,
    /// 退出组织在该共同体的域身份 id（名册成员条目 rootId 槽位）。
    pub member_identity: String,
    /// 本次退出是否使域进入空域只读档案（最后一个成员组织退出）。
    pub domain_archived: bool,
}

/// 共同体成员视图条目（名册中 `kind = org` 的成员）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityOrgMemberView {
    /// 组织在本共同体的域身份 id（成员条目 rootId 槽位，64hex）。
    pub identity: String,
    /// 角色。
    pub role: OrganizationRole,
    /// 加入时间（ms）。
    pub joined_at: i64,
    /// opt-in 公开组织绑定（org-genesis §3.2；未公开为 None）。
    pub org_binding: Option<OrgBinding>,
}

impl OrganizationService {
    /// 组织加入共同体域的加入验证（org-genesis §3.2/§3.3，加入操作验证时强制）：
    ///
    /// 1. 目标域存在且为共同体域（`leaf` 域拒绝组织成员——成员种类硬规则）；
    /// 2. 成环检查：待加入组织已出现在目标域的可达祖先集中（含目标域本身）
    ///    → 拒绝（`MembershipCycle`）。
    ///
    /// 成员关系图取各域公开名册中 `kind = org` 成员的 opt-in 公开绑定
    /// （`orgBinding.orgId`，org-genesis §3.2）；未公开绑定的组织成员不进图
    /// （无公开锚可查，如实标注——成环检查对不可见边不做承诺）。
    pub fn validate_org_join<S: StorageBackend>(
        storage: &S,
        joiner_org_id: &str,
        target_org_id: &str,
    ) -> Result<()> {
        let target =
            Self::get_record(storage, target_org_id)?.ok_or(OrgError::OrganizationNotFound)?;
        // 硬规则：共同体域只接受组织成员（目标为 leaf 域即拒绝）。
        enforce_member_kind(target.domain_type.unwrap_or_default(), MemberKind::Org)?;

        // 上级域闭包：org → 它已加入的域（由各域名册反查）。
        let parent_map = Self::member_of_index(storage)?;
        let parents =
            |org_id: &str| -> Vec<String> { parent_map.get(org_id).cloned().unwrap_or_default() };
        if membership_would_cycle(joiner_org_id, target_org_id, &parents) {
            return Err(OrgError::MembershipCycle);
        }
        Ok(())
    }

    /// 名册反查索引：memberOrgId → 已将其列入公开名册的域列表。
    ///
    /// 扫描 `org:meta:` 全量记录取 `kind = org` 成员的 `orgBinding.orgId`
    /// （公开绑定）；闭包遍历中逐层展开（图无单父约束，一个组织可加入多个域）。
    fn member_of_index<S: StorageBackend>(storage: &S) -> Result<HashMap<String, Vec<String>>> {
        let mut index: HashMap<String, Vec<String>> = HashMap::new();
        for record in Self::read_all_organizations(storage)? {
            for member in &record.members {
                if member.member_kind() != MemberKind::Org {
                    continue;
                }
                let Some(parent) = member.org_binding.as_ref().and_then(|b| b.org_id.clone())
                else {
                    continue;
                };
                index.entry(parent).or_default().push(record.org_id.clone());
            }
        }
        Ok(index)
    }

    /// 入站名册的共同体硬规则执法（org-genesis §3.2/§3.3 合入侧）：对名册
    /// 逐条校验 `kind = org` 成员——
    ///
    /// 组织成员关系进入本地存储有两条生产边界：出站侧跨组织 accept 流
    /// （邀请码经 org-mail 投递、[`Self::accept_community_org_invite`] 落库
    /// 前 [`Self::validate_org_join`]，见 kernel `community_ops`）与本入站
    /// 合入侧（对端名册经 orgsync 到达，本函数执法）；两侧同源校验——
    ///
    /// 1. 成员种类硬规则：本域非共同体域（leaf/缺省）→ 剔除组织成员条目；
    /// 2. 成环检查：对「既有公开关系图（存储中各域名册）+ 本批已接受边」
    ///    增量判定，成环条目剔除。
    ///
    /// 违规条目剔除并 WARN（保留其余名册条目，不整份拒收——与合入语义的
    /// 降级口径一致）；无公开绑定（`orgBinding.orgId` 缺失）的组织成员不进
    /// 图、不剔除（同 [`Self::member_of_index`] 口径）。返回剔除条数。
    pub fn enforce_incoming_roster<S: StorageBackend>(
        storage: &S,
        record: &mut OrganizationRecord,
    ) -> Result<usize> {
        let mut index = Self::member_of_index(storage)?;
        let domain = record.domain_type.unwrap_or_default();
        let target = record.org_id.clone();
        let mut dropped = 0usize;
        record.members.retain(|member| {
            if member.member_kind() != MemberKind::Org {
                return true;
            }
            let Some(joiner) = member
                .org_binding
                .as_ref()
                .and_then(|b| b.org_id.clone())
            else {
                return true;
            };
            if enforce_member_kind(domain, MemberKind::Org).is_err() {
                dropped += 1;
                log::warn!(
                    "[ORG] roster entry dropped: kind=org in non-community domain | org={} member={}",
                    target,
                    &joiner[..std::cmp::min(20, joiner.len())]
                );
                return false;
            }
            let parents = |org_id: &str| -> Vec<String> {
                index.get(org_id).cloned().unwrap_or_default()
            };
            if membership_would_cycle(&joiner, &target, &parents) {
                dropped += 1;
                log::warn!(
                    "[ORG] roster entry dropped: membership cycle | org={} member={}",
                    target,
                    &joiner[..std::cmp::min(20, joiner.len())]
                );
                return false;
            }
            // 接受边增量入图：同批后续条目的成环判定可见本边
            index.entry(joiner).or_default().push(target.clone());
            true
        });
        Ok(dropped)
    }

    // ------------------------------------------------------------------
    // 共同体邀请/接受（org-mail 跨组织流的存储面；编排见 kernel community_ops）
    // ------------------------------------------------------------------

    /// 创建共同体邀请码（仅共同体域 admin）：域类型硬规则前置（leaf 域没有
    /// 组织成员可邀请），载荷线形/编码见 [`CommunityOrgInvitePayload`]。
    ///
    /// `community_org_address`（共同体地址记录完整线形）与 `reply_domain_id`
    /// （邀请人收回信的 org-mail 域身份）由 kernel 侧注入——均可省，缺省时
    /// 接受侧跳过加入通知回发（私有共同体无地址记录即此形态）。
    pub fn create_community_org_invite<S: StorageBackend>(
        storage: &S,
        community_org_id: &str,
        current_root_id: &str,
        inviter: OrgInviteInviter,
        community_org_address: Option<String>,
        reply_domain_id: Option<String>,
        now_ms: i64,
    ) -> Result<CreatedCommunityOrgInvite> {
        let record = Self::require_organization(storage, community_org_id)?;
        Self::require_admin(&record, current_root_id)?;
        // 硬规则：只有共同体域能接受组织成员（leaf 域创建组织邀请即拒绝）。
        enforce_member_kind(record.domain_type.unwrap_or_default(), MemberKind::Org)?;
        // 空域只读档案：全员已退出的共同体不再产生新邀请（无人能写入）。
        Self::require_community_writable(storage, &record)?;

        let mut payload = CommunityOrgInvitePayload::new(
            record.org_id.clone(),
            record.name.clone(),
            inviter,
            now_ms,
        );
        payload.community_org_address = community_org_address;
        payload.reply_domain_id = reply_domain_id;
        Ok(CreatedCommunityOrgInvite {
            code: encode_community_org_invite(&payload),
            community_org_id: record.org_id,
            community_org_name: record.name,
        })
    }

    /// 接受共同体邀请的落库（接受侧 = 待加入组织的管理员本机）：
    ///
    /// 1. 待加入组织存在且当前身份为其 admin（代表组织行事）；
    /// 2. `member_identity`（组织根密钥对派生的共同体域身份 id，kernel 侧
    ///    经 [`crate::org::OrgDomainIdentity`] 派生注入）形状校验；
    /// 3. 共同体记录缺失时自举 **stub 记录**（orgId/名称/邀请人取自邀请载荷，
    ///    `domainType = community`、`updated_at = 0`——同 `org_join_ops`
    ///    个人加入的 stub 自举口径：零信息占位，真实记录到达后全字段秩高
    ///    覆盖）；
    /// 4. **落库前 [`Self::validate_org_join`]**（域类型硬规则 + DAG 成环
    ///    检查，org-genesis §3.2/§3.3）；
    /// 5. 名册写入 `kind = org` 成员条目：rootId 槽位 = `member_identity`，
    ///    `orgBinding` 公开绑定 opt-in（`publish_binding = false` 时不公开
    ///    orgId/地址，org-genesis §3.2 可见性策略）。已是成员 → 幂等返回
    ///    （`already_member = true`，不产生新写入）。
    ///
    /// `node_id = Some` 走 pdsync 版本化落库（kernel 门面路径）；`None` 裸写
    /// （测试/纯存储路径）。
    #[allow(clippy::too_many_arguments)]
    pub fn accept_community_org_invite<S: StorageBackend>(
        storage: &mut S,
        payload: &CommunityOrgInvitePayload,
        joiner_org_id: &str,
        current_root_id: &str,
        member_identity: &str,
        publish_binding: bool,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<CommunityJoinOutcome> {
        let joiner = Self::require_organization(storage, joiner_org_id)?;
        Self::require_admin(&joiner, current_root_id)?;
        if !is_valid_root_id(member_identity) {
            return Err(OrgError::InvalidMemberRootId);
        }

        // stub 自举：本地无共同体记录时按邀请载荷落零信息占位（真实记录经
        // 后续同步到达；入站合入侧硬规则执法见 enforce_incoming_roster）
        let mut stub_bootstrapped = false;
        if Self::get_record(storage, &payload.community_org_id)?.is_none() {
            let stub = OrganizationRecord {
                org_id: payload.community_org_id.clone(),
                name: payload.community_org_name.clone(),
                created_at: 0,
                created_by: payload.inviter.root_id.clone(),
                updated_at: 0,
                domain_type: Some(super::super::types::DomainType::Community),
                ..Default::default()
            };
            match node_id {
                Some(node_id) => {
                    Self::save_record_pdsync(storage, &stub, now_ms, node_id)?;
                    crate::plugindata::declare_builtin_org_collections(
                        storage,
                        &payload.community_org_id,
                        current_root_id,
                        now_ms,
                        node_id,
                    )?;
                }
                None => Self::save_record(storage, &stub)?,
            }
            stub_bootstrapped = true;
        }

        // 落库前加入验证（域类型硬规则 + 成环检查）
        Self::validate_org_join(storage, joiner_org_id, &payload.community_org_id)?;

        let mut community = Self::require_organization(storage, &payload.community_org_id)?;
        // 空域只读档案：全员已退出的共同体不再接受新加入（无人能写入）。
        Self::require_community_writable(storage, &community)?;
        if community.find_member(member_identity).is_some() {
            return Ok(CommunityJoinOutcome {
                community_org_id: community.org_id,
                community_org_name: community.name,
                member_identity: member_identity.to_string(),
                already_member: true,
                stub_bootstrapped,
            });
        }

        let entry = OrganizationMember {
            root_id: member_identity.to_string(),
            role: OrganizationRole::Member,
            joined_at: now_ms,
            added_by: payload.inviter.root_id.clone(),
            kind: Some(MemberKind::Org),
            org_binding: publish_binding.then(|| OrgBinding {
                org_id: Some(joiner_org_id.to_string()),
                org_address: joiner.org_address.clone(),
            }),
            ..Default::default()
        };
        community.members.push(entry.clone());
        community.members = sort_members(&community.members);
        match node_id {
            Some(node_id) => Self::save_record_pdsync(storage, &community, now_ms, node_id)?,
            None => Self::save_record(storage, &community)?,
        }
        // per-member 条目双写（阶段四A 分拆口径：条目为权威段）
        storage.put(
            &org_member_key(&community.org_id, member_identity),
            &serde_json::to_string(&entry)?,
        )?;
        Ok(CommunityJoinOutcome {
            community_org_id: community.org_id,
            community_org_name: community.name,
            member_identity: member_identity.to_string(),
            already_member: false,
            stub_bootstrapped,
        })
    }

    /// 列共同体成员（本地记录名册中 `kind = org` 的条目；公开绑定缺失的
    /// 成员只呈现域身份 id——org-genesis §3.2 可见性策略如实标注）。
    pub fn list_community_org_members<S: StorageBackend>(
        storage: &S,
        community_org_id: &str,
    ) -> Result<Vec<CommunityOrgMemberView>> {
        let record = Self::require_organization(storage, community_org_id)?;
        Ok(record
            .members
            .iter()
            .filter(|m| m.member_kind() == MemberKind::Org)
            .map(|m| CommunityOrgMemberView {
                identity: m.root_id.clone(),
                role: m.role,
                joined_at: m.joined_at,
                org_binding: m.org_binding.clone(),
            })
            .collect())
    }

    // ------------------------------------------------------------------
    // 组织退出共同体与空域只读档案（community-model：退出留史；域不可解散，
    // 只可退出——全员退出后域成为空域，只读历史档案，无人能写入）
    // ------------------------------------------------------------------

    /// 空域只读档案判定（确定性、可从既有记录推导，无易失标志）：
    ///
    /// `record` 为共同体域 且 名册无 `kind = org` 成员 且 存在 `org:cleave:`
    /// 留史记录（曾发生退出）。三个条件缺一不可：
    /// - 新建未收过成员组织的共同体（无留史记录）**不是**空域——邀请/加入
    ///   照常可用；
    /// - 仍有成员组织的共同体不是空域；
    /// - 重新加入后名册恢复非空即脱离档案态（留史记录保留，只作历史）。
    pub fn is_community_archived<S: StorageBackend>(
        storage: &S,
        record: &OrganizationRecord,
    ) -> Result<bool> {
        if record.domain_type != Some(DomainType::Community) {
            return Ok(false);
        }
        if record.members.iter().any(|m| m.member_kind() == MemberKind::Org) {
            return Ok(false);
        }
        Ok(!storage
            .scan(&ScanOptions::prefix(community_leave_prefix(&record.org_id)))?
            .is_empty())
    }

    /// 写路径守卫：空域只读档案拒绝写入（`OrgError::CommunityDomainArchived`）。
    /// 非共同体域 / 非档案态直接放行——leaf 域与活跃共同体的既有行为不变。
    pub fn require_community_writable<S: StorageBackend>(
        storage: &S,
        record: &OrganizationRecord,
    ) -> Result<()> {
        if Self::is_community_archived(storage, record)? {
            return Err(OrgError::CommunityDomainArchived);
        }
        Ok(())
    }

    /// 组织退出共同体的落库（本机为退出组织的管理员，代表组织行事——校验
    /// 口径对齐 [`Self::accept_community_org_invite`]：组织存在 + 当前身份为
    /// 其在任 admin；kernel 侧另以组织根私钥派生域身份，持钥即组织身份）：
    ///
    /// 1. 目标须为共同体域且非空域档案（档案态无人能写入）；
    /// 2. 成员条目须存在且为 `kind = org`；条目已公开 `orgBinding.orgId` 时
    ///    必须与退出组织一致（防以他人域身份代办退出）；
    /// 3. **留史**：追加 `org:cleave:` 留史记录（append-only，含退出时公开
    ///    绑定快照；未公开绑定不记 orgId，留史不扩大披露面）+ 本地 `member-leave`
    ///    事务审计；
    /// 4. 更新成员关系：名册移除该条目 + per-member 条目删除（版本化句柄
    ///    上自动墓碑 + org 域 dlog 传播，同成员移除口径）；
    /// 5. 最后一个成员组织退出 → `domain_archived = true`，此后本域写路径
    ///    一律被 [`Self::require_community_writable`] 拒绝，读路径不受影响。
    ///
    /// `node_id = Some` 走 pdsync 版本化落库（kernel 门面路径）；`None` 裸写
    /// （测试/纯存储路径）。
    pub fn leave_community<S: StorageBackend>(
        storage: &mut S,
        community_org_id: &str,
        leaver_org_id: &str,
        current_root_id: &str,
        member_identity: &str,
        now_ms: i64,
        node_id: Option<&str>,
    ) -> Result<CommunityLeaveOutcome> {
        let leaver = Self::require_organization(storage, leaver_org_id)?;
        Self::require_admin(&leaver, current_root_id)?;
        if !is_valid_root_id(member_identity) {
            return Err(OrgError::InvalidMemberRootId);
        }

        let mut community = Self::require_organization(storage, community_org_id)?;
        // 硬规则：退出语义只对共同体域成立（leaf 域没有组织成员）。
        enforce_member_kind(community.domain_type.unwrap_or_default(), MemberKind::Org)?;
        // 空域只读档案：已全员退出的域无人能写入（此时名册必无该成员，
        // 档案错误先于成员缺失错误给出，语义更明确）。
        Self::require_community_writable(storage, &community)?;

        let Some(member) = community.find_member(member_identity) else {
            return Err(OrgError::NotCommunityMember);
        };
        if member.member_kind() != MemberKind::Org {
            return Err(OrgError::NotCommunityMember);
        }
        // 已公开绑定的条目：绑定 orgId 必须与退出组织一致（域身份由退出组织
        // 根密钥派生，正常路径天然一致；此守卫兜住服务层的误用/伪造调用）。
        if let Some(published) = member.org_binding.as_ref().and_then(|b| b.org_id.as_deref())
            && published != leaver_org_id
        {
            return Err(OrgError::NotCommunityMember);
        }
        let binding_snapshot = member.org_binding.clone();

        // 留史（先追加记录，再移除名册——任何中途失败都不留「已退无史」态）
        let leave = CommunityLeaveRecord {
            leave_v: 1,
            community_org_id: community_org_id.to_string(),
            member_identity: member_identity.to_string(),
            org_binding: binding_snapshot,
            left_at: now_ms,
            actor_root_id: current_root_id.to_string(),
        };
        storage.put(
            &community_leave_key(community_org_id, member_identity, now_ms),
            &serde_json::to_string(&leave)?,
        )?;

        // 成员关系更新：名册移除 + per-member 条目删除（版本化句柄自动墓碑
        // + org 域 dlog 传播；raw 句柄裸删）
        community.members.retain(|m| m.root_id != member_identity);
        storage.delete(&org_member_key(community_org_id, member_identity))?;

        let domain_archived = !community
            .members
            .iter()
            .any(|m| m.member_kind() == MemberKind::Org);
        community.updated_at = now_ms;
        let previous_last_synced_at = community
            .sync
            .as_ref()
            .map(|s| s.last_synced_at)
            .unwrap_or(0);
        // 事务审计（org:tx 纯本地审计日志）：退出组织 orgId 仅在已公开绑定
        // 时落 payload（与留史记录同披露口径）
        let mut tx_payload = serde_json::Map::from_iter([(
            "domainArchived".to_string(),
            Value::from(domain_archived),
        )]);
        if let Some(published) = leave.org_binding.as_ref().and_then(|b| b.org_id.clone()) {
            tx_payload.insert("leaverOrgId".to_string(), Value::from(published));
        }
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id: community_org_id.to_string(),
                type_: OrganizationTransactionType::MemberLeave,
                created_at: now_ms,
                actor_root_id: current_root_id.to_string(),
                target_root_id: Some(member_identity.to_string()),
                summary: format!("组织退出共同体 {}", community.name),
                payload: Some(tx_payload),
            },
        )?;
        Self::rebuild_sync_after_mutation(&mut community, previous_last_synced_at, transaction.created_at);
        let outcome = CommunityLeaveOutcome {
            community_org_id: community.org_id.clone(),
            community_org_name: community.name.clone(),
            member_identity: member_identity.to_string(),
            domain_archived,
        };
        match node_id {
            Some(node_id) => Self::save_record_pdsync(storage, &community, now_ms, node_id)?,
            None => Self::save_record(storage, &community)?,
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::types::DomainType;
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRecord};

    fn org_record(
        org_id: &str,
        domain: DomainType,
        members: Vec<OrganizationMember>,
    ) -> OrganizationRecord {
        OrganizationRecord {
            org_id: org_id.to_string(),
            domain_type: Some(domain),
            members,
            ..Default::default()
        }
    }

    fn org_member(identity: &str, binding_org: &str) -> OrganizationMember {
        OrganizationMember {
            root_id: identity.to_string(),
            kind: Some(MemberKind::Org),
            org_binding: Some(super::super::super::types::OrgBinding {
                org_id: Some(binding_org.to_string()),
                org_address: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn join_validation_rejects_cycle_and_wrong_domain() {
        let mut storage = crate::storage::MemoryStorage::new();
        // 图（公开名册线形）：A 名册列 B（B 加入 A）；B 名册列 C（C 加入 B）。
        // 三个均为共同体域（kind 硬规则放行组织成员，专测成环）。
        let a = org_record(
            "org_aaaaaaaaaaaaaaaa",
            DomainType::Community,
            vec![org_member("bb".repeat(32).as_str(), "org_bbbbbbbbbbbbbbbb")],
        );
        let b = org_record(
            "org_bbbbbbbbbbbbbbbb",
            DomainType::Community,
            vec![org_member("cc".repeat(32).as_str(), "org_cccccccccccccccc")],
        );
        let c = org_record("org_cccccccccccccccc", DomainType::Community, vec![]);
        OrganizationService::save_record(&mut storage, &a).unwrap();
        OrganizationService::save_record(&mut storage, &b).unwrap();
        OrganizationService::save_record(&mut storage, &c).unwrap();

        // 传递环：A 加入 C——C 的可达祖先集 = {B, A}，A 已在其中
        //（名册口径：A→B→C 的成员链已存在，A 再加入 C 即成环）
        let err = OrganizationService::validate_org_join(
            &storage,
            "org_aaaaaaaaaaaaaaaa",
            "org_cccccccccccccccc",
        )
        .unwrap_err();
        assert!(matches!(err, OrgError::MembershipCycle));

        // 无环：C 加入 A（A 未加入任何域；C→B→A 与既有边不构成环）
        assert!(
            OrganizationService::validate_org_join(
                &storage,
                "org_cccccccccccccccc",
                "org_aaaaaaaaaaaaaaaa",
            )
            .is_ok()
        );

        // leaf 域拒绝组织成员（成员种类硬规则先于成环检查）
        let leaf = org_record("org_dddddddddddddddd", DomainType::Leaf, vec![]);
        OrganizationService::save_record(&mut storage, &leaf).unwrap();
        let err = OrganizationService::validate_org_join(
            &storage,
            "org_cccccccccccccccc",
            "org_dddddddddddddddd",
        )
        .unwrap_err();
        assert!(matches!(err, OrgError::MemberKindNotAllowed(_)));
    }
}
