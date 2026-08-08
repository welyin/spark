//! 分组服务：个人空间扁平分组存为独立记录 `ct:group:{groupId}`。

use crate::storage::StorageBackend;
use crate::sync::{delete_personal, put_personal};

use super::super::{ContactGroup, FRIEND_PREFIX, GROUP_PREFIX, GROUPS_KEY, Result, sync_err_to_contact};
use super::{ContactService, read_json, scan_json};

impl ContactService {
    /// 新建分组（id 由调用方给定）。
    pub fn create_group_with_id<S: StorageBackend>(
        storage: &mut S,
        id: &str,
        name: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<ContactGroup> {
        let max_order = Self::list_groups(storage)?
            .iter()
            .map(|g| g.order)
            .max()
            .unwrap_or(-1);
        let group = ContactGroup {
            id: id.to_string(),
            name: name.to_string(),
            order: max_order + 1,
        };
        let key = format!("{GROUP_PREFIX}{id}");
        let json = serde_json::to_string(&group)?;
        put_personal(storage, node_id, &key, &json, now_ms)
            .map_err(sync_err_to_contact)?;
        super::sync::bump_version(storage, super::sync::SyncDomain::Groups, now_ms)?;
        Ok(group)
    }

    /// 重命名分组。
    pub fn rename_group<S: StorageBackend>(
        storage: &mut S,
        group_id: &str,
        name: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let key = format!("{GROUP_PREFIX}{group_id}");
        let Some(mut group) = read_json::<S, ContactGroup>(storage, &key)? else {
            return Ok(());
        };
        group.name = name.to_string();
        let json = serde_json::to_string(&group)?;
        put_personal(storage, node_id, &key, &json, now_ms)
            .map_err(sync_err_to_contact)?;
        super::sync::bump_version(storage, super::sync::SyncDomain::Groups, now_ms)?;
        Ok(())
    }

    /// 删除分组（组内朋友复位为未分组）。
    pub fn delete_group<S: StorageBackend>(
        storage: &mut S,
        group_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let key = format!("{GROUP_PREFIX}{group_id}");
        // tombstone
        if storage.get(&key)?.is_some() {
            delete_personal(storage, node_id, &key, now_ms)
                .map_err(sync_err_to_contact)?;
            super::sync::bump_version(storage, super::sync::SyncDomain::Groups, now_ms)?;
        }
        // 复位组内朋友（走 pmeta + 刷新 updatedAt，双通道传播，同 delete_tag）
        for (friend_key, mut friend) in
            scan_json::<S, super::super::FriendRecord>(storage, FRIEND_PREFIX)?
        {
            if friend.group_id == group_id {
                friend.group_id = String::new();
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

    /// 拖拽重排分组（越界夹紧）。
    pub fn move_group<S: StorageBackend>(
        storage: &mut S,
        group_id: &str,
        to_index: usize,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let mut groups = Self::list_groups(storage)?;
        let Some(from) = groups.iter().position(|g| g.id == group_id) else {
            return Ok(());
        };
        // 对齐 TS `moveGroup` 与 `move_org_group_sibling`：to_index 以拖拽前
        // 原序为准（可等于长度表示移到末尾），越界夹紧到 [0, len]；源在目标位
        // 之前时摘除后目标索引前移一位——否则「拖到 C 前」会落到 C 后
        let target = to_index.min(groups.len());
        let group = groups.remove(from);
        let insert_at = if from < target { target - 1 } else { target };
        groups.insert(insert_at, group);
        // 重新分配 order 并逐条落盘
        for (i, g) in groups.iter_mut().enumerate() {
            g.order = i as i32;
            let key = format!("{GROUP_PREFIX}{}", g.id);
            let json = serde_json::to_string(&g)?;
            put_personal(storage, node_id, &key, &json, now_ms)
                .map_err(sync_err_to_contact)?;
        }
        super::sync::bump_version(storage, super::sync::SyncDomain::Groups, now_ms)?;
        Ok(())
    }

    /// 列出所有分组（按 order 升序）。
    pub fn list_groups<S: StorageBackend>(storage: &S) -> Result<Vec<ContactGroup>> {
        let mut groups: Vec<ContactGroup> = scan_json(storage, GROUP_PREFIX)?
            .into_iter()
            .map(|(_, g)| g)
            .collect();
        groups.sort_by_key(|g| g.order);
        Ok(groups)
    }
}

// ── 迁移 ────────────────────────────────────────────────────────────

impl ContactService {
    /// 将旧格式 `ct:groups`（数组）迁移为新格式 `ct:group:{groupId}`（独立记录）。
    ///
    /// 幂等：如果 `ct:groups` 不存在则空操作。
    /// 迁移后删除旧键。
    pub fn migrate_groups_to_items<S: StorageBackend>(
        storage: &mut S,
        node_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(raw) = storage.get(GROUPS_KEY)? else {
            return Ok(false);
        };
        let groups: Vec<ContactGroup> = serde_json::from_str(&raw).unwrap_or_default();
        for (i, mut group) in groups.into_iter().enumerate() {
            group.order = i as i32;
            let key = format!("{GROUP_PREFIX}{}", group.id);
            let json = serde_json::to_string(&group)?;
            put_personal(storage, node_id, &key, &json, now_ms)
                .map_err(sync_err_to_contact)?;
        }
        storage.delete(GROUPS_KEY)?;
        super::sync::bump_version(storage, super::sync::SyncDomain::Groups, now_ms)?;
        Ok(true)
    }
}
