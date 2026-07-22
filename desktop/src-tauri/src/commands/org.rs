//! 组织命令。
//!
//! `acceptInvite` 在 TS 侧 = 解码邀请 → P2P 连接邀请人拉取数据 → 落库确认，
//! 内核 `Kernel::accept_invite` 已编排全段（`org_accept_invite` 命令直通）。
//! `join_by_invite` / `check_join` 两个拆步命令保留（调试/分步场景可用）。

use spark_core::kernel::Kernel;
use spark_core::org::{OrgInvitePayload, OrganizationView};

use super::dto::{
    AddOrgMemberInputDto, CreateOrgInputDto, CreatedOrgInviteDto, InviteAcceptanceDto,
    OrgSyncOverviewDto, SuccessResult,
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

/// 接受邀请码（内核编排：解码 → 连接邀请人 → claim 捎带 → 拉取 → 成员确认）。
#[tauri::command]
pub fn org_accept_invite(
    state: tauri::State<'_, KernelState>,
    code: String,
) -> Result<InviteAcceptanceDto, String> {
    accept_invite_inner(&mut *lock_kernel(&state)?, &code)
}

// ------------------------------------------------------------------
// 单元测试
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    const PASSWORD: &str = "correct-horse-battery";

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity(PASSWORD, "alice", None).unwrap();
        (dir, kernel)
    }

    fn input() -> CreateOrgInputDto {
        serde_json::from_value(serde_json::json!({
            "name": "测试组织",
            "description": "demo",
            "basePluginDomain": "plugin:base"
        }))
        .unwrap()
    }

    #[test]
    fn create_and_list_roundtrip() {
        let (_dir, mut kernel) = unlocked_kernel();
        assert!(list_mine_inner(&kernel).unwrap().is_empty());

        let view = create_inner(&mut kernel, input()).unwrap();
        assert_eq!(view.record.name, "测试组织");
        assert!(view.is_current_user_admin);
        assert_eq!(view.member_count, 1);

        let mine = list_mine_inner(&kernel).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].record.org_id, view.record.org_id);

        // 副本概览：本机恒已同步
        let overview = sync_overview_inner(&kernel, &view.record.org_id).unwrap();
        assert_eq!(overview.total_members, 1);
        assert!(overview.synced_peers >= 1);
        assert!(overview.members[0].is_self);
    }

    #[test]
    fn invite_requires_p2p_and_bad_code_errors() {
        let (_dir, mut kernel) = unlocked_kernel();
        let view = create_inner(&mut kernel, input()).unwrap();

        // P2P 未启动 → 生成邀请码报专用文案（内核语义：邀请码须携带本机节点信息）
        assert_eq!(
            create_invite_inner(&kernel, &view.record.org_id).unwrap_err(),
            "本机 P2P 节点尚未启动，请先启动网络后再生成邀请码"
        );

        // 坏邀请码 → 解析失败
        assert!(join_by_invite_inner(&kernel, "not-a-code").is_err());

        // 未知组织 → check_join 失败（本地无成员记录）
        assert!(check_join_inner(&kernel, "org_0000000000000000").is_err());
    }

    #[test]
    fn overview_unknown_org_errors() {
        let (_dir, kernel) = unlocked_kernel();
        assert!(sync_overview_inner(&kernel, "org_0000000000000000").is_err());
    }

    #[test]
    fn member_management_and_delete() {
        let (_dir, mut kernel) = unlocked_kernel();
        let view = create_inner(&mut kernel, input()).unwrap();
        let org_id = view.record.org_id.clone();
        let member_root = "ab".repeat(32);

        // 添加成员（无 nodeInfo）
        let input: AddOrgMemberInputDto =
            serde_json::from_value(serde_json::json!({ "rootId": member_root })).unwrap();
        let view = add_member_inner(&mut kernel, &org_id, input).unwrap();
        assert_eq!(view.member_count, 2);
        assert!(!view.members.iter().all(|m| m.root_id != member_root));

        // 非法 rootId / 未知组织
        let bad: AddOrgMemberInputDto =
            serde_json::from_value(serde_json::json!({ "rootId": "zz" })).unwrap();
        assert_eq!(
            add_member_inner(&mut kernel, &org_id, bad).unwrap_err(),
            "Invalid member rootId"
        );
        let input: AddOrgMemberInputDto =
            serde_json::from_value(serde_json::json!({ "rootId": member_root })).unwrap();
        assert_eq!(
            add_member_inner(&mut kernel, "org_nope", input).unwrap_err(),
            "Organization not found"
        );

        // 移除成员；移除唯一 admin 被拒
        let view = remove_member_inner(&mut kernel, &org_id, &member_root).unwrap();
        assert_eq!(view.member_count, 1);
        let self_root = kernel.current_root_id().unwrap().unwrap();
        assert_eq!(
            remove_member_inner(&mut kernel, &org_id, &self_root).unwrap_err(),
            "Organization must keep at least one admin"
        );

        // 删除组织
        delete_inner(&mut kernel, &org_id).unwrap();
        assert!(list_mine_inner(&kernel).unwrap().is_empty());
        assert_eq!(
            delete_inner(&mut kernel, &org_id).unwrap_err(),
            "Organization not found"
        );
    }

    #[test]
    fn accept_invite_error_paths() {
        let (_dir, mut kernel) = unlocked_kernel();
        // 坏邀请码
        assert!(accept_invite_inner(&mut kernel, "not-a-code").is_err());
        // 合法邀请码但 p2p 未启动
        let code = spark_core::org::encode_org_invite(&spark_core::org::OrgInvitePayload::new(
            "org_abc".to_string(),
            "组织".to_string(),
            spark_core::org::OrgInviteInviter {
                root_id: "cd".repeat(32),
                peer_id: Some("peer-1234567890".to_string()),
                addresses: vec![],
            },
            spark_core::p2p::node::system_now_ms(),
        ));
        assert_eq!(
            accept_invite_inner(&mut kernel, &code).unwrap_err(),
            "P2P 网络未启动，无法通过邀请码加入"
        );
    }
}
