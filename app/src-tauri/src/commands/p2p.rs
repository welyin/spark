//! P2P 起停与状态命令。事件订阅不走命令——setup 中的转发任务把
//! `broadcast::Receiver<P2pEvent>` 转成 `p2p-event` 全局事件推到 WebView。

use serde::Serialize;
use spark_core::kernel::{Kernel, PeerOrgSyncResult};

use super::dto::{OrgNodeInfoDto, P2pInfoDto};
use super::{err, lock_kernel};
use crate::KernelState;

/// `p2p-start` 返回（TS 为 `{ started: boolean }`，附带 peerId 便于诊断）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct P2pStartResultDto {
    pub started: bool,
    pub peer_id: String,
}

/// `p2p-stop` 返回（TS `{ started: false }`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct P2pStopResultDto {
    pub started: bool,
}

/// `p2p-clear-peer-records` 返回（TS `clearSavedPeerRecords(): { cleared }`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct P2pClearPeerRecordsResultDto {
    pub cleared: u64,
}

/// `p2p-list-peer-records` 行（对齐 TS `db.query` 的 `{ key, value }` 形状：
/// key 为完整存储键 `p2p:peer:record:<peerId>`，value 为记录 JSON 字符串）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PeerRecordRowDto {
    pub key: String,
    pub value: String,
}

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn start_inner(kernel: &mut Kernel) -> Result<P2pStartResultDto, String> {
    let peer_id = kernel.start_p2p().map_err(err)?;
    Ok(P2pStartResultDto {
        started: true,
        peer_id,
    })
}

pub(crate) fn stop_inner(kernel: &mut Kernel) -> Result<P2pStopResultDto, String> {
    kernel.stop_p2p().map_err(err)?;
    Ok(P2pStopResultDto { started: false })
}

pub(crate) fn status_inner(kernel: &Kernel) -> Result<P2pInfoDto, String> {
    match kernel.p2p_status().map_err(err)? {
        Some(info) => Ok(P2pInfoDto::from(info)),
        None => Ok(P2pInfoDto::stopped()),
    }
}

/// `p2p-broadcast`：message 必须为 JSON 对象（信封 body），原样进信封广播。
pub(crate) fn broadcast_inner(
    kernel: &Kernel,
    topic: &str,
    message: serde_json::Value,
) -> Result<super::dto::SuccessResult, String> {
    let body = message
        .as_object()
        .cloned()
        .ok_or_else(|| "broadcast message must be a JSON object".to_string())?;
    kernel.p2p_broadcast(topic, body).map_err(err)?;
    Ok(super::dto::SuccessResult::ok())
}

/// `p2p-clear-peer-records`：清空邻居活动记录（TS 仅需 core services 就绪，
/// 不要求 P2P 已启动；内核 `clear_peer_records` 语义一致）。
pub(crate) fn clear_peer_records_inner(
    kernel: &Kernel,
) -> Result<P2pClearPeerRecordsResultDto, String> {
    let cleared = kernel.clear_peer_records().map_err(err)?;
    Ok(P2pClearPeerRecordsResultDto { cleared })
}

/// `p2p-sync-peer-organizations`：与目标 peer 定向反熵对账（双向 stale 推送
/// + org-pull 拉取 + removed 清理），校验顺序与文案对齐 TS ipc/p2p.ts。
pub(crate) fn sync_peer_organizations_inner(
    kernel: &Kernel,
    target_peer: OrgNodeInfoDto,
) -> Result<PeerOrgSyncResult, String> {
    kernel
        .sync_peer_organizations(&spark_core::org::OrganizationNodeInfo::from(target_peer))
        .map_err(err)
}

/// `p2p-list-peer-records`：邻居活跃度记录原始键值对（测试页邻居列表用；
/// 替代 TS 测试页的裸 `db.query('p2p:peer:record:')`）。
pub(crate) fn list_peer_records_inner(kernel: &Kernel) -> Result<Vec<PeerRecordRowDto>, String> {
    let rows = kernel.list_peer_records().map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|(key, value)| PeerRecordRowDto { key, value })
        .collect())
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn p2p_start(state: tauri::State<'_, KernelState>) -> Result<P2pStartResultDto, String> {
    start_inner(&mut *lock_kernel(&state)?)
}

#[tauri::command]
pub fn p2p_stop(state: tauri::State<'_, KernelState>) -> Result<P2pStopResultDto, String> {
    stop_inner(&mut *lock_kernel(&state)?)
}

#[tauri::command]
pub fn p2p_status(state: tauri::State<'_, KernelState>) -> Result<P2pInfoDto, String> {
    status_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn p2p_broadcast(
    state: tauri::State<'_, KernelState>,
    topic: String,
    message: serde_json::Value,
) -> Result<super::dto::SuccessResult, String> {
    broadcast_inner(&*lock_kernel(&state)?, &topic, message)
}

#[tauri::command]
pub fn p2p_clear_peer_records(
    state: tauri::State<'_, KernelState>,
) -> Result<P2pClearPeerRecordsResultDto, String> {
    clear_peer_records_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn p2p_sync_peer_organizations(
    state: tauri::State<'_, KernelState>,
    target_peer: OrgNodeInfoDto,
) -> Result<PeerOrgSyncResult, String> {
    sync_peer_organizations_inner(&*lock_kernel(&state)?, target_peer)
}

#[tauri::command]
pub fn p2p_list_peer_records(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<PeerRecordRowDto>, String> {
    list_peer_records_inner(&*lock_kernel(&state)?)
}

// ------------------------------------------------------------------
// 单元测试（不启动真实网络：只验证未启动语义与停止幂等）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    #[test]
    fn status_when_stopped_and_stop_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        let info = status_inner(&kernel).unwrap();
        assert!(!info.started);
        assert_eq!(info.peer_id, None);
        // 未启动时 stop 幂等成功
        let stopped = stop_inner(&mut kernel).unwrap();
        assert!(!stopped.started);
    }

    #[test]
    fn broadcast_validation_and_not_started() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("correct-horse-battery", "alice", None).unwrap();

        // message 非对象 → 形状错误
        assert_eq!(
            broadcast_inner(&kernel, "spark-sync", serde_json::json!("str")).unwrap_err(),
            "broadcast message must be a JSON object"
        );
        // 对象但节点未启动 → TS `p2p node not started`
        assert_eq!(
            broadcast_inner(&kernel, "spark-sync", serde_json::json!({"type": "update"}))
                .unwrap_err(),
            "p2p node not started"
        );
    }

    #[test]
    fn clear_peer_records_on_empty_store() {
        // TS：clearPeerRecords 仅需 core services 就绪（= 身份存储已开），
        // 不要求 P2P 已启动
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("correct-horse-battery", "alice", None).unwrap();
        let result = clear_peer_records_inner(&kernel).unwrap();
        assert_eq!(result.cleared, 0);
        // 空库时邻居列表亦为空
        assert!(list_peer_records_inner(&kernel).unwrap().is_empty());
    }

    #[test]
    fn sync_peer_organizations_requires_started_p2p() {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("correct-horse-battery", "alice", None).unwrap();

        // 校验顺序对齐 TS：p2p 未启动 → 专用文案（先于地址校验）
        let target: OrgNodeInfoDto = serde_json::from_value(serde_json::json!({
            "peerId": "12D3KooWFake",
            "addresses": ["/ip4/127.0.0.1/tcp/1"]
        }))
        .unwrap();
        assert_eq!(
            sync_peer_organizations_inner(&kernel, target).unwrap_err(),
            "P2P node is not started. Start P2P before syncing organizations."
        );
    }
}
