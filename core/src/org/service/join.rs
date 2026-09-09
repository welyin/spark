//! 免预录凭证入册的合入侧存储接线（A17，membership §4.5；org-join §8）：
//!
//! [`OrganizationService::accept_join_request`] = 加入声明的**合入点**——
//! 存储装载（名册预录判定 / 生效准入策略 / 信任声明 / 注销快照）→
//! [`crate::org::adjudicate_join_request`] 双路径合一纯逻辑验证 → 受理即
//! 入册（org:meta 原子段原语，whole + per-member 条目双写；事务审计携带
//! credId）。零管理员在线：任一**成员**节点皆可合入（其名册写入经 orgsync
//! 「from ∈ 成员表 ∩ 复制组」前置扩散，多节点并发处理按既有合并规则幂等
//! 收敛）。
//!
//! 拒收是数据不是异常（[`JoinOutcome::Rejected`]，kind 逐字稳定与
//! [`crate::org::JoinRejection::kind`] 对齐）；存储/记录级故障才走
//! [`OrgError`]。

use crate::credential::{RevocationSnapshot, RevocationView, revocation_snapshot_key, trust_decl_key};
use crate::policy::{accept_policy_key, effective_accept_policy};
use crate::storage::StorageBackend;

use super::super::access_key::verify_access_key_binding;
use super::super::genesis::enforce_member_kind;
use super::super::join_request::{JoinPath, JoinRequest, adjudicate_join_request};
use super::super::tx::{OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction};
use super::super::types::{
    MemberKind, OrganizationDeviceSet, OrganizationMember, OrganizationRole,
};
use super::{OrgMetaWriteLock, OrganizationService, Result};

/// 加入申请合入结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    /// 受理入册（路径 + 免预录路径的 credId）。
    Enrolled {
        /// 入册路径（claim 认领 / credential 免预录）。
        path: JoinPath,
        /// 免预录路径的 credId（事务审计引用；认领为 None）。
        cred_id: Option<String>,
    },
    /// 拒收（验证任一环节失败，fail-closed 不落库；kind 逐字稳定）。
    Rejected(String),
}

impl OrganizationService {
    /// 加入声明合入（org-join §8 生产调用点：org-mail `org-join-request`
    /// 载荷解箱后的处理入口——呈现层/自动处理皆经本函数）。
    ///
    /// 不要求 admin——本函数不含权限校验（凭证 + 策略替代管理员当场合签）；
    /// 调用方（kernel）须先确认本机为目标组织成员（写入扩散前提）。
    pub fn accept_join_request<S: StorageBackend>(
        storage: &mut S,
        io_lock: &OrgMetaWriteLock,
        org_id: &str,
        request: &JoinRequest,
        now_ms: i64,
    ) -> Result<JoinOutcome> {
        let record = Self::require_organization(storage, org_id)?;
        let pre_registered = record.find_member(&request.applicant.identity).is_some();

        // 生效准入策略（本地现行记录经合入裁决进入存储；损坏 = 无策略，
        // fail-closed）
        let policy: Option<crate::policy::AcceptPolicyRecord> = storage
            .get(&accept_policy_key(org_id))?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let effective = effective_accept_policy(policy.as_ref(), now_ms);

        // 信任声明（既往不咎时间线取版在验证链内）与注销快照按凭证注入；
        // 缺失 = 数据不可用 → 验证链 fail-closed（与 read-gate 同口径）。
        let trust_decl: Option<crate::credential::TrustDecl> = request
            .credential
            .as_ref()
            .and_then(|cred| {
                storage
                    .get(&trust_decl_key(&cred.subject_domain))
                    .ok()
                    .flatten()
            })
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let trust_decls: Vec<&crate::credential::TrustDecl> = trust_decl.iter().collect();
        let revocation_snapshot: Option<RevocationSnapshot> = request
            .credential
            .as_ref()
            .and_then(|cred| {
                storage
                    .get(&revocation_snapshot_key(&cred.issuer.identity))
                    .ok()
                    .flatten()
            })
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let revocation = revocation_snapshot.as_ref().map(|snap| RevocationView {
            entries: &snap.entries,
            head: &snap.head,
        });

        let admission = match adjudicate_join_request(
            org_id,
            request,
            pre_registered,
            effective,
            &trust_decls,
            revocation.as_ref(),
            now_ms,
        ) {
            Ok(admission) => admission,
            Err(rejection) => {
                log::info!(
                    "[ORG-JOIN] join request rejected | org={org_id} kind={}",
                    rejection.kind()
                );
                return Ok(JoinOutcome::Rejected(rejection.kind().to_string()));
            }
        };

        // 入册（原子段：提交前重读校验 + 三路合并；whole + 成员条目双写由
        // 原语完成）。并发下名册状态可能已变（他节点先入册）——mutate 内
        // 按当时名册重判路径（幂等：已在册退化为认领补齐）。
        let applicant = request.applicant.identity.clone();
        let access_key = request.access_key.clone();
        let node_info = request.node_info.clone();
        let cred_id = admission.cred_id.clone();
        let mut actual_path = admission.path;
        Self::update_record_atomic(storage, io_lock, org_id, |storage, record| {
            Self::enroll_join_mutate(
                storage,
                record,
                &applicant,
                &access_key,
                node_info.as_ref(),
                cred_id.as_deref(),
                &mut actual_path,
                now_ms,
            )
        })?;
        Ok(JoinOutcome::Enrolled {
            path: actual_path,
            cred_id,
        })
    }

    /// 入册纯变更段（原子段原语的 mutate）：认领 = 预录条目补 accessKey
    /// （写一次）/nodeInfo（端点集 upsert）；免预录 = 新增成员条目（role
    /// member、addedBy = 申请人自录、成员种类硬规则同 addMember）。返回
    /// 是否有变更（幂等重复申请无写不 bump）。
    #[allow(clippy::too_many_arguments)]
    fn enroll_join_mutate<S: StorageBackend>(
        storage: &mut S,
        record: &mut super::super::types::OrganizationRecord,
        applicant: &str,
        access_key: &super::super::types::OrganizationAccessKey,
        node_info: Option<&super::super::types::OrganizationNodeInfo>,
        cred_id: Option<&str>,
        actual_path: &mut JoinPath,
        now_ms: i64,
    ) -> Result<bool> {
        Self::require_community_writable(storage, record)?;
        let org_id = record.org_id.clone();
        let previous_last_synced_at = record.sync.as_ref().map(|s| s.last_synced_at).unwrap_or(0);

        if let Some(member) = record
            .members
            .iter_mut()
            .find(|m| m.root_id == applicant)
        {
            // 认领路径（含并发下免预录退化为认领）：accessKey 写一次 +
            // 端点集 upsert
            *actual_path = JoinPath::Claim;
            let mut changed = false;
            if member.access_key.is_none() {
                debug_assert!(
                    verify_access_key_binding(&org_id, applicant, access_key),
                    "accessKey 已经 adjudicate 验绑"
                );
                member.access_key = Some(access_key.clone());
                changed = true;
            }
            if let Some(info) = node_info {
                let set = member.node_info.get_or_insert_with(Default::default);
                changed |= set.upsert(info);
            }
            if !changed {
                return Ok(false); // 幂等重复申请（已认领）
            }
            record.updated_at = now_ms;
            let transaction = append_organization_transaction(
                storage,
                OrganizationTransactionRecord {
                    tx_id: String::new(),
                    org_id,
                    type_: OrganizationTransactionType::MemberUpdate,
                    created_at: now_ms,
                    actor_root_id: applicant.to_string(),
                    target_root_id: Some(applicant.to_string()),
                    summary: "认领预录成员条目（org-join-request）".to_string(),
                    payload: None,
                },
            )?;
            Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
            return Ok(true);
        }

        // 免预录路径：新增成员（个人成员硬规则同 addMember——共同体域只
        // 接受组织成员，org-join-request 路径不录组织成员）
        enforce_member_kind(record.domain_type.unwrap_or_default(), MemberKind::Person)?;
        record.members.push(OrganizationMember {
            root_id: applicant.to_string(),
            role: OrganizationRole::Member,
            joined_at: now_ms,
            added_by: applicant.to_string(), // 自录（凭证 + 策略替代管理员当场合签）
            node_info: node_info.map(|info| OrganizationDeviceSet::from_single(info.clone())),
            nickname: None,
            avatar: None,
            signature: None,
            gender: None,
            region: None,
            use_personal_identity: None,
            access_key: Some(access_key.clone()),
            kind: None,
            org_binding: None,
            extra: Default::default(),
        });
        record.updated_at = now_ms;
        let transaction = append_organization_transaction(
            storage,
            OrganizationTransactionRecord {
                tx_id: String::new(),
                org_id,
                type_: OrganizationTransactionType::MemberAdd,
                created_at: now_ms,
                actor_root_id: applicant.to_string(),
                target_root_id: Some(applicant.to_string()),
                summary: "免预录凭证入册（org-join-request）".to_string(),
                payload: cred_id.map(|id| {
                    [("credId".to_string(), serde_json::Value::from(id))]
                        .into_iter()
                        .collect()
                }),
            },
        )?;
        Self::rebuild_sync_after_mutation(record, previous_last_synced_at, transaction.created_at);
        Ok(true)
    }
}
