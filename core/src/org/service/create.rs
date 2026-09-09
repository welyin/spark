//! 组织创建（service.ts `createOrganization`）与全域删除守卫（A13）。
//!
//! 创建（C1，org-genesis §1/§2）：新组织一律创世哈希型——生成组织根密钥对，
//! 构造并签名创世策略记录（含 domainType/互绑 orgAddress），orgId =
//! `genesis_org_id(创世记录)`（`org_<64hex>` 自认证），创世记录落
//! `org:genesis:{orgId}`（org:structure@v1 键域，写一次不可变，随 orgsync
//! 全员流动）；创建者为唯一初始 admin，追加 `create` 事务并落库。
//! legacy `org_<16hex>` 仅为存量形态（既有数据原样可读/同步/加入），不再产生。
//! 删除：通路已整体移除（A13，community-model「域不可解散，只可退出」），
//! 仅存全域删除守卫 [`OrganizationService::delete_organization_impl`]
//! （全部域类型拒绝，fail-closed 兜底）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::Value;

use crate::storage::StorageBackend;

use super::super::genesis::{
    GenesisPolicyRecord, SigningPolicy, default_transition_decl, genesis_org_id, org_genesis_key,
    sign_genesis_record,
};
use super::super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::super::tx::{
    OrganizationTransactionRecord, OrganizationTransactionType, append_organization_transaction,
};
use super::super::types::{
    OrganizationMember, OrganizationRecord, OrganizationRole, OrganizationSyncState,
    generate_org_secret, generate_recovery_secret, normalize_plugin_domain, normalize_text,
    org_member_key,
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

        // C1 创世路径（org-genesis §1/§2）：域类型/签名策略/过渡声明先定
        // （创建时确定、不可变更），再生成组织根密钥对、构造创世策略记录——
        // orgId = genesis_org_id(创世记录)，自认证（org_<64hex>）。
        let domain_type = input.domain_type.unwrap_or_default();
        let signing_policy = input
            .signing_policy
            .clone()
            .unwrap_or(SigningPolicy::AnyAdmin);
        // m-of-n 校验（org-genesis §1：1 ≤ m ≤ n；实现口径 n ≤ 快照内 admin
        // 数——创建时创建者为唯一初始 admin，即 n ≤ 1）
        if let SigningPolicy::MOfN { m, n } = &signing_policy
            && (*m == 0 || m > n || *n > 1)
        {
            return Err(OrgError::InvalidSigningPolicy);
        }
        let transition = input
            .transition
            .clone()
            .or_else(|| Some(default_transition_decl()));

        // 组织根密钥对与 orgAddress（org.md §15）：创建时生成独立 Ed25519 密钥对；
        // 根私钥加密存 extra（不进快照、不同步出本机）
        let org_root_key = org_address::generate_org_root_signing_key();
        let org_address =
            org_address::org_address_from_public_key(&org_root_key.verifying_key().to_bytes());
        let mut genesis = GenesisPolicyRecord {
            genesis_v: 1,
            name: name.clone(),
            description: description.clone(),
            domain_type,
            root_public_key: B64.encode(org_root_key.verifying_key().to_bytes()),
            // 互绑（C1）：创世记录 orgAddress = §15 公式对 rootPublicKey 的派生值
            org_address: org_address.clone(),
            signing_policy,
            transition,
            born_of: input.born_of.clone(),
            created_by: current_root_id.to_string(),
            created_at: now_ms,
            sig: String::new(),
        };
        sign_genesis_record(&mut genesis, &org_root_key);
        let org_id = genesis_org_id(&genesis)?;

        let mut record = OrganizationRecord {
            org_id,
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
                kind: None,
                org_binding: None,
                extra: Default::default(),
            }],
            sync: None,
            // O1：创建时不显式指定角色——缺省推导生效（网关=全员候选、
            // 数据=全体管理员即创建者）
            gateways: Vec::new(),
            data_accounts: Vec::new(),
            // 互绑（C1，org-genesis §1）：record.orgAddress 与创世记录同源——
            // 同一把组织根密钥对的派生地址，verify_org_address_binding 可复算
            org_address: Some(org_address.clone()),
            is_public: false,
            // 域类型（org-genesis §3.1）：创建时显式携带，与创世记录一致
            domain_type: Some(domain_type),
            extra: Default::default(),
        };
        record.set_recovery_secret(generate_recovery_secret());
        // orgSecret（org.md §13）：创建时生成，经 extra 动态键随快照在成员间流动
        record.set_org_secret(generate_org_secret());
        // 根私钥加密存 extra（不进快照、不同步出本机）——封存的是创世签名
        // 所用的同一把根密钥对（org.md §15），可对创世记录验签
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
        // 创世策略记录落库（org-genesis §2.1）：`org:genesis:{orgId}`，写一次
        // 不可变；org:structure@v1 键域（versioned.rs 已纳管），随 orgsync 全员
        // 流动。删除组织时按 spec 不清除（创世记录是 orgId 的自认证锚）。
        storage.put(
            &org_genesis_key(&record.org_id),
            &serde_json::to_string(&genesis)?,
        )?;
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

    /// 全域删除守卫（A13 / community-model §4.1，fail-closed 兜底）：**全部
    /// 域类型拒绝删除**——「域只可退出，不可解散；历史保留为只读档案」。
    /// 删除通路已整体移除（前端/壳层/kernel/service 公开入口全拆，无生产
    /// 调用方）；本函数留在 service 层最深处——即使上层重新长出入口，底层
    /// 依然封死。**禁止新增调用方**（唯一消费是守卫单测）。
    pub fn delete_organization_impl<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<()> {
        let _record = Self::require_organization(storage, org_id)?;
        Err(OrgError::CommunityDomainNotDeletable)
    }
}
