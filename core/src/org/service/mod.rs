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
//!
//! 代码组织：本文件为 [`OrganizationService`] 门面、公开输入/返回类型与各变更
//! 路径共用的私有辅助（记录读写、admin 校验、sync 重建）；创建/删除在 `create`，
//! 成员增删与各类视图在 `members`，邀请码生成/接受确认在 `invites`，入站数据
//! 落库（快照/nodeInfoClaim）在 `snapshot_apply`，网关与公开标志在 `settings`；
//! 单测按域拆在 `tests/`。

mod create;
mod invite_records;
mod invites;
mod members;
mod settings;
mod snapshot_apply;

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

use super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::types::{
    ORG_META_PREFIX, OrganizationNodeInfo, OrganizationRecord, OrganizationSyncState,
    organization_key,
};
use super::{OrgError, Result};

/// 创建组织输入（types.ts:95-99）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateOrganizationInput {
    /// 组织名（trim + 连续空白归一）。
    pub name: String,
    /// 描述（trim，可省）。
    pub description: Option<String>,
    /// 组织 logo（`data:image/` data URL，可省；空白等同未设置，非空时按
    /// `identity::validate_avatar` 同口径校验）。
    pub avatar: Option<String>,
    /// 基础插件域（`plugin:` 前缀，可省——组织与插件不再强关联，设计 §7.2）。
    pub base_plugin_domain: Option<String>,
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

/// `updateMyIdentity` 的身份补丁（成员更新自己的组织内身份字段）。
///
/// 语义对齐 identity `update_profile`：字符串字段 `None` 不变；
/// `avatar` 三态（`Some(Some)` 设置 / `Some(None)` 清除 / `None` 不变）；
/// `gender`/`region`/`signature` `Some("")`（或全空白）= 清除。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrgIdentityPatch {
    /// 昵称（trim 后 1–24 字符；`None` 不变，不可清除）。
    pub nickname: Option<String>,
    /// 头像 data URL（三态；见上）。
    pub avatar: Option<Option<String>>,
    /// 性别（≤16 字符；`Some("")` 清除）。
    pub gender: Option<String>,
    /// 地区（≤64 字符；`Some("")` 清除）。
    pub region: Option<String>,
    /// 个性签名（≤128 字符；`Some("")` 清除）。
    pub signature: Option<String>,
    /// 是否在组织内展示个人身份（`None` 不变）。
    pub use_personal_identity: Option<bool>,
}

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
    pub fn save_record<S: StorageBackend>(
        storage: &mut S,
        record: &OrganizationRecord,
    ) -> Result<()> {
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

/// 三态字段（`Option<Option<&str>>`，如 avatar）的事务审计摘要（m1：payload
/// 不落完整内容——data URL 序列化可达 200KB）：`None` 未变更 → Null；
/// `Some(None)` 清除 → `false`；`Some(Some(_))` 设置 → 仅记内容长度。
fn tri_state_audit(value: Option<Option<&str>>) -> Value {
    match value {
        None => Value::Null,
        Some(None) => Value::from(false),
        Some(Some(content)) => Value::from(content.len() as i64),
    }
}

/// 空串清除字段（`Option<&str>`，空白 = 清除）的事务审计摘要（m1，口径同上）：
/// `None` 未变更 → Null；空白清除 → `false`；非空设置 → 仅记内容长度。
fn clearable_audit(value: Option<&str>) -> Value {
    match value {
        None => Value::Null,
        Some(content) if content.trim().is_empty() => Value::from(false),
        Some(content) => Value::from(content.len() as i64),
    }
}
