//! 标签（设计 §8.2：新建/重命名/删除；删除时从所有资料中摘除）。

use crate::storage::StorageBackend;

use super::*;
use crate::contact::{ContactProfileRecord, FRIEND_PREFIX, org_extra_prefix};

impl ContactService {
    /// 新建标签（id 由调用方给定；kernel 门面以客户端生成的 id 落库）。
    pub fn create_tag_with_id<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        id: &str,
        name: &str,
    ) -> Result<ContactTag> {
        let key = tags_key(space)?;
        let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
        let tag = ContactTag {
            id: id.to_string(),
            name: name.to_string(),
        };
        tags.push(tag.clone());
        write_json(storage, &key, &tags)?;
        Ok(tag)
    }

    /// 重命名标签；不存在时忽略（对齐 TS `renameTag`）。
    pub fn rename_tag<S: StorageBackend>(
        storage: &mut S,
        space: &str,
        tag_id: &str,
        name: &str,
    ) -> Result<()> {
        let key = tags_key(space)?;
        let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
        if let Some(tag) = tags.iter_mut().find(|tag| tag.id == tag_id) {
            tag.name = name.to_string();
            write_json(storage, &key, &tags)?;
        }
        Ok(())
    }

    /// 删除标签并把 tagId 从所有朋友/成员附加资料的 `tag_ids` 中摘除
    /// （对齐 TS `deleteTag`）。
    pub fn delete_tag<S: StorageBackend>(storage: &mut S, space: &str, tag_id: &str) -> Result<()> {
        let key = tags_key(space)?;
        let mut tags: Vec<ContactTag> = read_vec(storage, &key)?;
        tags.retain(|tag| tag.id != tag_id);
        write_json(storage, &key, &tags)?;
        match parse_space(space)? {
            Space::Personal => {
                for (key, mut friend) in scan_json::<S, FriendRecord>(storage, FRIEND_PREFIX)? {
                    let before = friend.tag_ids.len();
                    friend.tag_ids.retain(|id| id != tag_id);
                    if friend.tag_ids.len() != before {
                        write_json(storage, &key, &friend)?;
                    }
                }
            }
            Space::Org(org_id) => {
                let prefix = org_extra_prefix(org_id);
                for (key, mut profile) in scan_json::<S, ContactProfileRecord>(storage, &prefix)? {
                    let before = profile.tag_ids.len();
                    profile.tag_ids.retain(|id| id != tag_id);
                    if profile.tag_ids.len() != before {
                        write_json(storage, &key, &profile)?;
                    }
                }
            }
        }
        Ok(())
    }
}
