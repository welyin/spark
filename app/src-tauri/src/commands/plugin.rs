//! 插件运行时命令：`plugin-identity-sign` / `plugin-identity-verify` /
//! `plugin-org-sync-now`（语义对齐 TS desktop/src/main/ipc/plugin.ts:47-136）。
//!
//! 本期沿用旧 tab 模式语义：插件视图以 iframe tab 跑在 system 域窗口内，
//! 高级权限（TS `requirePluginPermission` 的声明-校验）不做强制校验，
//! 插件域一律由前端适配层显式传入（tab 场景取自 URL query `pluginDomain`）。
//! 独立插件窗口绑定域 + 强制权限校验待插件运行时排期。

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use spark_core::kernel::{DomainSignatureInfo, Kernel};
use spark_core::org::OrganizationRole;

use super::{err, lock_kernel};
use crate::{KernelState, MarketState};

/// sync-now 手动同步的单 peer 拨号超时：全局默认 10s 面向后台保活/反熵；
/// 手动刷新是用户可感路径，不可达成员 5s 快速失败，避免整批串行拨号拖住 UI。
const SYNC_NOW_DIAL_TIMEOUT: Duration = Duration::from_secs(5);

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

/// `plugin-org-sync-now`（ipc/plugin.ts:71-136）：按 orgId 直接同步——
/// 逐成员（admin 优先、跳过自己）定向拉取，返回尝试/成功计数。
///
/// 组织与插件无绑定（`basePluginDomain` 已删除）：不再有「组织归属当前
/// 插件域」校验；`plugin_domain` 仅保留为命令入参（空值校验），不再参与
/// 组织匹配。
///
/// 与 TS 的两处实现差异（语义等价）：
/// - TS `ensureCoreServicesStarted` → 内核幂等 `start_p2p`；
/// - TS 逐成员 `pullOrganizationsFromPeer` → 内核 `sync_peer_organizations`
///   （同一对账编排：双向 stale 推送 + org-pull + removed 清理）。TS 的成功
///   判定 `pulled > 0 || synced > 0` 中 `synced` 恒等于 `pulled`，故对齐到
///   内核只看 `pull_synced`（内核 `synced` 是反推成功数，对应 TS `pushed`，
///   TS 未计入）。成员仅报 peerId 不带地址时内核对账报"地址缺失"，与 TS
///   拨号失败一样计入 attempted 后跳过。
///
/// 拨号走 `sync_peer_organizations_with_dial_timeout`（5s，见
/// `SYNC_NOW_DIAL_TIMEOUT`）：手动同步是用户可感路径，不可达成员快速失败。
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
        match kernel.sync_peer_organizations_with_dial_timeout(&node_info, SYNC_NOW_DIAL_TIMEOUT) {
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
pub async fn plugin_org_sync_now(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    plugin_domain: String,
) -> Result<OrgSyncNowResultDto, String> {
    // 逐成员定向拨号是阻塞网络 IO（不可达 peer 需等拨号超时），挪入阻塞
    // 线程执行，不占命令调用线程（模式同 commands/market.rs `run_market`）；
    // `org_sync_now_inner` 同步签名保留，测试直调不受影响。
    let kernel = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = kernel
            .lock()
            .map_err(|_| "kernel state lock poisoned".to_string())?;
        org_sync_now_inner(&mut guard, &org_id, &plugin_domain)
    })
    .await
    .map_err(|e| format!("kernel task join failed: {e}"))?
}

/// `plugin-background-sync`：插件后台运行时对账（幂等）。前端在登录/
/// 身份切换进入主界面时调用——身份切换会停全部插件后台（插件数据不跨
/// 身份，见 kernel `align_storage`），靠本次对账按当前身份重新拉起；
/// 应用启动与启用/禁用/卸载的对账由壳层钩子直接触发，不经本命令。
#[tauri::command]
pub fn plugin_background_sync(
    webview: tauri::Webview,
    app: tauri::AppHandle,
    kernel_state: tauri::State<'_, KernelState>,
    market_state: tauri::State<'_, MarketState>,
) -> Result<(), String> {
    crate::domain_guard::require_system_domain(&webview)?;
    let data_dir = crate::resolve_data_dir(&app)
        .map_err(|e| format!("data dir unavailable: {e}"))?;
    crate::plugin_runtime::sync_from_market(&data_dir, kernel_state.inner(), market_state.inner());
    Ok(())
}

/// `plugin-background-running`：插件后台运行时下线/存活查询（bot 在线状态
/// 的权威来源；会话头部等展示层用）。
#[tauri::command]
pub fn plugin_background_running(
    webview: tauri::Webview,
    state: tauri::State<'_, KernelState>,
    plugin_id: String,
) -> Result<bool, String> {
    crate::domain_guard::require_system_domain(&webview)?;
    Ok(lock_kernel(&state)?.plugin_background_running(&plugin_id))
}

/// `plugin-host-query`：宿主 → 插件后台运行时反向查询（如删除联系人前的
/// 「bot 还在吗」询问）。插件未运行/超时（2s）返回 None——调用方按「查询
/// 无结果」的保守语义处理（删除询问场景即放行）。
/// spawn_blocking：内核侧同步阻塞等应答，不占命令调用线程；锁内仅克隆查询
/// 句柄（Arc 共享）即释锁，2s 等待不占内核全局锁，其他命令不被卡住。
#[tauri::command]
pub async fn plugin_host_query(
    webview: tauri::Webview,
    state: tauri::State<'_, KernelState>,
    plugin_id: String,
    kind: String,
    payload: serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    crate::domain_guard::require_system_domain(&webview)?;
    let kernel = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let query = kernel
            .lock()
            .map_err(|_| "kernel state lock poisoned".to_string())?
            .plugin_host_query_handle();
        Ok(query.query(&plugin_id, &kind, payload))
    })
    .await
    .map_err(|e| format!("kernel task join failed: {e}"))?
}

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    const PASSWORD: &str = "correct-horse-battery";
    const DOMAIN: &str = "plugin:spark-example";

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
    fn org_sync_now_ignores_domain_and_self_only_org() {
        let (_dir, mut kernel) = unlocked_kernel();

        // 组织与插件无绑定：建组织不带插件域；不同插件域入参同样按 orgId 同步
        let input: super::super::dto::CreateOrgInputDto = serde_json::from_value(serde_json::json!({
            "name": "微博组织"
        }))
        .unwrap();
        let view = kernel.create_org(input.into()).unwrap();
        let org_id = view.record.org_id.clone();

        // 无其他成员 → 无候选，attempted/pulled 均 0（P2P 已幂等启动）；
        // 插件域入参不再参与组织匹配，传任意域结果一致
        let result = org_sync_now_inner(&mut kernel, &org_id, "plugin:chat").unwrap();
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

    #[test]
    fn org_sync_now_unreachable_members_bounded() {
        let (_dir, mut kernel) = unlocked_kernel();

        let input: super::super::dto::CreateOrgInputDto = serde_json::from_value(serde_json::json!({
            "name": "离线组织"
        }))
        .unwrap();
        let view = kernel.create_org(input.into()).unwrap();
        let org_id = view.record.org_id.clone();

        // 两个 node_info 指向不可达地址（127.0.0.1:9 discard 端口，连接即拒）的成员
        let node_info = spark_core::org::OrganizationNodeInfo {
            peer_id: None,
            addresses: vec!["/ip4/127.0.0.1/tcp/9".to_string()],
        };
        kernel
            .org_add_member(&org_id, &"cd".repeat(32), Some(&node_info))
            .unwrap();
        kernel
            .org_add_member(&org_id, &"ef".repeat(32), Some(&node_info))
            .unwrap();

        let started = std::time::Instant::now();
        let result = org_sync_now_inner(&mut kernel, &org_id, DOMAIN).unwrap();
        let elapsed = started.elapsed();

        // 成员全不可达：计入 attempted 但 pulled=0；且整体在有限时间内返回
        // （单 peer 拨号 5s 超时 × 串行 2 成员为上界量级，连接即拒场景远快于此）
        assert_eq!(result.attempted, 2);
        assert_eq!(result.pulled, 0);
        assert!(
            elapsed < Duration::from_secs(15),
            "org_sync_now 耗时 {elapsed:?} 超出上界"
        );
    }
}
