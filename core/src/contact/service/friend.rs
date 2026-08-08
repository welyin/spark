//! 空间通讯录视图、个人空间朋友记录与本地资料（个人 friend 条目 / 组织成员
//! 附加资料）。

use std::collections::BTreeMap;

use crate::storage::StorageBackend;
use crate::sync::personal::put_personal;

use super::*;
use crate::contact::{
    BLOCKED_PREFIX, ContactProfileRecord, ContactTag, ContactGroup,
    FRIEND_PREFIX, FriendRecord, FriendRequestRecord,
    GROUP_PREFIX, ProfilePatch, REQ_IN_PREFIX, REQ_OUT_PREFIX, SpaceContactsView,
    TAG_PREFIX, org_extra_prefix, org_tree_key, sync_err_to_contact,
};

impl ContactService {
    // ------------------------------------------------------------------
    // 视图
    // ------------------------------------------------------------------

    /// 空间通讯录总览（对齐 TS `SpaceContacts` 形状）。
    ///
    /// 个人空间：`group_tree`/`member_extras` 为空；组织空间：
    /// `friends`/`requests`/`groups` 为空（成员名单本身来自组织模块，
    /// 「新的成员」入站申请走邀请码流程，不入库；`outgoing` 为我发出的
    /// 邀请记录 `ct:org:{orgId}:req:out:`，跨重启水合回来）。
    ///
    /// 个人空间 friends 的 `blocked` 字段以拉黑集合（`ct:blocked:`）为准
    /// overlay（friend 记录上的同名字段仅是展示镜像，集合才是事实来源）。
    pub fn overview<S: StorageBackend>(storage: &S, space: &str) -> Result<SpaceContactsView> {
        match parse_space(space)? {
            Space::Personal => {
                let blocked = blocked_set(storage)?;
                Ok(SpaceContactsView {
                    friends: scan_json::<S, FriendRecord>(storage, FRIEND_PREFIX)?
                        .into_iter()
                        .map(|(_, mut record)| {
                            record.blocked = blocked.contains(&record.root_id);
                            record
                        })
                        .collect(),
                    requests: scan_json::<S, FriendRequestRecord>(storage, REQ_IN_PREFIX)?
                        .into_iter()
                        .map(|(_, record)| record)
                        .collect(),
                    outgoing: scan_json::<S, FriendRequestRecord>(storage, REQ_OUT_PREFIX)?
                        .into_iter()
                        .map(|(_, record)| record)
                        .collect(),
                    tags: {
                        let mut tags: Vec<ContactTag> =
                            scan_json::<S, ContactTag>(storage, TAG_PREFIX)?
                                .into_iter()
                                .map(|(_, t)| t)
                                .collect();
                        tags.sort_by_key(|t| t.order);
                        tags
                    },
                    groups: {
                        let mut groups: Vec<ContactGroup> =
                            scan_json::<S, ContactGroup>(storage, GROUP_PREFIX)?
                                .into_iter()
                                .map(|(_, g)| g)
                                .collect();
                        groups.sort_by_key(|g| g.order);
                        groups
                    },
                    ..Default::default()
                })
            }
            Space::Org(org_id) => {
                let member_extras: BTreeMap<String, ContactProfileRecord> =
                    scan_json::<S, ContactProfileRecord>(storage, &org_extra_prefix(org_id))?
                        .into_iter()
                        .map(|(key, record)| {
                            let root_id = key[org_extra_prefix(org_id).len()..].to_string();
                            (root_id, record)
                        })
                        .collect();
                Ok(SpaceContactsView {
                    outgoing: Self::list_org_outgoing_requests(storage, org_id)?,
                    tags: read_vec(storage, &org_tags_key(org_id))?,
                    group_tree: read_vec(storage, &org_tree_key(org_id))?,
                    member_extras,
                    ..Default::default()
                })
            }
        }
    }

    // ------------------------------------------------------------------
    // 朋友（个人空间）
    // ------------------------------------------------------------------

    /// 读取朋友记录；不存在返回 `Ok(None)`。
    pub fn get_friend<S: StorageBackend>(
        storage: &S,
        root_id: &str,
    ) -> Result<Option<FriendRecord>> {
        read_json(storage, &format!("{FRIEND_PREFIX}{root_id}"))
    }

    /// 新增或覆盖朋友记录（upsert，对齐 TS `addFriend` 后写资料的组合语义）。
    pub fn upsert_friend<S: StorageBackend>(storage: &mut S, friend: &FriendRecord) -> Result<()> {
        write_json(storage, &format!("{FRIEND_PREFIX}{}", friend.root_id), friend)
    }

    /// 删除朋友（设计 §5.5：只删关系，不清拉黑状态——mock 下直接移除条目）。
    pub fn remove_friend<S: StorageBackend>(storage: &mut S, root_id: &str) -> Result<()> {
        storage.delete(&format!("{FRIEND_PREFIX}{root_id}"))?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 本地资料（个人 friend 条目 / 组织成员附加资料）
    // ------------------------------------------------------------------

    /// 更新本地资料（对齐 TS `updateProfile`：`Object.assign(profile, patch)`）。
    ///
    /// 个人空间改 friend 条目，不存在报 [`ContactError::ContactNotFound`]；
    /// 组织空间惰性创建默认附加资料后打补丁（对齐 TS `profileOf` 惰性建）。
    pub fn update_profile<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        root_id: &str,
        patch: ProfilePatch,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                let mut friend =
                    Self::get_friend(storage, root_id)?.ok_or(ContactError::ContactNotFound)?;
                apply_patch_to_friend(&mut friend, &patch);
                // 本机资料变更时间：自设备 contact-sync 的 LWW 依据
                friend.updated_at = now_ms;
                Self::upsert_friend_pdsync(storage, &friend, now_ms, node_id)
            }
            Space::Org(org_id) => {
                let key = format!("{}{}", org_extra_prefix(org_id), root_id);
                let mut profile: ContactProfileRecord =
                    read_json(storage, &key)?.unwrap_or_default();
                apply_patch_to_profile(&mut profile, &patch);
                // P5：组织成员附加资料走 pmeta，供自设备 pdsync
                put_personal(storage, node_id, &key, &serde_json::to_string(&profile)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                Ok(())
            }
        }
    }

    /// 设置联系人所属分组（`""` = 未分组）。
    ///
    /// 个人空间：改 friend 记录的 `group_id` 并经 pdsync 落盘（P1）。
    /// 组织空间：改成员附加资料的 `group_id`（组织数据，暂不走 pdsync）。
    pub fn set_contact_group<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        root_id: &str,
        group_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                let mut friend =
                    Self::get_friend(storage, root_id)?.ok_or(ContactError::ContactNotFound)?;
                friend.group_id = group_id.to_string();
                friend.updated_at = now_ms;
                Self::upsert_friend_pdsync(storage, &friend, now_ms, node_id)
            }
            Space::Org(org_id) => {
                let key = format!("{}{}", org_extra_prefix(org_id), root_id);
                let mut profile: ContactProfileRecord =
                    read_json(storage, &key)?.unwrap_or_default();
                profile.group_id = group_id.to_string();
                // P5：组织成员附加资料走 pmeta
                put_personal(storage, node_id, &key, &serde_json::to_string(&profile)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                Ok(())
            }
        }
    }

    /// 设置/取消拉黑（设计 §7.4：第一期应用层拉黑）。
    ///
    /// 个人空间：事实来源是独立拉黑集合 `ct:blocked:{rootId}`（陌生人也可
    /// 拉黑、删除朋友不清拉黑）；friend 记录存在时同步其 `blocked` 字段作
    /// 展示镜像。组织空间：成员附加资料 blocked（惰性建语义同 `update_profile`）。
    pub fn set_blocked<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        root_id: &str,
        blocked: bool,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                Self::set_blocked_pdsync(storage, root_id, blocked, now_ms, node_id)?;
                // 展示镜像：friend 记录存在时同步 blocked 字段（overview
                // 以集合为准 overlay，此处仅为记录内字段一致）；同时刷新
                // updatedAt 使镜像变更随 contact-sync 传播
                if let Some(mut friend) = Self::get_friend(storage, root_id)? {
                    friend.blocked = blocked;
                    friend.updated_at = now_ms;
                    Self::upsert_friend_pdsync(storage, &friend, now_ms, node_id)?;
                }
                // 拉黑集合整域版本（contact-sync LWW 依据；取消拉黑=删除
                // 条目，随整域替换传播）
                super::sync::bump_version(storage, super::sync::SyncDomain::Blocked, now_ms)?;
                Ok(())
            }
            Space::Org(org_id) => {
                let key = format!("{}{}", org_extra_prefix(org_id), root_id);
                let mut profile: ContactProfileRecord =
                    read_json(storage, &key)?.unwrap_or_default();
                profile.blocked = blocked;
                // P5：组织成员附加资料走 pmeta
                put_personal(storage, node_id, &key, &serde_json::to_string(&profile)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                Ok(())
            }
        }
    }

    /// 个人空间拉黑判定（查独立集合；陌生人亦可被拉黑）。
    pub fn is_blocked<S: StorageBackend>(storage: &S, root_id: &str) -> Result<bool> {
        Ok(storage.get(&format!("{BLOCKED_PREFIX}{root_id}"))?.is_some())
    }

    /// 读取组织成员的本地附加资料；不存在返回 `Ok(None)`（入站拉黑判定等
    /// 只读场景用，不惰性创建）。
    pub fn get_org_profile<S: StorageBackend>(
        storage: &S,
        org_id: &str,
        root_id: &str,
    ) -> Result<Option<ContactProfileRecord>> {
        read_json(storage, &format!("{}{}", org_extra_prefix(org_id), root_id))
    }
}

/// 个人空间拉黑集合（rootId 集合）。
fn blocked_set<S: StorageBackend>(storage: &S) -> Result<std::collections::HashSet<String>> {
    let rows = storage.scan(&crate::storage::ScanOptions::prefix(BLOCKED_PREFIX))?;
    Ok(rows
        .into_iter()
        .map(|(key, _)| key[BLOCKED_PREFIX.len()..].to_string())
        .collect())
}
