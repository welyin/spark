//! 组织命令。
//!
//! `acceptInvite` 在 TS 侧 = 解码邀请 → P2P 连接邀请人拉取数据 → 落库确认，
//! 内核 `Kernel::accept_invite` 已编排全段（`org_accept_invite` 命令直通）。
//! `join_by_invite` / `check_join` 两个拆步命令保留（调试/分步场景可用）。

use spark_core::kernel::Kernel;
use spark_core::org::{OrgInvitePayload, OrganizationView};

use super::dto::{
    AddOrgMemberInputDto, CreateOrgInputDto, CreatedOrgInviteDto, InviteAcceptanceDto,
    OrgAddressRecordDto, OrgSyncOverviewDto, SuccessResult,
};
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn list_mine_inner(kernel: &Kernel) -> Result<Vec<OrganizationView>, String> {
    kernel.list_orgs().map_err(err)
}

pub(crate) fn create_inner(
    kernel: &mut Kernel,
    input: CreateOrgInputDto,
) -> Result<OrganizationView, String> {
    kernel.create_org(input.into()).map_err(err)
}

pub(crate) fn create_invite_inner(
    kernel: &Kernel,
    org_id: &str,
) -> Result<CreatedOrgInviteDto, String> {
    kernel
        .create_org_invite(org_id)
        .map(CreatedOrgInviteDto::from)
        .map_err(err)
}

pub(crate) fn join_by_invite_inner(
    kernel: &Kernel,
    code: &str,
) -> Result<OrgInvitePayload, String> {
    kernel.join_by_invite(code).map_err(err)
}

pub(crate) fn check_join_inner(
    kernel: &Kernel,
    org_id: &str,
) -> Result<InviteAcceptanceDto, String> {
    kernel
        .check_join(org_id)
        .map(InviteAcceptanceDto::from)
        .map_err(err)
}

pub(crate) fn sync_overview_inner(
    kernel: &Kernel,
    org_id: &str,
) -> Result<OrgSyncOverviewDto, String> {
    kernel
        .org_overview(org_id)
        .map(OrgSyncOverviewDto::from)
        .map_err(err)
}

pub(crate) fn delete_inner(kernel: &mut Kernel, org_id: &str) -> Result<SuccessResult, String> {
    kernel.org_delete(org_id).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn add_member_inner(
    kernel: &mut Kernel,
    org_id: &str,
    input: AddOrgMemberInputDto,
) -> Result<OrganizationView, String> {
    let node_info = input.node_info.map(spark_core::org::OrganizationNodeInfo::from);
    kernel
        .org_add_member(org_id, &input.root_id, node_info.as_ref())
        .map_err(err)
}

pub(crate) fn remove_member_inner(
    kernel: &mut Kernel,
    org_id: &str,
    member_root_id: &str,
) -> Result<OrganizationView, String> {
    kernel.org_remove_member(org_id, member_root_id).map_err(err)
}

pub(crate) fn set_gateways_inner(
    kernel: &mut Kernel,
    org_id: &str,
    gateways: Vec<String>,
) -> Result<OrganizationView, String> {
    kernel.org_set_gateways(org_id, &gateways).map_err(err)
}

pub(crate) fn set_public_inner(
    kernel: &mut Kernel,
    org_id: &str,
    public: bool,
    display_name: Option<String>,
) -> Result<OrganizationView, String> {
    kernel
        .org_set_public(org_id, public, display_name.as_deref())
        .map_err(err)
}

pub(crate) fn update_info_inner(
    kernel: &mut Kernel,
    org_id: &str,
    name: Option<String>,
    description: Option<String>,
    avatar: Option<String>,
) -> Result<OrganizationView, String> {
    kernel
        .org_update_info(
            org_id,
            name.as_deref(),
            description.as_deref(),
            avatar.as_deref(),
        )
        .map_err(err)
}

pub(crate) fn resolve_address_inner(
    kernel: &Kernel,
    org_address: &str,
) -> Result<Option<OrgAddressRecordDto>, String> {
    kernel
        .resolve_org_address(org_address)
        .map(|record| record.map(OrgAddressRecordDto::from))
        .map_err(err)
}

pub(crate) fn search_known_inner(
    kernel: &Kernel,
    keyword: &str,
) -> Result<Vec<OrgAddressRecordDto>, String> {
    kernel
        .search_known_orgs(keyword)
        .map(|records| records.into_iter().map(OrgAddressRecordDto::from).collect())
        .map_err(err)
}

pub(crate) fn accept_invite_inner(
    kernel: &mut Kernel,
    code: &str,
) -> Result<InviteAcceptanceDto, String> {
    kernel
        .accept_invite(code)
        .map(InviteAcceptanceDto::from)
        .map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn org_list_mine(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<OrganizationView>, String> {
    list_mine_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn org_create(
    state: tauri::State<'_, KernelState>,
    input: CreateOrgInputDto,
) -> Result<OrganizationView, String> {
    create_inner(&mut *lock_kernel(&state)?, input)
}

#[tauri::command]
pub fn org_create_invite(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<CreatedOrgInviteDto, String> {
    create_invite_inner(&*lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn org_join_by_invite(
    state: tauri::State<'_, KernelState>,
    code: String,
) -> Result<OrgInvitePayload, String> {
    join_by_invite_inner(&*lock_kernel(&state)?, &code)
}

#[tauri::command]
pub fn org_check_join(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<InviteAcceptanceDto, String> {
    check_join_inner(&*lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn org_sync_overview(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<OrgSyncOverviewDto, String> {
    sync_overview_inner(&*lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn org_delete(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<SuccessResult, String> {
    delete_inner(&mut *lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn org_add_member(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    input: AddOrgMemberInputDto,
) -> Result<OrganizationView, String> {
    add_member_inner(&mut *lock_kernel(&state)?, &org_id, input)
}

#[tauri::command]
pub fn org_remove_member(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    member_root_id: String,
) -> Result<OrganizationView, String> {
    remove_member_inner(&mut *lock_kernel(&state)?, &org_id, &member_root_id)
}

/// 指定组织网关（仅 admin；2–3 名本组织成员的 rootId，org.md §14）。
#[tauri::command]
pub fn org_set_gateways(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    gateways: Vec<String>,
) -> Result<OrganizationView, String> {
    set_gateways_inner(&mut *lock_kernel(&state)?, &org_id, gateways)
}

/// 开关组织公开标志（仅 admin；org.md §16），可选更新地址记录展示名。
#[tauri::command]
pub fn org_set_public(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    public: bool,
    display_name: Option<String>,
) -> Result<OrganizationView, String> {
    set_public_inner(&mut *lock_kernel(&state)?, &org_id, public, display_name)
}

/// 更新组织名称/描述/logo（仅 admin；未提供的字段不变，avatar 空串 = 清除 logo）。
#[tauri::command]
pub fn org_update_info(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    name: Option<String>,
    description: Option<String>,
    avatar: Option<String>,
) -> Result<OrganizationView, String> {
    update_info_inner(&mut *lock_kernel(&state)?, &org_id, name, description, avatar)
}

/// 解析组织地址（缓存 → DHT，org.md §16.4）；未命中返回 null。
#[tauri::command]
pub fn org_resolve_address(
    state: tauri::State<'_, KernelState>,
    org_address: String,
) -> Result<Option<OrgAddressRecordDto>, String> {
    resolve_address_inner(&*lock_kernel(&state)?, &org_address)
}

/// 本地搜索已知组织（缓存按 displayName/orgAddress 子串匹配，纯本地）。
#[tauri::command]
pub fn org_search_known(
    state: tauri::State<'_, KernelState>,
    keyword: String,
) -> Result<Vec<OrgAddressRecordDto>, String> {
    search_known_inner(&*lock_kernel(&state)?, &keyword)
}

/// 接受邀请码（内核编排：解码 → 连接邀请人 → claim 捎带 → 拉取 → 成员确认）。
#[tauri::command]
pub fn org_accept_invite(
    state: tauri::State<'_, KernelState>,
    code: String,
) -> Result<InviteAcceptanceDto, String> {
    accept_invite_inner(&mut *lock_kernel(&state)?, &code)
}

// ------------------------------------------------------------------
// 单元测试（tests.rs）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests;
