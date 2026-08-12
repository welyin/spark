//! M3 epoch 选择性密钥轮换编排。
//!
//! 所有公开函数均要求调用方已持有 `Kernel::io_lock`；本模块不重复加锁，
//! 避免在 device_ops / profile_ops / p2p_ops 的 io_lock 内部产生死锁。

use base64::Engine;

use crate::device::{DeviceRecord, DeviceService};
use crate::epoch::{AuthorizedDevice, EpochService, EpochState, RotationReason};
use crate::kernel::{Kernel, KernelError};
use crate::p2p::identity_store;
use crate::p2p::node::system_now_ms;
use crate::storage::StorageBackend;

fn require_x25519_self(storage: &dyn StorageBackend) -> Result<[u8; 32], KernelError> {
    identity_store::load_x25519_private_key(storage)
        .ok_or_else(|| KernelError::Internal("无法读取本机 libp2p 私钥以执行 epoch 轮换".to_string()))
}

fn build_authorized<'a>(
    records: &'a [DeviceRecord],
    pk_bufs: &'a [Option<[u8; 32]>],
) -> Vec<AuthorizedDevice<'a>> {
    let mut devices = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        if rec.revoked_at.is_some() {
            continue;
        }
        devices.push(AuthorizedDevice {
            peer: &rec.peer_id,
            device_pub_key: pk_bufs[i].as_ref(),
        });
    }
    devices
}

/// 通用 epoch 轮换入口。调用方必须已持有 io_lock。
pub fn rotate(
    kernel: &mut Kernel,
    reason: RotationReason,
) -> Result<Option<EpochState>, KernelError> {
    let root_id = kernel.require_unlocked_root_id()?;
    let my_peer = kernel.sync_node_id();
    let node_id = my_peer.clone();
    let now_ms = system_now_ms();

    let records = {
        let storage = kernel.require_storage()?;
        DeviceService::list(storage.raw())
            .map_err(|e| KernelError::Internal(format!("读取设备清单失败: {e}")))?
    };
    let pk_bufs: Vec<Option<[u8; 32]>> = records
        .iter()
        .map(|rec| {
            rec.device_pub_key
                .as_ref()
                .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
                .and_then(|bytes| bytes.try_into().ok())
        })
        .collect();
    let devices = build_authorized(&records, &pk_bufs);

    let kverify = crate::kernel::pw_ops::derive_session_kverify(kernel)?;

    let storage = kernel.require_storage_mut()?;
    let self_x25519_priv = require_x25519_self(storage.raw())?;

    EpochService::rotate(
        storage.raw_mut(),
        &root_id,
        &my_peer,
        &node_id,
        now_ms,
        reason,
        &self_x25519_priv,
        &devices,
        kverify.as_ref(),
    )
    .map(Some)
    .map_err(|e| KernelError::Internal(format!("epoch 轮换失败: {e}")))
}

/// 在 p2p 启动成功尾段调用：当 epoch:state 与 effective 均缺失时做
/// reason:"init" 的幂等初始化轮换。
pub fn maybe_init_epoch_state(kernel: &mut Kernel) -> Result<(), KernelError> {
    let _root_id = kernel.require_unlocked_root_id()?;
    let state_missing = {
        let storage = kernel.require_storage()?;
        crate::epoch::get_epoch_state(storage.raw())
            .ok()
            .flatten()
            .is_none()
    };
    // `get_effective` 在键缺失时返回 Ok(0)，不能区分「未初始化」与「显式 0」，
    // 因此必须按原始键是否存在判断。
    let effective_missing = {
        let storage = kernel.require_storage()?;
        storage.get(crate::epoch::EFFECTIVE_KEY)?.is_none()
    };
    if state_missing && effective_missing {
        rotate(kernel, RotationReason::Init)?;
    }
    Ok(())
}

/// 撤销设备快照广播后调用：reason:"revoke"。
pub fn after_revoke_snapshot(kernel: &mut Kernel, _revoked_peer: &str) -> Result<(), KernelError> {
    rotate(kernel, RotationReason::Revoke)?;
    Ok(())
}

/// 修改解锁密码后调用：reason:"password_change"。
pub fn after_password_change(kernel: &mut Kernel) -> Result<(), KernelError> {
    rotate(kernel, RotationReason::PasswordChange)?;
    Ok(())
}

/// 重置密码会话后调用：reason:"password_reset"。
pub fn after_password_reset(kernel: &mut Kernel) -> Result<(), KernelError> {
    rotate(kernel, RotationReason::PasswordReset)?;
    Ok(())
}
