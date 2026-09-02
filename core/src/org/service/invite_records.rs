//! 组织邀请记录 CRUD（DM 邀约流程的本地状态；照 contact 好友申请记录风格）。
//!
//! 键：`org:inv:out:{orgId}:{peerRootId}` / `org:inv:in:{orgId}:{peerRootId}`
//! ——同一对 `(orgId, peer)` 只留一条，重复邀请/投递由调用方原地更新（幂等）。

use crate::storage::{ScanOptions, StorageBackend};

use super::super::invite_record::{
    ORG_INV_IN_PREFIX, ORG_INV_OUT_PREFIX, OrgInviteDirection, OrgInviteRecord, OrgInviteStatus,
    org_invite_in_key, org_invite_out_key,
};
use super::super::{OrgError, Result};
use super::OrganizationService;

impl OrganizationService {
    /// 落库一条邀请记录（按键原地覆盖；`updated_at` 兜底取 `created_at`）。
    pub fn put_invite_record<S: StorageBackend>(
        storage: &mut S,
        record: &OrgInviteRecord,
    ) -> Result<()> {
        let mut record = record.clone();
        if record.updated_at == 0 {
            record.updated_at = record.created_at;
        }
        let key = match record.direction {
            OrgInviteDirection::Outgoing => org_invite_out_key(&record.org_id, &record.peer_root_id),
            OrgInviteDirection::Incoming => org_invite_in_key(&record.org_id, &record.peer_root_id),
        };
        storage.put(&key, &serde_json::to_string(&record)?)?;
        Ok(())
    }

    /// pdsync 感知的邀请记录写入（P5）：落 `org:inv:*`。
    ///
    /// **命名陷阱警示（F7，org-invite-scope-fix §2.2）**：版本记账依赖调用方
    /// 句柄——版本化句柄（内核门面路径）经中间件自动记账；**raw 句柄（入站
    /// handler）上本函数沉默无记账**（记录无 pmeta，同步面失明）。raw 调用方
    /// 必须改用显式 `crate::sync::put_personal`（先例：
    /// `kernel/inbound_dm/org_invite.rs` 两 handler）。
    pub fn put_invite_record_pdsync<S: StorageBackend>(
        storage: &mut S,
        record: &OrgInviteRecord,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let _ = (now_ms, node_id); // 记账已下沉中间件，参数保留以稳定签名
        let mut record = record.clone();
        if record.updated_at == 0 {
            record.updated_at = record.created_at;
        }
        let key = match record.direction {
            OrgInviteDirection::Outgoing => org_invite_out_key(&record.org_id, &record.peer_root_id),
            OrgInviteDirection::Incoming => org_invite_in_key(&record.org_id, &record.peer_root_id),
        };
        let json = serde_json::to_string(&record)?;
        storage.put(&key, &json)?;
        Ok(())
    }

    /// 读取出站邀请记录（我邀 `peer_root_id` 加入 `org_id`）；不存在返回 `Ok(None)`。
    pub fn get_outgoing_invite<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        peer_root_id: &str,
    ) -> Result<Option<OrgInviteRecord>> {
        Self::read_invite(storage, &org_invite_out_key(org_id, peer_root_id))
    }

    /// 读取入站邀请记录（`peer_root_id` 邀我加入 `org_id`）；不存在返回 `Ok(None)`。
    pub fn get_incoming_invite<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        peer_root_id: &str,
    ) -> Result<Option<OrgInviteRecord>> {
        Self::read_invite(storage, &org_invite_in_key(org_id, peer_root_id))
    }

    /// 按邀请 id 查入站记录（回应对账用；键以 `(orgId, peer)` 组织，id 只能
    /// 扫描匹配）。不存在返回 `Ok(None)`。
    pub fn find_incoming_invite_by_id<S: StorageBackend>(
        storage: &S,
        invite_id: &str,
    ) -> Result<Option<OrgInviteRecord>> {
        for record in Self::scan_invites(storage, ORG_INV_IN_PREFIX)? {
            if record.id == invite_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    /// 按邀请 id 查任意方向的记录（生成 id 的撞 id 避让用）。
    pub fn find_invite_by_id<S: StorageBackend>(
        storage: &S,
        invite_id: &str,
    ) -> Result<Option<OrgInviteRecord>> {
        for prefix in [ORG_INV_IN_PREFIX, ORG_INV_OUT_PREFIX] {
            for record in Self::scan_invites(storage, prefix)? {
                if record.id == invite_id {
                    return Ok(Some(record));
                }
            }
        }
        Ok(None)
    }

    /// 全部邀请记录（出/入站合并；id 生成的计数种子用）。
    pub fn list_all_invite_records<S: StorageBackend>(
        storage: &S,
    ) -> Result<Vec<OrgInviteRecord>> {
        let mut records = Self::scan_invites(storage, ORG_INV_IN_PREFIX)?;
        records.extend(Self::scan_invites(storage, ORG_INV_OUT_PREFIX)?);
        Ok(records)
    }

    /// 列出指定组织的全部邀请记录（出/入站合并，键升序：先入后出——
    /// `org:inv:in:` 字典序在 `org:inv:out:` 之前）。
    pub fn list_invite_records<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<Vec<OrgInviteRecord>> {
        let mut records = Vec::new();
        for prefix in [ORG_INV_IN_PREFIX, ORG_INV_OUT_PREFIX] {
            for record in Self::scan_invites(storage, prefix)? {
                if record.org_id == org_id {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }

    /// 流转邀请状态：pending → accepted/declined；记录不存在或已在终态
    /// 返回 `Ok(None)`（幂等：终态不重置），成功流转返回更新后的记录。
    pub fn mark_invite_status<S: StorageBackend>(
        storage: &mut S,
        direction: OrgInviteDirection,
        org_id: &str,
        peer_root_id: &str,
        status: OrgInviteStatus,
        now_ms: i64,
    ) -> Result<Option<OrgInviteRecord>> {
        let key = match direction {
            OrgInviteDirection::Outgoing => org_invite_out_key(org_id, peer_root_id),
            OrgInviteDirection::Incoming => org_invite_in_key(org_id, peer_root_id),
        };
        let Some(mut record) = Self::read_invite(storage, &key)? else {
            return Ok(None);
        };
        if record.status != OrgInviteStatus::Pending {
            return Ok(None);
        }
        record.status = status;
        record.updated_at = now_ms;
        storage.put(&key, &serde_json::to_string(&record)?)?;
        Ok(Some(record))
    }

    /// pdsync 感知的状态流转（P5）：落 `org:inv:*` + bump pmeta。
    ///
    /// 命名陷阱同 [`Self::put_invite_record_pdsync`]（F7）：记账依赖调用方
    /// 句柄——raw 句柄（入站 handler）上沉默无记账，raw 调用方改用显式
    /// `crate::sync::put_personal`（org-invite-scope-fix §2.2）。
    pub fn mark_invite_status_pdsync<S: StorageBackend>(
        storage: &mut S,
        direction: OrgInviteDirection,
        org_id: &str,
        peer_root_id: &str,
        status: OrgInviteStatus,
        now_ms: i64,
        node_id: &str,
    ) -> Result<Option<OrgInviteRecord>> {
        let key = match direction {
            OrgInviteDirection::Outgoing => org_invite_out_key(org_id, peer_root_id),
            OrgInviteDirection::Incoming => org_invite_in_key(org_id, peer_root_id),
        };
        let Some(mut record) = Self::read_invite(storage, &key)? else {
            return Ok(None);
        };
        if record.status != OrgInviteStatus::Pending {
            return Ok(None);
        }
        record.status = status;
        record.updated_at = now_ms;
        let json = serde_json::to_string(&record)?;
        // 版本记账由中间件自动完成
        let _ = node_id;
        storage.put(&key, &json)?;
        Ok(Some(record))
    }

    fn read_invite<S: StorageBackend>(storage: &S, key: &str) -> Result<Option<OrgInviteRecord>> {
        let Some(raw) = storage.get(key)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 前缀扫描并逐条反序列化；损坏 JSON 直接报错（与 read_all_organizations 口径一致）。
    fn scan_invites<S: StorageBackend>(storage: &S, prefix: &str) -> Result<Vec<OrgInviteRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(prefix))?;
        rows.into_iter()
            .map(|(_, value)| serde_json::from_str(&value).map_err(OrgError::from))
            .collect()
    }
}

/// F7 存量迁移（org-invite-scope-fix §2.3，升级一次性、幂等）：
/// org:invites 退出 orgsync 后——
///
/// 1. 扫删 `org:inv:in:*` **全部**入站邀请记录及其 pmeta（裸删，不墓碑不进
///    dlog——自有与泄漏记录无法区分（记录无 invitee 字段），且被覆盖设备上
///    的自有 pending 已是粘滞坏态；恢复路径 = 邀请人重发（幂等 upsert 重建
///    干净记录）；自设备各自迁移自清）；
/// 2. 存量 org:invites 声明记录（`org:coll:{orgId}:org:invites@v1`，已随
///    orgsync 流出）墓碑化删除——本地不再驱动 org:invites 的 hello/diff；
///    旧端漂浮的同名声明为空集合（无数据键），无害。墓碑 pmeta 保留既有 vv
///    分量不 bump（迁移不是同步写事件）。
///
/// 幂等：in: 前缀扫空即无操作；声明不存在或已是墓碑即跳过。
/// 返回 (删除的入站记录数, 墓碑化的声明数)。
pub fn migrate_org_invites_out_of_orgsync<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
) -> Result<(usize, usize)> {
    let mut ops = Vec::new();
    // 1. 入站邀请记录 + pmeta 全清
    let mut removed_records = 0usize;
    for (key, _) in storage.scan(&ScanOptions::prefix(ORG_INV_IN_PREFIX))? {
        ops.push(crate::storage::BatchOperation::delete(key.clone()));
        ops.push(crate::storage::BatchOperation::delete(
            crate::sync::personal_meta_key(&key),
        ));
        removed_records += 1;
    }
    // 2. org:invites 声明墓碑化（扫 org:coll: 前缀按集合名过滤）
    let mut tombstoned_decls = 0usize;
    for (key, _) in storage.scan(&ScanOptions::prefix("org:coll:"))? {
        if !key.ends_with(":org:invites@v1") {
            continue;
        }
        let pmeta_key = crate::sync::personal_meta_key(&key);
        // pmeta 直读直解析（缺失/损坏视为无；SyncError 不进 OrgError 通道）
        let pmeta: Option<crate::sync::meta::DocMeta> = storage
            .get(&pmeta_key)?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        if storage.get(&key)?.is_none()
            && pmeta.as_ref().is_none_or(crate::sync::is_tombstone)
        {
            continue; // 已迁移（记录不在且 pmeta 缺/已墓碑）
        }
        ops.push(crate::storage::BatchOperation::delete(key));
        let tombstone = crate::sync::meta::DocMeta {
            vv: pmeta.map(|m| m.vv).unwrap_or_default(),
            ts: now_ms,
            node_id: None,
            tombstone: Some(true),
        };
        ops.push(crate::storage::BatchOperation::put(
            pmeta_key,
            serde_json::to_string(&tombstone)?,
        ));
        tombstoned_decls += 1;
    }
    if !ops.is_empty() {
        storage.batch(ops)?;
    }
    Ok((removed_records, tombstoned_decls))
}
