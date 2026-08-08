//! 通讯录服务层（对齐 app/src/mock/contacts.ts 同名函数语义）。
//!
//! 纯逻辑层：无状态 Service，全部静态方法，只操作 [`StorageBackend`]。
//! 按职责拆分子模块：friend（视图/朋友/本地资料）、request（好友申请）、
//! tag（标签）、group（个人扁平分组）、org_group（组织分组树）。

mod friend;
mod group;
mod org_group;
mod request;
pub(crate) mod sync;
mod tag;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::storage::{ScanOptions, StorageBackend};

use super::{
    ContactError, ContactProfileRecord, ContactTag, FriendRecord, ProfilePatch, Result, TAGS_KEY,
    org_tags_key,
};

/// 通讯录服务（无状态；全部方法以存储与参数为输入）。
pub struct ContactService;

/// 解析后的空间：个人或某个组织。
enum Space<'a> {
    Personal,
    Org(&'a str),
}

fn parse_space(space: &str) -> Result<Space<'_>> {
    if space == "personal" {
        return Ok(Space::Personal);
    }
    match space.strip_prefix("org:") {
        Some(org_id) if !org_id.is_empty() => Ok(Space::Org(org_id)),
        _ => Err(ContactError::InvalidSpace),
    }
}

/// 期望组织空间时调用；个人空间或非法标识报 [`ContactError::InvalidSpace`]。
fn require_org_space(space: &str) -> Result<&str> {
    match parse_space(space)? {
        Space::Org(org_id) => Ok(org_id),
        Space::Personal => Err(ContactError::InvalidSpace),
    }
}

fn read_json<S: StorageBackend, T: DeserializeOwned>(
    storage: &S,
    key: &str,
) -> Result<Option<T>> {
    let Some(raw) = storage.get(key)? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(&raw)?))
}

fn write_json<S: StorageBackend, T: Serialize>(storage: &mut S, key: &str, value: &T) -> Result<()> {
    storage.put(key, &serde_json::to_string(value)?)?;
    Ok(())
}

/// 读取数组键；缺省返回空数组（对齐 TS `space.tags` 等恒为数组）。
fn read_vec<S: StorageBackend, T: DeserializeOwned>(storage: &S, key: &str) -> Result<Vec<T>> {
    Ok(read_json(storage, key)?.unwrap_or_default())
}

/// 前缀扫描并逐条反序列化；损坏 JSON 直接报错（不静默跳过，对齐 org 做法）。
fn scan_json<S: StorageBackend, T: DeserializeOwned>(
    storage: &S,
    prefix: &str,
) -> Result<Vec<(String, T)>> {
    let rows = storage.scan(&ScanOptions::prefix(prefix))?;
    rows.into_iter()
        .map(|(key, value)| {
            serde_json::from_str(&value)
                .map(|record| (key, record))
                .map_err(ContactError::from)
        })
        .collect()
}

/// 朋友/资料记录的变更时间由 `FriendRecord::updated_at` 与整域版本号
/// （sync 模块）承载：各变更方法以 `now_ms` 入参刷新，供自设备
/// contact-sync 的 LWW 裁决。

/// 空间对应的标签数组键。
fn tags_key(space: &str) -> Result<String> {
    Ok(match parse_space(space)? {
        Space::Personal => TAGS_KEY.to_string(),
        Space::Org(org_id) => org_tags_key(org_id),
    })
}

fn apply_patch_to_friend(friend: &mut FriendRecord, patch: &ProfilePatch) {
    if let Some(value) = &patch.remark {
        friend.remark = value.clone();
    }
    if let Some(value) = &patch.phones {
        friend.phones = value.clone();
    }
    if let Some(value) = &patch.tag_ids {
        friend.tag_ids = value.clone();
    }
    if let Some(value) = &patch.group_id {
        friend.group_id = value.clone();
    }
    if let Some(value) = &patch.memo {
        friend.memo = value.clone();
    }
    if let Some(value) = &patch.photos {
        friend.photos = value.clone();
    }
    if let Some(value) = &patch.permission {
        friend.permission = value.clone();
    }
}

fn apply_patch_to_profile(profile: &mut ContactProfileRecord, patch: &ProfilePatch) {
    if let Some(value) = &patch.remark {
        profile.remark = value.clone();
    }
    if let Some(value) = &patch.phones {
        profile.phones = value.clone();
    }
    if let Some(value) = &patch.tag_ids {
        profile.tag_ids = value.clone();
    }
    if let Some(value) = &patch.group_id {
        profile.group_id = value.clone();
    }
    if let Some(value) = &patch.memo {
        profile.memo = value.clone();
    }
    if let Some(value) = &patch.photos {
        profile.photos = value.clone();
    }
    if let Some(value) = &patch.permission {
        profile.permission = value.clone();
    }
}
