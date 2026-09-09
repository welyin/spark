//! 组织命令。
//!
//! `acceptInvite` 在 TS 侧 = 解码邀请 → P2P 连接邀请人拉取数据 → 落库确认，
//! 内核 `Kernel::accept_invite` 已编排全段（`org_accept_invite` 命令直通）。
//! `join_by_invite` / `check_join` 两个拆步命令保留（调试/分步场景可用）。

use spark_core::kernel::Kernel;
use spark_core::org::service::OrgIdentityPatch;
use spark_core::org::{OrgInvitePayload, OrgInviteRecord, OrganizationView};

use super::dto::{
    AddOrgMemberInputDto, CommunityAcceptDto, CommunityLeaveDto, CommunityMemberDto,
    CreateOrgInputDto, CreatedCommunityInviteDto, CreatedOrgInviteDto, InviteAcceptanceDto,
    OrgAddressRecordDto, OrgSyncOverviewDto, avatar_patch,
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

pub(crate) fn leave_inner(kernel: &mut Kernel, org_id: &str) -> Result<OrganizationView, String> {
    kernel.org_leave(org_id).map_err(err)
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

// `org_update_my_identity` 的 avatar 三态映射走 `dto::avatar_patch`（B1 口径注释
// 也在那里）；`org_update_info` 的 avatar 直传内核（空串 = 清除，内核 settings 口径）。

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

/// `OrgIdentityPatch` 的组装在命令壳完成（IPC 扁平参数 → 结构体），inner 直接
/// 收结构体（§2.4：参数超过 4 个用结构体传参）。
pub(crate) fn update_my_identity_inner(
    kernel: &mut Kernel,
    org_id: &str,
    patch: &OrgIdentityPatch,
) -> Result<OrganizationView, String> {
    kernel.org_update_my_identity(org_id, patch).map_err(err)
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

pub(crate) fn send_invite_inner(
    kernel: &mut Kernel,
    org_id: &str,
    target_root_id: &str,
    target_peer_id: Option<String>,
    target_addresses: Option<Vec<String>>,
    target_nickname: Option<String>,
) -> Result<OrgInviteRecord, String> {
    kernel
        .org_send_invite(
            org_id,
            target_root_id,
            target_peer_id.as_deref(),
            &target_addresses.unwrap_or_default(),
            target_nickname.as_deref(),
        )
        .map_err(err)
}

pub(crate) fn respond_invite_inner(
    kernel: &mut Kernel,
    invite_id: &str,
    accept: bool,
) -> Result<OrgInviteRecord, String> {
    kernel.org_respond_invite(invite_id, accept).map_err(err)
}

pub(crate) fn invite_records_inner(
    kernel: &Kernel,
    org_id: &str,
) -> Result<Vec<OrgInviteRecord>, String> {
    kernel.org_invite_records(org_id).map_err(err)
}

// ------------------------------------------------------------------
// 共同体域（组织加入共同体：org-mail 邀请/接受，org-genesis §3/§4）
// ------------------------------------------------------------------

pub(crate) fn community_create_invite_inner(
    kernel: &Kernel,
    community_org_id: &str,
) -> Result<CreatedCommunityInviteDto, String> {
    kernel
        .community_create_invite(community_org_id)
        .map(CreatedCommunityInviteDto::from)
        .map_err(err)
}

pub(crate) fn community_send_invite_inner(
    kernel: &mut Kernel,
    community_org_id: &str,
    code: &str,
    to_org_address: &str,
    recipient_domain_id: &str,
    gateway_peer_id: Option<String>,
    gateway_addresses: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    kernel
        .community_send_invite(
            community_org_id,
            code,
            to_org_address,
            recipient_domain_id,
            gateway_peer_id.as_deref(),
            &gateway_addresses.unwrap_or_default(),
        )
        .map_err(err)
}

pub(crate) fn community_accept_invite_inner(
    kernel: &mut Kernel,
    joiner_org_id: &str,
    code: &str,
    publish_binding: bool,
) -> Result<CommunityAcceptDto, String> {
    kernel
        .community_accept_invite(joiner_org_id, code, publish_binding)
        .map(CommunityAcceptDto::from)
        .map_err(err)
}

pub(crate) fn community_list_members_inner(
    kernel: &Kernel,
    community_org_id: &str,
) -> Result<Vec<CommunityMemberDto>, String> {
    kernel
        .community_list_members(community_org_id)
        .map(|members| members.into_iter().map(CommunityMemberDto::from).collect())
        .map_err(err)
}

pub(crate) fn community_leave_inner(
    kernel: &mut Kernel,
    community_org_id: &str,
    leaver_org_id: &str,
) -> Result<CommunityLeaveDto, String> {
    kernel
        .community_leave(community_org_id, leaver_org_id)
        .map(CommunityLeaveDto::from)
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

/// 退出组织（A13 / community-model §4.3：成员自退出；最后一名成员退出
/// 即成空域只读档案）。删除通路已整体移除，域只可退出不可解散。
#[tauri::command]
pub fn org_leave(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<OrganizationView, String> {
    leave_inner(&mut *lock_kernel(&state)?, &org_id)
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

/// 当前履职网关活跃集（只读；A47 / network §三 G6）：全员候选计分推导，
/// 返回履职成员 rootId 列表（≤3，全叶组织为空集）。
#[tauri::command]
pub fn org_gateway_active_set(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<Vec<String>, String> {
    lock_kernel(&state)?
        .org_gateway_active_set(&org_id)
        .map_err(err)
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

/// 更新自己的组织内身份（任何成员可改，仅改本人成员记录；字段语义同
/// root_update_profile：nickname 设置即校验；avatar 缺省不变 / `""` 清除 /
/// 非空设置；gender/region/signature 空串清除；usePersonalIdentity 缺省不变）。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn org_update_my_identity(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    nickname: Option<String>,
    avatar: Option<String>,
    gender: Option<String>,
    region: Option<String>,
    signature: Option<String>,
    use_personal_identity: Option<bool>,
) -> Result<OrganizationView, String> {
    let patch = OrgIdentityPatch {
        nickname,
        avatar: avatar_patch(avatar),
        gender,
        region,
        signature,
        use_personal_identity,
    };
    update_my_identity_inner(&mut *lock_kernel(&state)?, &org_id, &patch)
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

/// 接受邀请码（内核编排：解码 → 连接邀请人 → stub 自举 + orgsync 收敛 → 成员确认）。
#[tauri::command]
pub fn org_accept_invite(
    state: tauri::State<'_, KernelState>,
    code: String,
) -> Result<InviteAcceptanceDto, String> {
    accept_invite_inner(&mut *lock_kernel(&state)?, &code)
}

/// 经 DM 发出组织邀请（仅 admin；落出站记录 + org-invite 信封尽力投递）。
#[tauri::command]
pub fn org_send_invite(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    target_root_id: String,
    target_peer_id: Option<String>,
    target_addresses: Option<Vec<String>>,
    target_nickname: Option<String>,
) -> Result<OrgInviteRecord, String> {
    send_invite_inner(
        &mut *lock_kernel(&state)?,
        &org_id,
        &target_root_id,
        target_peer_id,
        target_addresses,
        target_nickname,
    )
}

/// 回应收到的组织邀请（accept=true 走加入编排，false 仅拒绝；幂等）。
#[tauri::command]
pub fn org_respond_invite(
    state: tauri::State<'_, KernelState>,
    invite_id: String,
    accept: bool,
) -> Result<OrgInviteRecord, String> {
    respond_invite_inner(&mut *lock_kernel(&state)?, &invite_id, accept)
}

/// 指定组织的全部邀请记录（出/入站合并）。
#[tauri::command]
pub fn org_invite_records(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<Vec<OrgInviteRecord>, String> {
    invite_records_inner(&*lock_kernel(&state)?, &org_id)
}

/// 创建共同体邀请码（仅共同体域 admin；org-mail `community-org-invite` 载荷
/// 内嵌的 code，投递走 `community_send_invite` 或带外渠道）。
#[tauri::command]
pub fn community_create_invite(
    state: tauri::State<'_, KernelState>,
    community_org_id: String,
) -> Result<CreatedCommunityInviteDto, String> {
    community_create_invite_inner(&*lock_kernel(&state)?, &community_org_id)
}

/// 邀请码经 org-mail 投递到目标组织信箱（目标组织地址记录完整线形 + 收件人
/// 域身份为带外通道输入；网关寻址线索可省，内核按 显式 hint → 地址记录
/// gateways 联系人 解析）。
#[tauri::command]
pub fn community_send_invite(
    state: tauri::State<'_, KernelState>,
    community_org_id: String,
    code: String,
    to_org_address: String,
    recipient_domain_id: String,
    gateway_peer_id: Option<String>,
    gateway_addresses: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    community_send_invite_inner(
        &mut *lock_kernel(&state)?,
        &community_org_id,
        &code,
        &to_org_address,
        &recipient_domain_id,
        gateway_peer_id,
        gateway_addresses,
    )
}

/// 接受共同体邀请（本机为待加入组织的管理员）：组织根私钥派生共同体域身份
/// → 落库前 validate_org_join 硬规则 → 名册写 kind=org 成员条目；
/// `publish_binding` = 是否公开 orgId/地址绑定（opt-in，org-genesis §3.2）。
#[tauri::command]
pub fn community_accept_invite(
    state: tauri::State<'_, KernelState>,
    joiner_org_id: String,
    code: String,
    publish_binding: bool,
) -> Result<CommunityAcceptDto, String> {
    community_accept_invite_inner(
        &mut *lock_kernel(&state)?,
        &joiner_org_id,
        &code,
        publish_binding,
    )
}

/// 列共同体成员（本地名册中 kind=org 的条目）。
#[tauri::command]
pub fn community_list_members(
    state: tauri::State<'_, KernelState>,
    community_org_id: String,
) -> Result<Vec<CommunityMemberDto>, String> {
    community_list_members_inner(&*lock_kernel(&state)?, &community_org_id)
}

/// 组织退出共同体（本机为退出组织的管理员）：组织根私钥派生域身份核账 →
/// 留史记录（append-only）+ 名册移除；最后一个成员组织退出后域进入空域
/// 只读档案（`domainArchived=true`，此后该域写入一律拒绝）。
#[tauri::command]
pub fn community_leave(
    state: tauri::State<'_, KernelState>,
    community_org_id: String,
    leaver_org_id: String,
) -> Result<CommunityLeaveDto, String> {
    community_leave_inner(&mut *lock_kernel(&state)?, &community_org_id, &leaver_org_id)
}

// ------------------------------------------------------------------
// 单元测试（tests.rs）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests;
