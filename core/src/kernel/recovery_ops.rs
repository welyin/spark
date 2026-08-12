//! M5 延迟恢复通道内核命令。
//!
//! 四个命令均持有 `io_lock`；initiate/confirm/veto 要求解锁态，status 允许锁定态只读。
//! 纯逻辑下沉到 [`crate::recovery::RecoveryService`]，本层负责事件、安全日志与广播。
//!
//! 借用纪律（与 device_ops 同型）：所有 `&self` 派生值（now_ms/from_device 等）
//! 在 `require_storage_mut()` 之前取好；storage 的可变借用限制在块作用域内；
//! 事件/广播（需要 `&self`）在借用结束后执行。

use serde_json::json;

use crate::device::DeviceService;
use crate::kernel::dm_envelope::KIND_RECOVERY;
use crate::kernel::error::{KernelError, Result};
use crate::kernel::{Kernel, KernelStorage};
use crate::p2p::node::{P2pEvent, system_now_ms};
use crate::p2p::PeerNodeInfo;
use crate::recovery::{
    KEY_PENDING, PendingRecovery, RecoveryError, RecoveryOp, RecoveryService, RecoveryState,
};
use crate::storage::StorageBackend;

/// 发起恢复请求参数。
#[derive(Clone, Debug)]
pub struct RecoveryInitiateArgs {
    pub op: String,
    pub delay_hours: Option<f64>,
}

/// 确认恢复请求参数。
#[derive(Clone, Debug)]
pub struct RecoveryConfirmArgs {
    pub request_id: String,
    pub new_password: Option<String>,
}

/// 否决恢复请求参数。
#[derive(Clone, Debug)]
pub struct RecoveryVetoArgs {
    pub request_id: String,
}

/// 恢复状态结果。
#[derive(Clone, Debug)]
pub struct RecoveryStatusResult {
    pub pending: Option<PendingRecovery>,
    pub ready_to_confirm: bool,
}

fn recovery_error_to_kernel_error(e: RecoveryError) -> KernelError {
    match e {
        RecoveryError::TooEarly => KernelError::TooEarly,
        RecoveryError::RecoveryVetoed => KernelError::RecoveryVetoed,
        RecoveryError::RecoveryPending => KernelError::RecoveryPending,
        RecoveryError::RecoveryNotFound => KernelError::RecoveryNotFound,
        RecoveryError::VetoWindowExpired => KernelError::VetoWindowExpired,
        RecoveryError::UnsupportedOp => KernelError::UnsupportedOp,
        RecoveryError::InvalidInput => KernelError::InvalidInput,
        RecoveryError::Storage(msg) => KernelError::Internal(msg),
    }
}

impl Kernel {
    /// 本机 fromDevice 标注（peerId；p2p 未启动时为空串，仅作展示/日志）。
    fn local_from_device(&self) -> String {
        self.p2p_status()
            .ok()
            .flatten()
            .and_then(|info| info.peer_id)
            .unwrap_or_default()
    }

    /// 追加 M5 安全日志（security:log 前缀，本地 append-only 不进 pdsync）。
    fn append_recovery_log(
        storage: &mut KernelStorage,
        now_ms: i64,
        kind: &str,
        request_id: &str,
        op: &str,
        from_device: &str,
        actor: &str,
    ) -> Result<()> {
        let fields = json!({
            "requestId": request_id,
            "op": op,
            "fromDevice": from_device,
            "actor": actor,
        });
        DeviceService::append_security_log(storage, kind, fields, now_ms)
            .map_err(|e| KernelError::Internal(format!("security log failed: {e}")))?;
        Ok(())
    }

    /// 广播 recovery 信封到全部自设备（`self_device_peers` 已含 revoked 过滤
    /// 与本机排除），best-effort 不补投——照 `broadcast_device_sync` 先例。
    fn broadcast_recovery(&self, body: serde_json::Value) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(root_id) = self.require_unlocked_root_id() else {
            return;
        };
        let Ok(peers) = self.self_device_peers(&root_id) else {
            return;
        };
        let deliveries: Vec<(PeerNodeInfo, serde_json::Value)> = peers
            .into_iter()
            .filter_map(|peer| {
                self.build_dm_envelope(KIND_RECOVERY, &root_id, body.clone())
                    .ok()
                    .map(|envelope| (peer, envelope))
            })
            .collect();
        self.spawn_deliveries(deliveries);
    }

    fn emit_recovery_updated(
        &self,
        request_id: &str,
        state: &str,
        op: Option<String>,
        deadline: Option<i64>,
        from_device: &str,
    ) {
        let _ = self.event_tx.send(P2pEvent::RecoveryUpdated {
            request_id: request_id.to_string(),
            state: state.to_string(),
            op,
            deadline,
            from_device: from_device.to_string(),
        });
    }

    /// 发起恢复请求。
    pub fn recovery_initiate(&mut self, args: RecoveryInitiateArgs) -> Result<PendingRecovery> {
        self.require_unlocked_root_id()?;
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = system_now_ms();
        let from_device = self.local_from_device();

        let op = args
            .op
            .parse::<RecoveryOp>()
            .map_err(|_| KernelError::InvalidInput)?;

        let pending = {
            let storage = self.require_storage_mut()?;
            let pending = RecoveryService::<KernelStorage>::initiate(
                storage,
                now_ms,
                op.clone(),
                args.delay_hours,
            )
            .map_err(recovery_error_to_kernel_error)?;
            Self::append_recovery_log(
                storage,
                now_ms,
                "recovery_initiated",
                &pending.request_id,
                op.as_str(),
                &from_device,
                "local",
            )?;
            pending
        };

        let body = json!({
            "kind": "initiated",
            "op": op.as_str(),
            "requestId": pending.request_id,
            "deadline": pending.deadline,
            "fromDevice": from_device,
        });
        self.broadcast_recovery(body);

        self.emit_recovery_updated(
            &pending.request_id,
            "initiated",
            Some(op.as_str().to_string()),
            Some(pending.deadline),
            &from_device,
        );

        Ok(pending)
    }

    /// 确认并执行恢复请求（仅 reset_password 实现；pair_new_device 报 UnsupportedOp）。
    pub fn recovery_confirm(&mut self, args: RecoveryConfirmArgs) -> Result<PendingRecovery> {
        self.require_unlocked_root_id()?;
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = system_now_ms();
        let from_device = self.local_from_device();

        let pending = {
            let storage = self.require_storage_mut()?;
            match RecoveryService::<KernelStorage>::check_confirm_ready(
                storage,
                now_ms,
                &args.request_id,
            ) {
                Ok(p) => p,
                Err(RecoveryError::RecoveryVetoed) => {
                    // 命中 veto：置 rejected 终态 + 安全日志（仅首次跃迁留痕，
                    // 重复 confirm 不再刷日志/事件）。
                    if let Ok(Some(mut p)) = RecoveryService::<KernelStorage>::get_pending(storage)
                    {
                        if p.request_id == args.request_id {
                            if p.state == RecoveryState::Initiated {
                                let op_label = p.op.as_str().to_string();
                                p.vetoed = true;
                                p.state = RecoveryState::Vetoed;
                                storage.put(KEY_PENDING, &serde_json::to_string(&p)?)?;
                                Self::append_recovery_log(
                                    storage,
                                    now_ms,
                                    "recovery_vetoed",
                                    &args.request_id,
                                    &op_label,
                                    &from_device,
                                    "local",
                                )?;
                            }
                        }
                    }
                    return Err(KernelError::RecoveryVetoed);
                }
                Err(e) => return Err(recovery_error_to_kernel_error(e)),
            }
        };

        match pending.op {
            RecoveryOp::ResetPassword => {
                let password = args
                    .new_password
                    .as_deref()
                    .ok_or_else(|| KernelError::Internal("newPassword required".to_string()))?;
                self.reset_password_session(password)?;
            }
            RecoveryOp::PairNewDevice => {
                return Err(KernelError::UnsupportedOp);
            }
        }

        let result = {
            let storage = self.require_storage_mut()?;
            RecoveryService::<KernelStorage>::mark_committed(storage, &args.request_id)
                .map_err(recovery_error_to_kernel_error)?;
            Self::append_recovery_log(
                storage,
                now_ms,
                "recovery_committed",
                &args.request_id,
                pending.op.as_str(),
                &from_device,
                "local",
            )?;
            RecoveryService::<KernelStorage>::get_pending(storage)
                .map_err(recovery_error_to_kernel_error)?
        };

        let body = json!({
            "kind": "committed",
            "op": pending.op.as_str(),
            "requestId": args.request_id,
            "fromDevice": from_device,
        });
        self.broadcast_recovery(body);

        self.emit_recovery_updated(
            &args.request_id,
            "committed",
            Some(pending.op.as_str().to_string()),
            None,
            &from_device,
        );

        result.ok_or_else(|| KernelError::Internal("pending disappeared after commit".to_string()))
    }

    /// 否决恢复请求（发起方 veto 自己 = 取消，同路径）。
    pub fn recovery_veto(&mut self, args: RecoveryVetoArgs) -> Result<()> {
        self.require_unlocked_root_id()?;
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = system_now_ms();
        let from_device = self.local_from_device();

        let op_label = {
            let storage = self.require_storage_mut()?;
            // 日志 fields 需带 op：优先 seen（本机作接收方），退回 pending（本机作发起方）。
            let op_label = RecoveryService::<KernelStorage>::get_seen(storage, &args.request_id)
                .ok()
                .flatten()
                .map(|s| s.op.as_str().to_string())
                .or_else(|| {
                    RecoveryService::<KernelStorage>::get_pending(storage)
                        .ok()
                        .flatten()
                        .filter(|p| p.request_id == args.request_id)
                        .map(|p| p.op.as_str().to_string())
                })
                .unwrap_or_default();
            RecoveryService::<KernelStorage>::veto(storage, now_ms, &args.request_id, &from_device)
                .map_err(recovery_error_to_kernel_error)?;
            Self::append_recovery_log(
                storage,
                now_ms,
                "recovery_vetoed",
                &args.request_id,
                &op_label,
                &from_device,
                "local",
            )?;
            op_label
        };

        let body = json!({
            "kind": "vetoed",
            "requestId": args.request_id,
            "fromDevice": from_device,
        });
        self.broadcast_recovery(body);

        self.emit_recovery_updated(
            &args.request_id,
            "vetoed",
            if op_label.is_empty() {
                None
            } else {
                Some(op_label)
            },
            None,
            &from_device,
        );

        Ok(())
    }

    /// 查询恢复状态（锁定态允许只读，供 RecoverPage 展示进行中的恢复）。
    pub fn recovery_status(&self) -> Result<RecoveryStatusResult> {
        self.require_current_root_id()?;
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = system_now_ms();

        let storage = self.require_storage()?;
        let status = RecoveryService::<KernelStorage>::status(storage, now_ms)
            .map_err(recovery_error_to_kernel_error)?;
        Ok(RecoveryStatusResult {
            pending: status.pending,
            ready_to_confirm: status.ready_to_confirm,
        })
    }
}
