//! 插件运行时命令：`plugin-identity-sign` / `plugin-identity-verify` /
//! `plugin-org-sync-now`（语义对齐 TS desktop/src/main/ipc/plugin.ts:47-136）。
//!
//! 本期沿用旧 tab 模式语义：插件视图以 iframe tab 跑在 system 域窗口内，
//! 高级权限（TS `requirePluginPermission` 的声明-校验）不做强制校验，
//! 插件域一律由前端适配层显式传入（tab 场景取自 URL query `pluginDomain`）。
//! 独立插件窗口绑定域 + 强制权限校验待插件运行时排期。

use serde::Serialize;
use spark_core::kernel::{DomainSignatureInfo, Kernel};
use spark_core::org::OrganizationRole;

use super::{err, lock_kernel};
use crate::KernelState;

/// `plugin-identity-verify` 返回（TS `{ valid: boolean }`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VerifyResultDto {
    pub valid: bool,
}

/// `plugin-org-sync-now` 返回（TS `{ orgId, attempted, pulled }`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrgSyncNowResultDto {
    pub org_id: String,
    pub attempted: u32,
    pub pulled: u32,
}

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

/// `plugin-identity-sign`（ipc/plugin.ts:47-53）：以调用方插件域身份签名，
/// 根身份与域私钥均不离开内核。校验顺序对齐 TS：载荷非空先于签名。
pub(crate) fn identity_sign_inner(
    kernel: &Kernel,
    payload: &str,
    plugin_domain: &str,
) -> Result<DomainSignatureInfo, String> {
    if payload.is_empty() {
        return Err("Payload is required".to_string());
    }
    kernel
        .sign_with_domain_identity(plugin_domain, payload)
        .map_err(err)
}

/// `plugin-identity-verify`（ipc/plugin.ts:56-64）：纯验签（Ed25519），
/// 不含任何敏感数据。TS 的 typeof 三参校验由 serde 参数类型吸收。
pub(crate) fn identity_verify_inner(
    payload: &str,
    signature: &str,
    public_key: &str,
) -> VerifyResultDto {
    VerifyResultDto {
        valid: spark_core::identity::verify_ed25519_signature(payload, signature, public_key),
    }
}

/// `plugin-org-sync-now`（ipc/plugin.ts:71-136）：校验组织归属当前插件域后
/// 逐成员（admin 优先、跳过自己）定向拉取，返回尝试/成功计数。
///
/// 与 TS 的两处实现差异（语义等价）：
/// - TS `ensureCoreServicesStarted` → 内核幂等 `start_p2p`；
/// - TS 逐成员 `pullOrganizationsFromPeer` → 内核 `sync_peer_organizations`
///   （同一对账编排：双向 stale 推送 + org-pull + removed 清理）。TS 的成功
///   判定 `pulled > 0 || synced > 0` 中 `synced` 恒等于 `pulled`，故对齐到
///   内核只看 `pull_synced`（内核 `synced` 是反推成功数，对应 TS `pushed`，
///   TS 未计入）。成员仅报 peerId 不带地址时内核对账报"地址缺失"，与 TS
///   拨号失败一样计入 attempted 后跳过。
pub(crate) fn org_sync_now_inner(
    kernel: &mut Kernel,
    org_id: &str,
    plugin_domain: &str,
) -> Result<OrgSyncNowResultDto, String> {
    if plugin_domain.trim().is_empty() {
        return Err("Domain is required".to_string());
    }
    if org_id.is_empty() {
        return Err("Organization id is required".to_string());
    }

    // TS：P2P 未初始化/未启动时先确保核心服务已启动（内核 start_p2p 幂等）。
    if !kernel.p2p_running() {
        kernel.start_p2p().map_err(err)?;
    }

    let organizations = kernel.list_orgs().map_err(err)?;
    let target = organizations
        .iter()
        .find(|item| item.record.org_id == org_id)
        .ok_or_else(|| "Organization not found or not joined".to_string())?;

    if target.record.base_plugin_domain != plugin_domain {
        return Err("Organization does not belong to current plugin domain".to_string());
    }

    let current_root_id = kernel
        .current_root_id()
        .map_err(err)?
        .ok_or_else(|| "Root identity is unavailable".to_string())?;

    let mut candidates: Vec<_> = target
        .members
        .iter()
        .filter(|member| member.root_id != current_root_id && member.node_info.is_some())
        .collect();
    // admin 优先（稳定排序；view 成员列表本身已 admin 在前，此处对齐 TS 显式排序）。
    candidates.sort_by_key(|member| {
        if member.role == OrganizationRole::Admin {
            0
        } else {
            1
        }
    });

    let mut attempted = 0u32;
    let mut pulled = 0u32;

    for member in candidates {
        let Some(node_info) = member.node_info.clone() else {
            continue;
        };
        let has_peer = node_info
            .peer_id
            .as_deref()
            .is_some_and(|peer_id| !peer_id.trim().is_empty());
        let has_address = !node_info.addresses.is_empty();
        if !has_peer && !has_address {
            continue;
        }

        attempted += 1;
        match kernel.sync_peer_organizations(&node_info) {
            Ok(result) => {
                if result.pull_synced > 0 {
                    pulled += 1;
                }
            }
            Err(error) => {
                eprintln!(
                    "[plugin-org-sync-now] pull failed orgId={} memberRootId={} error={}",
                    org_id, member.root_id, error
                );
            }
        }
    }

    Ok(OrgSyncNowResultDto {
        org_id: org_id.to_string(),
        attempted,
        pulled,
    })
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn plugin_identity_sign(
    state: tauri::State<'_, KernelState>,
    payload: String,
    plugin_domain: String,
) -> Result<DomainSignatureInfo, String> {
    identity_sign_inner(&*lock_kernel(&state)?, &payload, &plugin_domain)
}

#[tauri::command]
pub fn plugin_identity_verify(
    payload: String,
    signature: String,
    public_key: String,
) -> VerifyResultDto {
    // 纯函数验签，无需内核状态（TS 同样不依赖身份态）。
    identity_verify_inner(&payload, &signature, &public_key)
}

#[tauri::command]
pub fn plugin_org_sync_now(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    plugin_domain: String,
) -> Result<OrgSyncNowResultDto, String> {
    org_sync_now_inner(&mut *lock_kernel(&state)?, &org_id, &plugin_domain)
}

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    const PASSWORD: &str = "correct-horse-battery";
    const DOMAIN: &str = "plugin:weibo-core";

    fn temp_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        (dir, kernel)
    }

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let (dir, mut kernel) = temp_kernel();
        kernel.init_identity(PASSWORD, "alice", None).unwrap();
        (dir, kernel)
    }

    #[test]
    fn identity_sign_requires_unlocked_and_payload() {
        let (_dir, mut kernel) = temp_kernel();

        // 锁定 → TS `Root identity is locked`（域身份签名需解锁派生）
        assert_eq!(
            identity_sign_inner(&kernel, "payload", DOMAIN).unwrap_err(),
            "Root identity is locked"
        );

        kernel.init_identity(PASSWORD, "alice", None).unwrap();

        // 空载荷 → TS `Payload is required`（先于签名）
        assert_eq!(
            identity_sign_inner(&kernel, "", DOMAIN).unwrap_err(),
            "Payload is required"
        );
        // 空域 → 内核 `Domain is required`
        assert_eq!(
            identity_sign_inner(&kernel, "payload", " ").unwrap_err(),
            "Domain is required"
        );

        // 固定载荷签名：形状齐全且可验
        let sig = identity_sign_inner(&kernel, "payload-1", DOMAIN).unwrap();
        assert_eq!(sig.domain, DOMAIN);
        assert_eq!(sig.payload_hash.len(), 64);
        assert!(spark_core::identity::verify_ed25519_signature(
            "payload-1",
            &sig.signature,
            &sig.public_key
        ));
    }

    #[test]
    fn identity_verify_roundtrip_and_garbage() {
        let (_dir, kernel) = unlocked_kernel();
        let sig = identity_sign_inner(&kernel, "payload-2", DOMAIN).unwrap();

        // 签名回路 → valid
        assert_eq!(
            identity_verify_inner("payload-2", &sig.signature, &sig.public_key),
            VerifyResultDto { valid: true }
        );
        // 篡改载荷 / 坏 base64 → invalid（不报错，对齐 TS try/catch false）
        assert_eq!(
            identity_verify_inner("tampered", &sig.signature, &sig.public_key),
            VerifyResultDto { valid: false }
        );
        assert_eq!(
            identity_verify_inner("payload-2", "not-base64!!!", &sig.public_key),
            VerifyResultDto { valid: false }
        );
    }

    #[test]
    fn org_sync_now_validation_order() {
        let (_dir, mut kernel) = unlocked_kernel();

        // 域为空 → 先于 orgId 校验（对齐 TS requirePluginPermission 在前）
        assert_eq!(
            org_sync_now_inner(&mut kernel, "org_1", "").unwrap_err(),
            "Domain is required"
        );
        // orgId 为空 → TS `Organization id is required`
        assert_eq!(
            org_sync_now_inner(&mut kernel, "", DOMAIN).unwrap_err(),
            "Organization id is required"
        );
        // 未加入的组织 → TS `Organization not found or not joined`
        // （会顺带幂等启动 P2P，对齐 TS ensureCoreServicesStarted）
        assert_eq!(
            org_sync_now_inner(&mut kernel, "org_nope", DOMAIN).unwrap_err(),
            "Organization not found or not joined"
        );
    }

    #[test]
    fn org_sync_now_domain_mismatch_and_self_only_org() {
        let (_dir, mut kernel) = unlocked_kernel();

        // 建一个绑定本插件域的组织（仅自己一个成员）
        let input: super::super::dto::CreateOrgInputDto = serde_json::from_value(serde_json::json!({
            "name": "微博组织",
            "basePluginDomain": DOMAIN
        }))
        .unwrap();
        let view = kernel.create_org(input.into()).unwrap();
        let org_id = view.record.org_id.clone();

        // 域不匹配 → TS `Organization does not belong to current plugin domain`
        assert_eq!(
            org_sync_now_inner(&mut kernel, &org_id, "plugin:chat").unwrap_err(),
            "Organization does not belong to current plugin domain"
        );

        // 无其他成员 → 无候选，attempted/pulled 均 0（P2P 已幂等启动）
        let result = org_sync_now_inner(&mut kernel, &org_id, DOMAIN).unwrap();
        assert_eq!(
            result,
            OrgSyncNowResultDto {
                org_id: org_id.clone(),
                attempted: 0,
                pulled: 0
            }
        );

        // 加一个无 nodeInfo 的成员 → 仍无候选（filter 掉）
        let member_root = "ab".repeat(32);
        kernel
            .org_add_member(&org_id, &member_root, None)
            .unwrap();
        let result = org_sync_now_inner(&mut kernel, &org_id, DOMAIN).unwrap();
        assert_eq!(result.attempted, 0);
        assert_eq!(result.pulled, 0);
    }
}
