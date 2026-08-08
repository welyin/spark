//! 标签服务：个人空间标签存为独立记录 `ct:tag:{tagId}`（组织空间仍为数组）。

use crate::storage::StorageBackend;
use crate::sync::{delete_personal, put_personal};

use super::super::{ContactTag, Result, TAG_PREFIX, TAGS_KEY, org_tags_key, FRIEND_PREFIX, sync_err_to_contact};
use super::{ContactService, parse_space, read_json, read_vec, scan_json, Space};

impl ContactService {
    /// 新建标签（id 由调用方给定）。
    pub fn create_tag_with_id<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        id: &str,
        name: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<ContactTag> {
        match parse_space(space)? {
            Space::Personal => {
                // 个人空间：独立记录 `ct:tag:{id}`，order = 当前最大 order + 1
                let max_order = Self::list_tags(storage)?
                    .iter()
                    .map(|t| t.order)
                    .max()
                    .unwrap_or(-1);
                let tag = ContactTag {
                    id: id.to_string(),
                    name: name.to_string(),
                    order: max_order + 1,
                };
                let key = format!("{TAG_PREFIX}{id}");
                let json = serde_json::to_string(&tag)?;
                put_personal(storage, node_id, &key, &json, now_ms)
                    .map_err(sync_err_to_contact)?;
                super::sync::bump_version(storage, super::sync::SyncDomain::Tags, now_ms)?;
                Ok(tag)
            }
            Space::Org(org_id) => {
                let key = org_tags_key(org_id);
                let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
                let tag = ContactTag {
                    id: id.to_string(),
                    name: name.to_string(),
                    order: tags.len() as i32,
                };
                tags.push(tag.clone());
                // P5：组织标签整域单记录同步（写 pmeta，供自设备 pdsync）
                put_personal(storage, node_id, &key, &serde_json::to_string(&tags)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                Ok(tag)
            }
        }
    }

    /// 重命名标签。
    pub fn rename_tag<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        tag_id: &str,
        name: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                let key = format!("{TAG_PREFIX}{tag_id}");
                let Some(mut tag) = read_json::<S, ContactTag>(storage, &key)? else {
                    return Ok(());
                };
                tag.name = name.to_string();
                let json = serde_json::to_string(&tag)?;
                put_personal(storage, node_id, &key, &json, now_ms)
                    .map_err(sync_err_to_contact)?;
                super::sync::bump_version(storage, super::sync::SyncDomain::Tags, now_ms)?;
                Ok(())
            }
            Space::Org(org_id) => {
                let key = org_tags_key(org_id);
                let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
                if let Some(tag) = tags.iter_mut().find(|tag| tag.id == tag_id) {
                    tag.name = name.to_string();
                    put_personal(storage, node_id, &key, &serde_json::to_string(&tags)?, now_ms)
                        .map_err(sync_err_to_contact)?;
                }
                Ok(())
            }
        }
    }

    /// 删除标签（并从所有资料中摘除对其的引用）。
    pub fn delete_tag<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        tag_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                let key = format!("{TAG_PREFIX}{tag_id}");
                // tombstone pmeta + 删除记录本体
                // 只有记录存在时才 tombstone（避免空删产生垃圾 pmeta）。
                if storage.get(&key)?.is_some() {
                    delete_personal(storage, node_id, &key, now_ms)
                        .map_err(sync_err_to_contact)?;
                    super::sync::bump_version(storage, super::sync::SyncDomain::Tags, now_ms)?;
                }
                // 摘除所有朋友记录中的该标签引用（走 pmeta + 刷新 updatedAt，
                // 引用变更才能随 pdsync 与旧 contact-sync 双通道传播）
                for (friend_key, mut friend) in
                    scan_json::<S, super::super::FriendRecord>(storage, FRIEND_PREFIX)?
                {
                    let before = friend.tag_ids.len();
                    friend.tag_ids.retain(|id| id != tag_id);
                    if friend.tag_ids.len() != before {
                        friend.updated_at = now_ms;
                        put_personal(
                            storage,
                            node_id,
                            &friend_key,
                            &serde_json::to_string(&friend)?,
                            now_ms,
                        )
                        .map_err(sync_err_to_contact)?;
                    }
                }
                Ok(())
            }
            Space::Org(org_id) => {
                let key = org_tags_key(org_id);
                let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
                tags.retain(|tag| tag.id != tag_id);
                put_personal(storage, node_id, &key, &serde_json::to_string(&tags)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                // 摘除组织空间引用（成员附加资料走 pmeta，P5）
                let extra_prefix = super::super::org_extra_prefix(org_id);
                for (profile_key, mut profile) in
                    scan_json::<S, super::super::ContactProfileRecord>(storage, &extra_prefix)?
                {
                    let before = profile.tag_ids.len();
                    profile.tag_ids.retain(|id| id != tag_id);
                    if profile.tag_ids.len() != before {
                        put_personal(
                            storage,
                            node_id,
                            &profile_key,
                            &serde_json::to_string(&profile)?,
                            now_ms,
                        )
                        .map_err(sync_err_to_contact)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// 拖拽重排标签（目标位置越界夹紧）。
    ///
    /// 个人空间：调整各个标签的 `order` 字段后逐条落盘。
    pub fn reorder_tags<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        tag_id: &str,
        to_index: usize,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        match parse_space(space)? {
            Space::Personal => {
                let mut tags = Self::list_tags(storage)?;
                let Some(from) = tags.iter().position(|tag| tag.id == tag_id) else {
                    return Ok(());
                };
                let tag = tags.remove(from);
                let to = to_index.min(tags.len());
                tags.insert(to, tag);
                // 重新分配 order 并逐条落盘
                for (i, tag) in tags.iter_mut().enumerate() {
                    tag.order = i as i32;
                    let key = format!("{TAG_PREFIX}{}", tag.id);
                    let json = serde_json::to_string(&tag)?;
                    put_personal(storage, node_id, &key, &json, now_ms)
                        .map_err(sync_err_to_contact)?;
                }
                super::sync::bump_version(storage, super::sync::SyncDomain::Tags, now_ms)?;
                Ok(())
            }
            Space::Org(org_id) => {
                let key = org_tags_key(org_id);
                let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
                let Some(from) = tags.iter().position(|tag| tag.id == tag_id) else {
                    return Ok(());
                };
                let tag = tags.remove(from);
                let to = to_index.min(tags.len());
                tags.insert(to, tag);
                put_personal(storage, node_id, &key, &serde_json::to_string(&tags)?, now_ms)
                    .map_err(sync_err_to_contact)?;
                Ok(())
            }
        }
    }

    /// 列出所有标签（按 order 升序）。
    pub fn list_tags<S: StorageBackend>(storage: &S) -> Result<Vec<ContactTag>> {
        let mut tags: Vec<ContactTag> = scan_json(storage, TAG_PREFIX)?
            .into_iter()
            .map(|(_, tag)| tag)
            .collect();
        tags.sort_by_key(|t| t.order);
        Ok(tags)
    }
}

// ── 迁移 ────────────────────────────────────────────────────────────

impl ContactService {
    /// 将旧格式 `ct:tags`（数组）迁移为新格式 `ct:tag:{tagId}`（独立记录）。
    ///
    /// 幂等：如果 `ct:tags` 不存在则空操作。
    /// 迁移后删除旧键。
    pub fn migrate_tags_to_items<S: StorageBackend>(
        storage: &mut S,
        node_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(raw) = storage.get(TAGS_KEY)? else {
            return Ok(false);
        };
        let tags: Vec<ContactTag> = serde_json::from_str(&raw).unwrap_or_default();
        for (i, mut tag) in tags.into_iter().enumerate() {
            tag.order = i as i32;
            let key = format!("{TAG_PREFIX}{}", tag.id);
            let json = serde_json::to_string(&tag)?;
            put_personal(storage, node_id, &key, &json, now_ms)
                .map_err(sync_err_to_contact)?;
        }
        storage.delete(TAGS_KEY)?;
        super::sync::bump_version(storage, super::sync::SyncDomain::Tags, now_ms)?;
        Ok(true)
    }
}
