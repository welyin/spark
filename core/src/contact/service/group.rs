//! 个人空间：扁平分组（数组顺序即显示顺序）。

use crate::storage::StorageBackend;

use super::*;
use crate::contact::{ContactGroup, FRIEND_PREFIX, GROUPS_KEY, ProfilePatch};

impl ContactService {
    /// 新建分组（id 由调用方给定；kernel 门面以客户端生成的 id 落库）。
    pub fn create_group_with_id<S: StorageBackend>(
        storage: &mut S,
        id: &str,
        name: &str,
    ) -> Result<ContactGroup> {
        let mut groups: Vec<ContactGroup> = read_vec(storage, GROUPS_KEY)?;
        let group = ContactGroup {
            id: id.to_string(),
            name: name.to_string(),
        };
        groups.push(group.clone());
        write_json(storage, GROUPS_KEY, &groups)?;
        Ok(group)
    }

    /// 重命名分组；不存在时忽略（对齐 TS `renameGroup`）。
    pub fn rename_group<S: StorageBackend>(
        storage: &mut S,
        group_id: &str,
        name: &str,
    ) -> Result<()> {
        let mut groups: Vec<ContactGroup> = read_vec(storage, GROUPS_KEY)?;
        if let Some(group) = groups.iter_mut().find(|group| group.id == group_id) {
            group.name = name.to_string();
            write_json(storage, GROUPS_KEY, &groups)?;
        }
        Ok(())
    }

    /// 删除分组，组内朋友 `group_id` 复位为 `""`（对齐 TS `deleteGroup`）。
    pub fn delete_group<S: StorageBackend>(storage: &mut S, group_id: &str) -> Result<()> {
        let mut groups: Vec<ContactGroup> = read_vec(storage, GROUPS_KEY)?;
        groups.retain(|group| group.id != group_id);
        write_json(storage, GROUPS_KEY, &groups)?;
        for (key, mut friend) in scan_json::<S, FriendRecord>(storage, FRIEND_PREFIX)? {
            if friend.group_id == group_id {
                friend.group_id = String::new();
                write_json(storage, &key, &friend)?;
            }
        }
        Ok(())
    }

    /// 拖拽重排（对齐 TS `moveGroup`）：`to_index` 以拖拽前原序为准（可等于
    /// 数组长度表示移到末尾），越界夹紧到 `[0, len]`；源在目标位之前时摘除
    /// 后目标索引前移一位（否则「拖到 C 前」会落到 C 后）。不存在时忽略。
    pub fn move_group<S: StorageBackend>(
        storage: &mut S,
        group_id: &str,
        to_index: usize,
    ) -> Result<()> {
        let mut groups: Vec<ContactGroup> = read_vec(storage, GROUPS_KEY)?;
        let Some(from) = groups.iter().position(|group| group.id == group_id) else {
            return Ok(());
        };
        let target = to_index.min(groups.len());
        let moved = groups.remove(from);
        let insert_at = if from < target { target - 1 } else { target };
        groups.insert(insert_at, moved);
        write_json(storage, GROUPS_KEY, &groups)
    }

    /// 设置联系人所属分组（`""` = 未分组；惰性建语义同 `update_profile`）。
    pub fn set_contact_group<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        root_id: &str,
        group_id: &str,
        now_ms: i64,
    ) -> Result<()> {
        Self::update_profile(
            storage,
            space,
            root_id,
            ProfilePatch {
                group_id: Some(group_id.to_string()),
                ..Default::default()
            },
            now_ms,
        )
    }
}
