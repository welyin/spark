//! 口令校验器内核编排（E3：乙 + V + D′ 三件套）。
//!
//! 所有对外 Kernel 方法都在 `io_lock` 内执行。本模块不直接触碰 p2p/网络；
//! V/ack 的构造与解析复用 `core::pw`（事实源：wiki/protocol/p2p/personal-data-sync.md §13）。
//!
//! 挂点：
//! - change_password / reset_password_session 收敛点 → `publish_pw_value`
//! - 创世（init/recover） → `publish_pw_value`
//! - unlock 懒发布 + 自动 ack → `maybe_publish_on_unlock` / `maybe_ack_on_unlock`
//! - shell 命令支撑：verify_password_ticket / unify_password / password_unify_status
//! - 自愈：maybe_heal（24h 阈值，与 graceMs 7d 解耦）

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::json;

use crate::device::{DeviceRecord, DeviceService};
use crate::epoch::RotationReason;
use crate::identity;
use crate::kernel::{Kernel, KernelError, Result};
use crate::p2p::{P2pEvent, node::system_now_ms};
use crate::pw::{self, put_last_verified_vts};
use crate::storage::ScanOptions;
use rand::Rng as _;

/// D′ 自愈：包裹超时阈值（24 小时）。
const HEAL_THRESHOLD_MS: i64 = 24 * 60 * 60 * 1000;

/// 从当前解锁会话派生 `Kverify`（scrypt(会话口令, pwv.salt)）。
///
/// - 未解锁 → `Ok(None)`。
/// - 无 V → `Ok(None)`。
/// - 有 V 有口令 → 返回 32B Kverify，**仅存内存，随 lock 清除**。
pub(crate) fn derive_session_kverify(kernel: &Kernel) -> Result<Option<[u8; 32]>> {
    let Some(unlocked) = kernel.unlocked.as_ref() else {
        return Ok(None);
    };
    let storage = kernel.require_storage()?;
    let Some(pwv) = pw::get_pwv(storage)? else {
        return Ok(None);
    };
    let salt = B64
        .decode(&pwv.salt)
        .map_err(|e| KernelError::Internal(format!("pwv salt decode: {e}")))?
        .try_into()
        .map_err(|_| KernelError::Internal("pwv salt length".into()))?;
    Ok(Some(pw::derive_kverify(&unlocked.password, &salt)?))
}

/// 获取 io_lock 守卫（不借用 Kernel，允许在持锁期间调用 `&mut self` 方法）。
macro_rules! io_guard {
    ($kernel:expr) => {
        #[allow(unused_variables)]
        let __io_arc = std::sync::Arc::clone(&$kernel.io_lock);
        #[allow(unused_variables)]
        let _io_guard = __io_arc.lock().unwrap_or_else(|e| e.into_inner());
    };
}

/// 发布一条 `pwv:self`（四挂点共用）。
///
/// 调用方已持 `io_lock`；本函数不重复上锁。
pub(crate) fn publish_pw_value(
    kernel: &mut Kernel,
    password: &str,
    reason: RotationReason,
) -> Result<()> {
    let node_id = kernel.sync_node_id();
    let now_ms = system_now_ms();

    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);

    let pwv = pw::build_value(password, &salt, &nonce, now_ms as u64, &node_id)?;

    let storage = kernel.require_storage_mut()?;
    pw::apply_value(storage, &node_id, &pwv, now_ms)?;

    DeviceService::append_security_log(
        storage,
        "pw_verifier_published",
        json!({
            "deviceId": node_id,
            "changedAt": pwv.changed_at,
            "reason": reason.as_str(),
        }),
        now_ms,
    )?;

    // E5 事件：改密/重置（用户主动换锁）才广播 `PasswordChangeObserved` 作 UI 提示；
    // 创世（init/recover）与 unlock 懒发布（首次补 V）不属于「观察到的口令变更」。
    if matches!(
        reason,
        RotationReason::PasswordChange | RotationReason::PasswordReset
    ) {
        emit_password_change_observed(kernel, &pwv, &node_id, reason);
    }

    Ok(())
}

/// 广播 `PasswordChangeObserved`（E5）：载荷字段由 pwv + 设备清单装配。
fn emit_password_change_observed(
    kernel: &Kernel,
    pwv: &crate::pw::PasswordVerifier,
    node_id: &str,
    reason: RotationReason,
) {
    let rotated_by_device = kernel
        .require_storage()
        .ok()
        .and_then(|storage| DeviceService::list(storage.raw()).ok())
        .and_then(|records| {
            records
                .iter()
                .find(|r: &&DeviceRecord| r.peer_id == pwv.changed_by)
                .map(|r| r.device_name.clone())
        })
        .unwrap_or_else(|| node_id.to_string());
    let _ = kernel.event_tx.send(P2pEvent::PasswordChangeObserved {
        rotated_at: pwv.changed_at,
        rotated_by: pwv.changed_by.clone(),
        rotated_by_device,
        reason: reason.as_str().to_string(),
    });
}

/// unlock 时：若本地尚无 `pwv:self`（存量账号），用手持口令就地计算 V 并静默迁移。
/// 调用方已持 `io_lock`。
pub(crate) fn maybe_publish_on_unlock(kernel: &mut Kernel, password: &str) -> Result<()> {
    let has_v = {
        let storage = kernel.require_storage()?;
        pw::get_pwv(storage)?.is_some()
    };
    // 懒发布用 Init：这是「首次补 V 的静默迁移」，不是「观察到的他人改密」——
    // 若用 PasswordChange 会误触发 `PasswordChangeObserved` 广播（E5 🟡）。
    if !has_v {
        publish_pw_value(kernel, password, RotationReason::Init)?;
    }
    Ok(())
}

/// unlock 时自动 ack：合法设备输入正确口令即天然完成 D′ ack，无需额外步骤。
/// 调用方已持 `io_lock`。
pub(crate) fn maybe_ack_on_unlock(kernel: &mut Kernel, password: &str) -> Result<()> {
    let node_id = kernel.sync_node_id();
    let storage = kernel.require_storage_mut()?;
    let Some(pwv) = pw::get_pwv(storage)? else {
        return Ok(());
    };

    if !pw::verify_value(&pwv, password) {
        // 当前 LWW V 无法用手持口令验证：标记 stale，但不破坏已有 last-good。
        pw::put_stale(storage, true)?;
        DeviceService::append_security_log(
            storage,
            "pw_verifier_mismatch",
            json!({ "expectedChangedAt": pwv.changed_at }),
            system_now_ms(),
        )?;
        return Ok(());
    }

    let applied = pw::get_applied_vts(storage)?;
    if pwv.changed_at <= applied {
        return Ok(());
    }

    let salt = B64
        .decode(&pwv.salt)
        .map_err(|e| KernelError::Internal(format!("pwv salt decode: {e}")))?
        .try_into()
        .map_err(|_| KernelError::Internal("pwv salt length".into()))?;
    let kverify = pw::derive_kverify(password, &salt)?;
    let ack = pw::build_ack(&kverify, &node_id, pwv.changed_at);

    let now_ms = system_now_ms();
    pw::put_pwack(storage, &node_id, &node_id, &ack, now_ms)?;
    pw::put_applied_vts(storage, pwv.changed_at)?;
    put_last_verified_vts(storage, &node_id, pwv.changed_at)?;
    pw::put_stale(storage, false)?;

    DeviceService::append_security_log(
        storage,
        "pw_ack_auto",
        json!({
            "deviceId": node_id,
            "vTs": pwv.changed_at,
        }),
        now_ms,
    )?;

    Ok(())
}

/// 用候选口令验票（不动身份文件）。
/// 三态：TicketUnavailable / TicketMismatch / Ok。
pub(crate) fn verify_password_ticket(kernel: &Kernel, password: &str) -> Result<()> {
    io_guard!(kernel);
    let storage = kernel.require_storage()?;
    match pw::get_pwv(storage)? {
        Some(pwv) => {
            if pw::verify_value(&pwv, password) {
                Ok(())
            } else {
                Err(KernelError::TicketMismatch)
            }
        }
        None => Err(KernelError::TicketUnavailable),
    }
}

/// 重封口令：V 复验 → identity::change_password → set_unlocked 刷新会话 → 水位 → ack → 清 stale。
///
/// `reason` 用于区分普通改密后的收敛与恢复重置后的收敛，影响安全日志 kind。
pub(crate) fn unify_password(
    kernel: &mut Kernel,
    old_password: &str,
    new_password: &str,
    reason: RotationReason,
) -> Result<()> {
    io_guard!(kernel);

    let unlocked = kernel.unlocked.as_ref().ok_or(KernelError::Locked)?;

    let file = kernel
        .read_identity_file(&unlocked.root_id())?
        .ok_or(KernelError::NotInitialized)?;

    let (new_file, new_key) = identity::change_password(&file, old_password, new_password)
        .map_err(|e| match e {
            identity::IdentityError::DecryptionFailed => KernelError::InvalidPassword,
            other => KernelError::Identity(other),
        })?;

    // 内核复验 V：防止本地会话口令与全局 V 不一致时被绕过。
    {
        let storage = kernel.require_storage()?;
        match pw::get_pwv(storage)? {
            Some(pwv) => {
                if !pw::verify_value(&pwv, old_password) {
                    return Err(KernelError::TicketMismatch);
                }
            }
            None => return Err(KernelError::TicketUnavailable),
        }
    }

    kernel.write_identity_file(&new_file)?;
    kernel.set_unlocked(
        unlocked.identity.clone(),
        unlocked.seed,
        new_password,
        Some(new_key),
    );

    // 刷新会话后：水位推进 + ack + 清 stale。
    {
        let node_id = kernel.sync_node_id();
        let storage = kernel.require_storage_mut()?;
        let pwv = pw::get_pwv(storage)?.ok_or(KernelError::TicketUnavailable)?;

        let salt = B64
            .decode(&pwv.salt)
            .map_err(|e| KernelError::Internal(format!("pwv salt decode: {e}")))?
            .try_into()
            .map_err(|_| KernelError::Internal("pwv salt length".into()))?;
        let kverify = pw::derive_kverify(old_password, &salt)?;
        let ack = pw::build_ack(&kverify, &node_id, pwv.changed_at);

        let now_ms = system_now_ms();
        pw::put_pwack(storage, &node_id, &node_id, &ack, now_ms)?;
        pw::put_applied_vts(storage, pwv.changed_at)?;
        put_last_verified_vts(storage, &node_id, pwv.changed_at)?;
        pw::put_stale(storage, false)?;

        DeviceService::append_security_log(
            storage,
            "pw_verified_resealed",
            json!({
                "deviceId": node_id,
                "vTs": pwv.changed_at,
            }),
            now_ms,
        )?;

        if reason == RotationReason::PasswordReset {
            DeviceService::append_security_log(
                storage,
                "password_unified_after_reset",
                json!({
                    "deviceId": node_id,
                    "vTs": pwv.changed_at,
                }),
                now_ms,
            )?;
        }

        // E5：统一完成，撤其他设备的改密提示。
        let _ = kernel.event_tx.send(P2pEvent::PasswordUnificationDone {
            rotated_at: pwv.changed_at,
        });
    }

    Ok(())
}

/// 密码统一状态查询（锁定态可调）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PasswordUnifyStatus {
    pub stale: bool,
    pub v_changed_at: Option<u64>,
    pub v_changed_by: Option<String>,
    pub v_changed_by_device: Option<String>,
    pub v_changed_reason: Option<String>,
    pub applied_vts: u64,
}

pub(crate) fn password_unify_status(kernel: &Kernel) -> Result<PasswordUnifyStatus> {
    io_guard!(kernel);
    let storage = kernel.require_storage()?;
    let stale = pw::get_stale(storage)?;
    let applied_vts = pw::get_applied_vts(storage)?;
    let (v_changed_at, v_changed_by) = match pw::get_pwv(storage)? {
        Some(pwv) => (Some(pwv.changed_at), Some(pwv.changed_by)),
        None => (None, None),
    };
    // 变更原因：epoch:state.reason 为权威（仅改密/重置才置 stale 标记，其余枚举不进）。
    let v_changed_reason = crate::epoch::get_epoch_state(storage.raw())
        .ok()
        .flatten()
        .map(|state| state.reason.as_str().to_string());
    // 变更设备名：按发起改密的 peerId 查设备清单（查不到回退用 peerId 本身）。
    let v_changed_by_device = v_changed_by
        .as_ref()
        .and_then(|by| {
            DeviceService::list(storage.raw()).ok().and_then(|records| {
                records
                    .iter()
                    .find(|r| r.peer_id == *by)
                    .map(|r| r.device_name.clone())
            })
        })
        .or_else(|| v_changed_by.clone());

    // E5 `DeviceOutOfGrace`：本机判定（非广播）——曾验证设备已超 grace 未覆盖最新 V。
    // 复用 `should_gate` 对本机的门控判定：`Gated{grace_remaining_ms: Some(0)}` =
    // 曾验证但 grace 耗尽（未验证设备为 `None`，不进该事件，契约 F8 语义）。
    if v_changed_at.is_some() {
        let node_id = kernel.sync_node_id();
        let now_ms = system_now_ms();
        if let Ok(pw::GateDecision::Gated {
            last_verified_vts,
            grace_remaining_ms,
            ..
        }) = pw::should_gate(storage.raw(), &node_id, now_ms as u64)
        {
            if last_verified_vts > 0 && grace_remaining_ms == Some(0) {
                let grace_ms = pw::get_grace_ms(storage.raw()).unwrap_or(pw::DEFAULT_GRACE_MS);
                let _ = kernel.event_tx.send(P2pEvent::DeviceOutOfGrace {
                    password_changed_at: v_changed_at.unwrap_or(0),
                    grace_ms,
                });
            }
        }
    }

    Ok(PasswordUnifyStatus {
        stale,
        v_changed_at,
        v_changed_by,
        v_changed_by_device,
        v_changed_reason,
        applied_vts,
    })
}

fn has_ikey_for_epoch<S: crate::storage::StorageBackend>(
    storage: &S,
    epoch: u64,
    my_peer: &str,
) -> crate::storage::Result<bool> {
    let suffix = format!(":{my_peer}");
    let prefix = format!("ikey:{epoch}:");
    let items = storage.scan(&ScanOptions::prefix(&prefix))?;
    for (k, _v) in items {
        if k.ends_with(&suffix) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// D′ 自愈：已验证设备对当前 epoch 的 ikey 包裹超时未达（24h）→ rotate(Heal)+重发 V。
///
/// 本函数在 `io_lock` 内执行一次检查；实际触发通常由壳层定时任务/事件调用。
pub(crate) fn maybe_heal(kernel: &mut Kernel, password: &str) -> Result<bool> {
    io_guard!(kernel);

    if kernel.unlocked.is_none() {
        return Ok(false);
    }

    let storage = kernel.require_storage()?;

    // 当前 V 必须已被本机验证（applied >= V.changed_at），否则没有自愈立场。
    let Some(pwv) = pw::get_pwv(storage)? else {
        return Ok(false);
    };
    let applied = pw::get_applied_vts(storage)?;
    if applied < pwv.changed_at {
        return Ok(false);
    }

    let state = crate::epoch::get_epoch_state(storage)?;
    let Some(state) = state else {
        return Ok(false);
    };

    let now_ms = system_now_ms();
    let elapsed_ms = now_ms.saturating_sub(state.rotated_at);
    if elapsed_ms < HEAL_THRESHOLD_MS {
        return Ok(false);
    }

    // 检查本机是否已有当前 epoch 的 ikey 包裹（任意 writer）。若已存在则无需 heal。
    let my_peer = kernel.sync_node_id();
    if has_ikey_for_epoch(storage, state.current, &my_peer)? {
        return Ok(false);
    }

    // 执行 Heal 轮换并重新发布 V。
    crate::kernel::epoch_ops::rotate(kernel, RotationReason::Heal)?;
    publish_pw_value(kernel, password, RotationReason::Heal)?;

    Ok(true)
}

// ── Kernel 方法门面 ──────────────────────────────────────────────────────

impl Kernel {
    /// 用候选口令验票（`root_verify_password_ticket` 内核语义）。
    pub fn verify_password_ticket(&self, password: &str) -> Result<()> {
        verify_password_ticket(self, password)
    }

    /// 重封口令并刷新会话（`root_unify_password` 内核语义）。
    ///
    /// `reason` 用于安全日志分支；壳层普通改密收敛传
    /// [`RotationReason::PasswordChange`]，恢复重置收敛传
    /// [`RotationReason::PasswordReset`]。
    pub fn unify_password(
        &mut self,
        old_password: &str,
        new_password: &str,
        reason: RotationReason,
    ) -> Result<()> {
        unify_password(self, old_password, new_password, reason)
    }

    /// 查询口令统一状态（`root_password_unify_status` 内核语义）。
    pub fn password_unify_status(&self) -> Result<PasswordUnifyStatus> {
        password_unify_status(self)
    }

    /// unlock 后自动完成 ack 与懒发布。
    pub(crate) fn on_unlock_password_ops(&mut self, password: &str) -> Result<()> {
        io_guard!(self);
        maybe_publish_on_unlock(self, password)?;
        maybe_ack_on_unlock(self, password)
    }

    /// D′ 自愈检查（通常由定时器/事件触发）。
    pub fn maybe_heal_password(&mut self, password: &str) -> Result<bool> {
        maybe_heal(self, password)
    }
}
