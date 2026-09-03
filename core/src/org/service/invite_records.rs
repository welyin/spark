//! 组织邀请记录 CRUD（DM 邀约流程的本地状态；照 contact 好友申请记录风格）。
//!
//! 键：`org:inv:out:{orgId}:{peerRootId}` / `org:inv:in:{orgId}:{peerRootId}`
//! ——同一对 `(orgId, peer)` 只留一条，重复邀请/投递由调用方原地更新（幂等）。

use crate::storage::{ScanOptions, StorageBackend};

use super::super::invite_record::{
    ORG_INV_IN_PREFIX, ORG_INV_OUT_PREFIX, OrgInviteDirection, OrgInviteRecord, OrgInviteStatus,
    org_invite_in_key, org_invite_out_key,
};
use super::super::types::OrganizationRecord;
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
            OrgInviteDirection::Outgoing => {
                org_invite_out_key(&record.org_id, &record.peer_root_id)
            }
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
            OrgInviteDirection::Outgoing => {
                org_invite_out_key(&record.org_id, &record.peer_root_id)
            }
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
    pub fn list_all_invite_records<S: StorageBackend>(storage: &S) -> Result<Vec<OrgInviteRecord>> {
        let mut records = Self::scan_invites(storage, ORG_INV_IN_PREFIX)?;
        records.extend(OrganizationService::scan_invites(storage, ORG_INV_OUT_PREFIX)?);
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
/// 1. `org:inv:in:*` **全部**入站邀请记录：删本体 + 写墓碑 pmeta（**vv 保留
///    既有分量不 bump、不登 dlog**——batch1 §3 / f7-review 建议 1 裁决：
///    裸删会留 pdsync 复活窗口（未迁移自设备推回旧记录被判无条件采纳）；
///    墓碑后本地 vv == 远端 vv → Equal 拒收，复活窗口关闭；折叠侧两端 vv
///    一致无 diff，无需 dlog 传播）。自有与泄漏记录无法区分（记录无
///    invitee 字段）；恢复路径 = 邀请人重发（入站写 bump 支配墓碑，正常
///    落库）；
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
    // 1. 入站邀请记录：删本体 + 写墓碑 pmeta（vv 保留不 bump、不登 dlog）
    let mut removed_records = 0usize;
    for (key, _) in storage.scan(&ScanOptions::prefix(ORG_INV_IN_PREFIX))? {
        let pmeta_key = crate::sync::personal_meta_key(&key);
        // pmeta 直读直解析（缺失/损坏视为无；SyncError 不进 OrgError 通道）
        let pmeta: Option<crate::sync::meta::DocMeta> = storage
            .get(&pmeta_key)?
            .and_then(|raw| serde_json::from_str(&raw).ok());
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
        if storage.get(&key)?.is_none() && pmeta.as_ref().is_none_or(crate::sync::is_tombstone) {
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

// ── batch3 §2 管理面邀请投影（org:invitations@v1 / org:invpub:）──────────────

/// 管理面邀请投影键：`org:invpub:{orgId}:{inviterRoot}:{inviteeRoot}`
/// （**双维度定键**——F7 的碰撞缺陷在键形上根除：同邀请人多人、多邀请人
/// 同一人均不撞）。
pub fn org_invpub_key(org_id: &str, inviter_root: &str, invitee_root: &str) -> String {
    format!("org:invpub:{org_id}:{inviter_root}:{invitee_root}")
}

/// 出站邀请记录的管理面投影（值 = 公开元数据，**不含 inviteCode**——出站
/// 记录本就不存邀请码，投影维持此边界）。仅 Outgoing 方向有投影（入站记录
/// 是 invitee 私有应答状态，F7 裁决不进 orgsync）。
pub fn invpub_projection(
    record: &OrgInviteRecord,
    inviter_root: &str,
) -> Option<(String, serde_json::Value)> {
    if record.direction != OrgInviteDirection::Outgoing {
        return None;
    }
    let key = org_invpub_key(&record.org_id, inviter_root, &record.peer_root_id);
    Some((
        key,
        serde_json::json!({
            "inviter": inviter_root,
            "invitee": record.peer_root_id,
            "status": record.status,
            "createdAt": record.created_at,
            "updatedAt": record.updated_at,
        }),
    ))
}

/// 邀请发送/状态流转的双写：原出站记录 + 管理面投影，同一 batch（版本化
/// 句柄一次 batch 两键，中间件逐键记账，原子）。
pub fn put_invite_record_with_projection<S: StorageBackend>(
    storage: &mut S,
    record: &OrgInviteRecord,
    inviter_root: &str,
) -> Result<()> {
    let mut ops = Vec::new();
    let mut r = record.clone();
    if r.updated_at == 0 {
        r.updated_at = r.created_at;
    }
    ops.push(crate::storage::BatchOperation::put(
        org_invite_out_key_of(&r),
        serde_json::to_string(&r)?,
    ));
    if let Some((key, projection)) = invpub_projection(&r, inviter_root) {
        ops.push(crate::storage::BatchOperation::put(
            key,
            serde_json::to_string(&projection)?,
        ));
    }
    storage.batch(ops)?;
    Ok(())
}

fn org_invite_out_key_of(record: &OrgInviteRecord) -> String {
    super::super::invite_record::org_invite_out_key(&record.org_id, &record.peer_root_id)
}

/// F4 第二层（batch3 §1.2 成员表对账兜底）：org:meta / org:member 合入后
/// 发现「某 outbound pending 邀请的 invitee 已在成员表」⟹ 其必已接受
/// ——原地标 accepted + 投影同步（batch3 §2：对账置 accepted 时同步投影）。
/// 返回被对账的记录（事件由调用方发 OrgInviteUpdated）。
///
/// 触发条件（裁决 §10.2，org-p2-channel-review 建议 1——收紧「在成员表
/// ⟹ 已接受」的预录模型冲突）：
/// 1. 合入 applied=true（调用方保证，仅合入生效时调用）；
/// 2. 合入后 invitee 在成员表（本函数检查）；
/// 3. `incoming_vv`（**本次合入记录**的 meta.vv，非合并落库结果）至少一个
///    分量键 ∈ invitee 已知端点 peerId 集（取自本地成员表 node_info）——
///    vv 分量 = 写入设备，命中 ⟹ 该记录由 invitee 本人设备写过 ⟹ invitee
///    真走了接受编排（自写条目只在 accept 路径产生）。预录/中继类记录
///    （管理员双写产物、他人中继）只含他人分量 → 不误标；终态不重置
///    语义不变（前提收紧后 declined 回执不再被误标记录挡在门外）。
/// 已知保守边界：invitee 换设备（peerId 漂移）且新端点尚未被本地知晓时
/// 不触发——pending 停留，由第一层补投通道与端点刷新后收敛兜底。
///
/// 记账：派生记账是本机事实（成员表是事实源，回执只是通知）——入站 raw
/// 句柄上显式 put_personal（F7 先例），投影同口径。
pub fn reconcile_outbound_invites_with_members<S: StorageBackend>(
    storage: &mut S,
    record: &OrganizationRecord,
    incoming_vv: &crate::sync::meta::VersionVector,
    now_ms: i64,
    node_id: &str,
    inviter_root: &str,
) -> Result<Vec<OrgInviteRecord>> {
    let mut reconciled = Vec::new();
    for inv in OrganizationService::scan_invites(storage, ORG_INV_OUT_PREFIX)? {
        if inv.org_id != record.org_id || inv.status != OrgInviteStatus::Pending {
            continue;
        }
        let Some(member) = record.find_member(&inv.peer_root_id) else {
            continue;
        };
        // 裁决 §10.2 条件 3：合入记录 vv 分量 ∩ invitee 已知端点 peerId 集
        let self_written = member.node_info.as_ref().is_some_and(|set| {
            set.iter()
                .filter_map(|e| e.peer_id.as_deref())
                .any(|peer| incoming_vv.contains_key(peer))
        });
        if !self_written {
            continue; // 预录/中继记录（无 invitee 本人分量）不误标
        }
        let mut inv = inv;
        inv.status = OrgInviteStatus::Accepted;
        inv.updated_at = now_ms;
        // 原记录（personal 域）+ 投影（orgsync 管理面）同口径显式记账
        let sync_err = |e: crate::sync::SyncError| {
            OrgError::Storage(crate::storage::StorageError::Backend(e.to_string()))
        };
        crate::sync::put_personal(
            storage,
            node_id,
            &org_invite_out_key_of(&inv),
            &serde_json::to_string(&inv)?,
            now_ms,
        )
        .map_err(sync_err)?;
        if let Some((key, projection)) = invpub_projection(&inv, inviter_root) {
            crate::sync::put_personal(storage, node_id, &key, &serde_json::to_string(&projection)?, now_ms)
                .map_err(sync_err)?;
        }
        reconciled.push(inv);
    }
    Ok(reconciled)
}
