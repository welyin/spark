//! 通讯录命令。
//!
//! 视图/资料薄封装直通内核 `contact_*` 门面；标签/分组/组织树的 id 一律由
//! 前端生成透传（client id 幂等键）。好友申请投递为尽力而为——寻址失败才报错，
//! 命令落库 pending 即返回，投递终态（failed/回填昵称）经 FriendRequestSent
//! 事件回传（内核语义，见 core contact_ops.rs）。

use spark_core::contact::{
    ContactGroup, ContactTag, FriendRequestRecord, OrgGroupNode, ProfilePatch, SpaceContactsView,
};
use spark_core::kernel::{Kernel, SendFriendRequestInput};

use super::dto::SuccessResult;
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn overview_inner(
    kernel: &mut Kernel,
    space: &str,
) -> Result<SpaceContactsView, String> {
    kernel.contact_overview(space).map_err(err)
}

pub(crate) fn update_profile_inner(
    kernel: &mut Kernel,
    space: &str,
    root_id: &str,
    patch: ProfilePatch,
) -> Result<SuccessResult, String> {
    kernel
        .contact_update_profile(space, root_id, patch)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn set_blocked_inner(
    kernel: &mut Kernel,
    space: &str,
    root_id: &str,
    blocked: bool,
) -> Result<SuccessResult, String> {
    kernel
        .contact_set_blocked(space, root_id, blocked)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn remove_friend_inner(
    kernel: &mut Kernel,
    root_id: &str,
    block: bool,
) -> Result<SuccessResult, String> {
    kernel.contact_remove_friend(root_id, block).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn send_request_inner(
    kernel: &mut Kernel,
    input: SendFriendRequestInput,
) -> Result<FriendRequestRecord, String> {
    kernel.contact_send_request(input).map_err(err)
}

pub(crate) fn resolve_request_inner(
    kernel: &mut Kernel,
    request_id: &str,
    accept: bool,
    permission: Option<&str>,
) -> Result<SuccessResult, String> {
    kernel
        .contact_resolve_request(request_id, accept, permission)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn reply_request_inner(
    kernel: &mut Kernel,
    request_id: &str,
    text: &str,
) -> Result<FriendRequestRecord, String> {
    kernel.contact_reply_request(request_id, text).map_err(err)
}

pub(crate) fn tag_create_inner(
    kernel: &mut Kernel,
    space: &str,
    id: &str,
    name: &str,
) -> Result<ContactTag, String> {
    kernel.contact_tag_create(space, id, name).map_err(err)
}

pub(crate) fn tag_rename_inner(
    kernel: &mut Kernel,
    space: &str,
    tag_id: &str,
    name: &str,
) -> Result<SuccessResult, String> {
    kernel.contact_tag_rename(space, tag_id, name).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn tag_delete_inner(
    kernel: &mut Kernel,
    space: &str,
    tag_id: &str,
) -> Result<SuccessResult, String> {
    kernel.contact_tag_delete(space, tag_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

/// `space_key` 仅为对齐前端参数表（个人空间扁平分组，内核不落空间维度）。
pub(crate) fn group_create_inner(
    kernel: &mut Kernel,
    id: &str,
    name: &str,
) -> Result<ContactGroup, String> {
    kernel.contact_group_create(id, name).map_err(err)
}

pub(crate) fn group_rename_inner(
    kernel: &mut Kernel,
    group_id: &str,
    name: &str,
) -> Result<SuccessResult, String> {
    kernel.contact_group_rename(group_id, name).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn group_delete_inner(
    kernel: &mut Kernel,
    group_id: &str,
) -> Result<SuccessResult, String> {
    kernel.contact_group_delete(group_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn group_move_inner(
    kernel: &mut Kernel,
    group_id: &str,
    to_index: usize,
) -> Result<SuccessResult, String> {
    kernel.contact_group_move(group_id, to_index).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn set_group_inner(
    kernel: &mut Kernel,
    space: &str,
    root_id: &str,
    group_id: &str,
) -> Result<SuccessResult, String> {
    kernel
        .contact_set_group(space, root_id, group_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn org_group_create_inner(
    kernel: &mut Kernel,
    space: &str,
    parent_id: &str,
    id: &str,
    name: &str,
) -> Result<Option<OrgGroupNode>, String> {
    kernel
        .contact_org_group_create(space, parent_id, id, name)
        .map_err(err)
}

pub(crate) fn org_group_rename_inner(
    kernel: &mut Kernel,
    space: &str,
    id: &str,
    name: &str,
) -> Result<SuccessResult, String> {
    kernel
        .contact_org_group_rename(space, id, name)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn org_group_delete_inner(
    kernel: &mut Kernel,
    space: &str,
    id: &str,
) -> Result<SuccessResult, String> {
    kernel.contact_org_group_delete(space, id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn org_group_move_inner(
    kernel: &mut Kernel,
    space: &str,
    id: &str,
    to_index: usize,
    new_parent_id: Option<&str>,
) -> Result<SuccessResult, String> {
    kernel
        .contact_org_group_move(space, id, to_index, new_parent_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

/// 空间通讯录总览（个人空间：朋友/申请/标签/扁平分组，friends 恒含自己；
/// 组织空间：附加资料/标签/分组树）。
#[tauri::command]
pub fn contact_overview(
    state: tauri::State<'_, KernelState>,
    space_key: String,
) -> Result<SpaceContactsView, String> {
    overview_inner(&mut *lock_kernel(&state)?, &space_key)
}

/// 更新联系人本地资料（`patch` 中缺省字段保持不变）。
#[tauri::command]
pub fn contact_update_profile(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    root_id: String,
    patch: ProfilePatch,
) -> Result<SuccessResult, String> {
    update_profile_inner(&mut *lock_kernel(&state)?, &space_key, &root_id, patch)
}

/// 设置/取消拉黑。
#[tauri::command]
pub fn contact_set_blocked(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    root_id: String,
    blocked: bool,
) -> Result<SuccessResult, String> {
    set_blocked_inner(&mut *lock_kernel(&state)?, &space_key, &root_id, blocked)
}

/// 删除朋友（个人空间；`block` 为 true 时同时拉黑，缺省 false）。
#[tauri::command]
pub fn contact_remove_friend(
    state: tauri::State<'_, KernelState>,
    root_id: String,
    block: Option<bool>,
) -> Result<SuccessResult, String> {
    remove_friend_inner(&mut *lock_kernel(&state)?, &root_id, block.unwrap_or(false))
}

/// 发出好友申请（寻址失败报错；投递终态经 FriendRequestSent 事件回传，
/// 前端可用同 id 重试失败申请）。
#[tauri::command]
pub fn contact_send_request(
    state: tauri::State<'_, KernelState>,
    input: SendFriendRequestInput,
) -> Result<FriendRequestRecord, String> {
    send_request_inner(&mut *lock_kernel(&state)?, input)
}

/// 处理收到的好友申请（accept=true 建朋友并尽力回发 friend-accept）。
#[tauri::command]
pub fn contact_resolve_request(
    state: tauri::State<'_, KernelState>,
    request_id: String,
    accept: bool,
    permission: Option<String>,
) -> Result<SuccessResult, String> {
    resolve_request_inner(
        &mut *lock_kernel(&state)?,
        &request_id,
        accept,
        permission.as_deref(),
    )
}

/// 回复对方对发出申请的询问（本地落 thread 回 pending 并尽力投递
/// friend-reply 信封；返回更新后的申请记录）。
#[tauri::command]
pub fn contact_reply_request(
    state: tauri::State<'_, KernelState>,
    request_id: String,
    text: String,
) -> Result<FriendRequestRecord, String> {
    reply_request_inner(&mut *lock_kernel(&state)?, &request_id, &text)
}

/// 新建标签（id 前端生成透传）。
#[tauri::command]
pub fn contact_tag_create(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    id: String,
    name: String,
) -> Result<ContactTag, String> {
    tag_create_inner(&mut *lock_kernel(&state)?, &space_key, &id, &name)
}

/// 重命名标签。
#[tauri::command]
pub fn contact_tag_rename(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    tag_id: String,
    name: String,
) -> Result<SuccessResult, String> {
    tag_rename_inner(&mut *lock_kernel(&state)?, &space_key, &tag_id, &name)
}

/// 删除标签（从所有资料中摘除）。
#[tauri::command]
pub fn contact_tag_delete(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    tag_id: String,
) -> Result<SuccessResult, String> {
    tag_delete_inner(&mut *lock_kernel(&state)?, &space_key, &tag_id)
}

/// 新建个人空间扁平分组（`space_key` 仅为对齐前端参数表）。
#[tauri::command]
pub fn contact_group_create(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    id: String,
    name: String,
) -> Result<ContactGroup, String> {
    let _ = &space_key;
    group_create_inner(&mut *lock_kernel(&state)?, &id, &name)
}

/// 重命名分组。
#[tauri::command]
pub fn contact_group_rename(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    group_id: String,
    name: String,
) -> Result<SuccessResult, String> {
    let _ = &space_key;
    group_rename_inner(&mut *lock_kernel(&state)?, &group_id, &name)
}

/// 删除分组（组内朋友复位为未分组）。
#[tauri::command]
pub fn contact_group_delete(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    group_id: String,
) -> Result<SuccessResult, String> {
    let _ = &space_key;
    group_delete_inner(&mut *lock_kernel(&state)?, &group_id)
}

/// 拖拽重排分组（越界夹紧）。
#[tauri::command]
pub fn contact_group_move(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    group_id: String,
    to_index: usize,
) -> Result<SuccessResult, String> {
    let _ = &space_key;
    group_move_inner(&mut *lock_kernel(&state)?, &group_id, to_index)
}

/// 设置联系人所属分组（`""` = 未分组）。
#[tauri::command]
pub fn contact_set_group(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    root_id: String,
    group_id: String,
) -> Result<SuccessResult, String> {
    set_group_inner(&mut *lock_kernel(&state)?, &space_key, &root_id, &group_id)
}

/// 新建组织分组（`parent_id` 为 `""` 挂根层；父不存在返回 null）。
#[tauri::command]
pub fn contact_org_group_create(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    parent_id: String,
    id: String,
    name: String,
) -> Result<Option<OrgGroupNode>, String> {
    org_group_create_inner(&mut *lock_kernel(&state)?, &space_key, &parent_id, &id, &name)
}

/// 重命名组织分组。
#[tauri::command]
pub fn contact_org_group_rename(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    id: String,
    name: String,
) -> Result<SuccessResult, String> {
    org_group_rename_inner(&mut *lock_kernel(&state)?, &space_key, &id, &name)
}

/// 删除组织分组（子节点提升一层）。
#[tauri::command]
pub fn contact_org_group_delete(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    id: String,
) -> Result<SuccessResult, String> {
    org_group_delete_inner(&mut *lock_kernel(&state)?, &space_key, &id)
}

/// 拖拽移动组织分组（`new_parent_id` 缺省 = 同级重排；`Some("")` = 移到根层）。
#[tauri::command]
pub fn contact_org_group_move(
    state: tauri::State<'_, KernelState>,
    space_key: String,
    id: String,
    to_index: usize,
    new_parent_id: Option<String>,
) -> Result<SuccessResult, String> {
    org_group_move_inner(
        &mut *lock_kernel(&state)?,
        &space_key,
        &id,
        to_index,
        new_parent_id.as_deref(),
    )
}

// ------------------------------------------------------------------
// 单元测试（tests.rs）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests;
