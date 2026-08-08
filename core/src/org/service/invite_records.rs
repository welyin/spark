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

    /// pdsync 感知的邀请记录写入（P5）：`put_personal` 落 `org:inv:*` + bump
    /// pmeta，使邀请记录可经自设备 pdsync 同步。
    pub fn put_invite_record_pdsync<S: StorageBackend>(
        storage: &mut S,
        record: &OrgInviteRecord,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let mut record = record.clone();
        if record.updated_at == 0 {
            record.updated_at = record.created_at;
        }
        let key = match record.direction {
            OrgInviteDirection::Outgoing => org_invite_out_key(&record.org_id, &record.peer_root_id),
            OrgInviteDirection::Incoming => org_invite_in_key(&record.org_id, &record.peer_root_id),
        };
        let json = serde_json::to_string(&record)?;
        crate::sync::put_personal(storage, node_id, &key, &json, now_ms).map_err(|e| {
            OrgError::Json(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )))
        })?;
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
        crate::sync::put_personal(storage, node_id, &key, &json, now_ms).map_err(|e| {
            OrgError::Json(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )))
        })?;
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
