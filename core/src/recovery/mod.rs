//! M5 延迟恢复通道纯逻辑。
//!
//! 本模块只操作 `security:recovery:*` 本地安全前缀，不进 pdsync；
//! 所有方法均为无状态静态方法，时间由调用方以 `now_ms: i64` 注入，
//! 对齐 coding-standards §3.1。

use rand::Rng as _;
use serde::{Deserialize, Serialize};

use crate::storage::StorageBackend;

/// DM 信封 kind：延迟恢复通道。
pub const RECOVERY_KIND: &str = "system/recovery";

/// 恢复请求单槽键。
pub const KEY_PENDING: &str = "security:recovery:pending";
/// 已到达恢复信封记录前缀。
pub const PREFIX_SEEN: &str = "security:recovery:seen:";
/// 否决墓碑前缀（用于乱序到达的否决）。
pub const PREFIX_VETO: &str = "security:recovery:veto:";

/// 最小延迟（小时）。
pub const MIN_DELAY_HOURS: f64 = 0.001;
/// 默认延迟（小时）。
pub const DEFAULT_DELAY_HOURS: f64 = 24.0;
/// 最大否决窗口（7 天，毫秒）。
pub const MAX_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 恢复操作类型。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOp {
    /// 重置身份密码。
    ResetPassword,
    /// 配对新设备。
    PairNewDevice,
}

impl RecoveryOp {
    /// 操作字符串（与序列化一致）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ResetPassword => "reset_password",
            Self::PairNewDevice => "pair_new_device",
        }
    }
}

impl std::str::FromStr for RecoveryOp {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reset_password" => Ok(Self::ResetPassword),
            "pair_new_device" => Ok(Self::PairNewDevice),
            _ => Err(()),
        }
    }
}

/// 恢复请求状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    /// 已发起，等待确认。
    Initiated,
    /// 已被否决。
    Vetoed,
    /// 已确认并执行。
    Committed,
}

/// 本地待确认恢复请求（单槽）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingRecovery {
    pub request_id: String,
    pub op: RecoveryOp,
    pub deadline: i64,
    pub initiated_at: i64,
    pub vetoed: bool,
    pub state: RecoveryState,
}

/// 已到达的恢复信封记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeenRecovery {
    pub request_id: String,
    pub op: RecoveryOp,
    pub from_device: String,
    pub local_arrival_ms: i64,
    pub window_ms: i64,
    pub vetoed: bool,
    pub committed: bool,
}

/// 否决墓碑。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VetoTombstone {
    pub vetoed_at: i64,
    pub from_device: String,
}

/// 恢复状态聚合（供前端查询）。
#[derive(Clone, Debug)]
pub struct RecoveryStatus {
    pub pending: Option<PendingRecovery>,
    pub ready_to_confirm: bool,
}

/// 恢复流程错误。
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RecoveryError {
    #[error("TooEarly")]
    TooEarly,
    #[error("RecoveryVetoed")]
    RecoveryVetoed,
    #[error("RecoveryPending")]
    RecoveryPending,
    #[error("RecoveryNotFound")]
    RecoveryNotFound,
    #[error("VetoWindowExpired")]
    VetoWindowExpired,
    #[error("UnsupportedOp")]
    UnsupportedOp,
    #[error("InvalidInput")]
    InvalidInput,
    #[error("Storage error: {0}")]
    Storage(String),
}

impl From<crate::storage::StorageError> for RecoveryError {
    fn from(e: crate::storage::StorageError) -> Self {
        Self::Storage(e.to_string())
    }
}

impl From<serde_json::Error> for RecoveryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

/// 纯逻辑恢复服务，无状态，泛型于存储后端。
pub struct RecoveryService<S: StorageBackend> {
    _marker: std::marker::PhantomData<S>,
}

impl<S: StorageBackend> RecoveryService<S> {
    /// 生成恢复请求 id：`rc` + 16 B 十六进制。
    fn generate_request_id() -> String {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        format!("rc{}", hex::encode(bytes))
    }

    /// 把延迟小时数转换成截止毫秒数。
    fn deadline_from_delay(now_ms: i64, delay_hours: f64) -> i64 {
        now_ms + (delay_hours * 3_600_000.0) as i64
    }

    /// 计算接收方否决窗口毫秒数。
    /// 窗口 = deadline - 信封 ts，限制在 (0, 7d]。
    pub fn recovery_window_ms(deadline: i64, envelope_ts: i64) -> i64 {
        let raw = deadline.saturating_sub(envelope_ts);
        raw.clamp(1, MAX_WINDOW_MS)
    }

    /// 读取待确认请求。
    pub fn get_pending(storage: &S) -> Result<Option<PendingRecovery>, RecoveryError> {
        match storage.get(KEY_PENDING)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// 读取已到达记录。
    pub fn get_seen(storage: &S, request_id: &str) -> Result<Option<SeenRecovery>, RecoveryError> {
        let key = format!("{PREFIX_SEEN}{request_id}");
        match storage.get(&key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// 读取否决墓碑。
    pub fn get_veto_tombstone(
        storage: &S,
        request_id: &str,
    ) -> Result<Option<VetoTombstone>, RecoveryError> {
        let key = format!("{PREFIX_VETO}{request_id}");
        match storage.get(&key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// 判断 pending 是否处于非终态。
    fn is_active_pending(pending: &PendingRecovery) -> bool {
        pending.state == RecoveryState::Initiated
    }

    /// 发起恢复请求：检查无活跃 pending，生成 request_id 与 deadline，写入单槽。
    pub fn initiate(
        storage: &mut S,
        now_ms: i64,
        op: RecoveryOp,
        delay_hours: Option<f64>,
    ) -> Result<PendingRecovery, RecoveryError> {
        if matches!(op, RecoveryOp::PairNewDevice) {
            return Err(RecoveryError::UnsupportedOp);
        }
        let delay = delay_hours.unwrap_or(DEFAULT_DELAY_HOURS);
        if delay < MIN_DELAY_HOURS {
            return Err(RecoveryError::InvalidInput);
        }

        if let Some(pending) = Self::get_pending(storage)? {
            if Self::is_active_pending(&pending) {
                return Err(RecoveryError::RecoveryPending);
            }
        }

        let request_id = Self::generate_request_id();
        let deadline = Self::deadline_from_delay(now_ms, delay);
        let pending = PendingRecovery {
            request_id: request_id.clone(),
            op,
            deadline,
            initiated_at: now_ms,
            vetoed: false,
            state: RecoveryState::Initiated,
        };
        storage.put(KEY_PENDING, &serde_json::to_string(&pending)?)?;
        Ok(pending)
    }

    /// 本地确认检查：命中 pending、request_id 一致、未到截止时间、未被否决。
    pub fn check_confirm_ready(
        storage: &S,
        now_ms: i64,
        request_id: &str,
    ) -> Result<PendingRecovery, RecoveryError> {
        let Some(pending) = Self::get_pending(storage)? else {
            return Err(RecoveryError::RecoveryNotFound);
        };
        if pending.request_id != request_id {
            return Err(RecoveryError::RecoveryNotFound);
        }
        if pending.state == RecoveryState::Committed {
            return Err(RecoveryError::RecoveryNotFound);
        }
        if now_ms < pending.deadline {
            return Err(RecoveryError::TooEarly);
        }
        if pending.vetoed {
            return Err(RecoveryError::RecoveryVetoed);
        }
        if Self::get_veto_tombstone(storage, request_id)?.is_some() {
            return Err(RecoveryError::RecoveryVetoed);
        }
        Ok(pending)
    }

    /// 标记请求为已提交。
    pub fn mark_committed(storage: &mut S, request_id: &str) -> Result<(), RecoveryError> {
        let key = format!("{PREFIX_SEEN}{request_id}");
        if let Some(raw) = storage.get(&key)? {
            let mut seen: SeenRecovery = serde_json::from_str(&raw)?;
            seen.committed = true;
            storage.put(&key, &serde_json::to_string(&seen)?)?;
        }

        if let Some(mut pending) = Self::get_pending(storage)? {
            // 终态不可逆：已 Vetoed 不能再被提交。
            if pending.request_id == request_id
                && pending.state != RecoveryState::Committed
                && pending.state != RecoveryState::Vetoed
            {
                pending.state = RecoveryState::Committed;
                storage.put(KEY_PENDING, &serde_json::to_string(&pending)?)?;
            }
        }
        Ok(())
    }

    /// 否决：在窗口内对 seen 或 pending 命中 request_id 时置 veto 标记。
    pub fn veto(
        storage: &mut S,
        now_ms: i64,
        request_id: &str,
        from_device: &str,
    ) -> Result<bool, RecoveryError> {
        let mut window_end: Option<i64> = None;

        if let Some(seen) = Self::get_seen(storage, request_id)? {
            window_end = Some(seen.local_arrival_ms.saturating_add(seen.window_ms));
        } else if let Some(pending) = Self::get_pending(storage)? {
            if pending.request_id == request_id {
                // pending 由本机发起，无 arrival/window；用 deadline 作为窗口边界。
                window_end = Some(pending.deadline);
            }
        }

        let Some(window_end) = window_end else {
            return Err(RecoveryError::RecoveryNotFound);
        };

        if now_ms > window_end {
            return Err(RecoveryError::VetoWindowExpired);
        }

        // 写/刷新墓碑。
        let tombstone = VetoTombstone {
            vetoed_at: now_ms,
            from_device: from_device.to_string(),
        };
        storage.put(
            &format!("{PREFIX_VETO}{request_id}"),
            &serde_json::to_string(&tombstone)?,
        )?;

        // 标记 seen。
        let key = format!("{PREFIX_SEEN}{request_id}");
        if let Some(raw) = storage.get(&key)? {
            let mut seen: SeenRecovery = serde_json::from_str(&raw)?;
            seen.vetoed = true;
            storage.put(&key, &serde_json::to_string(&seen)?)?;
        }

        // 标记 pending；已 Committed 的终态不可逆。
        if let Some(mut pending) = Self::get_pending(storage)? {
            if pending.request_id == request_id && pending.state != RecoveryState::Committed {
                pending.vetoed = true;
                pending.state = RecoveryState::Vetoed;
                storage.put(KEY_PENDING, &serde_json::to_string(&pending)?)?;
            }
        }

        Ok(true)
    }

    /// 处理入站 initiated 信封：写入 seen 记录，若已存在墓碑则置 vetoed。
    /// 返回 (seen, was_created, is_vetoed)。
    pub fn apply_inbound_initiated(
        storage: &mut S,
        now_ms: i64,
        request_id: &str,
        op: RecoveryOp,
        from_device: &str,
        window_ms: i64,
    ) -> Result<(SeenRecovery, bool, bool), RecoveryError> {
        let key = format!("{PREFIX_SEEN}{request_id}");
        let existed = storage.get(&key)?.is_some();

        let vetoed = Self::get_veto_tombstone(storage, request_id)?.is_some();

        let seen = SeenRecovery {
            request_id: request_id.to_string(),
            op,
            from_device: from_device.to_string(),
            local_arrival_ms: now_ms,
            window_ms,
            vetoed,
            committed: false,
        };

        if !existed {
            storage.put(&key, &serde_json::to_string(&seen)?)?;
        }

        // 若 pending 单槽同名，按入站覆盖更新（自设备管道可能本机也发起了同 id）。
        if let Some(mut pending) = Self::get_pending(storage)? {
            if pending.request_id == request_id && !pending.vetoed {
                pending.vetoed = vetoed;
                if vetoed && pending.state == RecoveryState::Initiated {
                    pending.state = RecoveryState::Vetoed;
                }
                storage.put(KEY_PENDING, &serde_json::to_string(&pending)?)?;
            }
        }

        Ok((seen, !existed, vetoed))
    }

    /// 处理入站 vetoed 信封：无条件写墓碑，标记 seen/pending。
    /// 返回 `changed` = 墓碑新建 或 seen/pending 首次置 veto（重复 veto 幂等
    /// 返回 false，调用侧据此决定是否发事件/写日志）。
    pub fn apply_inbound_vetoed(
        storage: &mut S,
        now_ms: i64,
        request_id: &str,
        from_device: &str,
    ) -> Result<bool, RecoveryError> {
        let tombstone_key = format!("{PREFIX_VETO}{request_id}");
        let tombstone_new = storage.get(&tombstone_key)?.is_none();
        let tombstone = VetoTombstone {
            vetoed_at: now_ms,
            from_device: from_device.to_string(),
        };
        storage.put(&tombstone_key, &serde_json::to_string(&tombstone)?)?;

        let mut changed = tombstone_new;
        let key = format!("{PREFIX_SEEN}{request_id}");
        if let Some(raw) = storage.get(&key)? {
            let mut seen: SeenRecovery = serde_json::from_str(&raw)?;
            if !seen.vetoed {
                seen.vetoed = true;
                storage.put(&key, &serde_json::to_string(&seen)?)?;
                changed = true;
            }
        }

        if let Some(mut pending) = Self::get_pending(storage)? {
            // 终态不可逆：已 Committed 不能再被否决。
            if pending.request_id == request_id
                && pending.state != RecoveryState::Committed
                && !pending.vetoed
            {
                pending.vetoed = true;
                pending.state = RecoveryState::Vetoed;
                storage.put(KEY_PENDING, &serde_json::to_string(&pending)?)?;
                changed = true;
            }
        }

        Ok(changed)
    }

    /// 处理入站 committed 信封：seen 命中置 committed；未知静默成功。
    pub fn apply_inbound_committed(
        storage: &mut S,
        request_id: &str,
    ) -> Result<bool, RecoveryError> {
        let key = format!("{PREFIX_SEEN}{request_id}");
        let Some(raw) = storage.get(&key)? else {
            return Ok(false);
        };
        let mut seen: SeenRecovery = serde_json::from_str(&raw)?;
        if seen.committed {
            return Ok(false);
        }
        // 终态不可逆：已否决的 seen 不再接受 committed 覆盖；保持 veto 优先语义。
        if seen.vetoed {
            return Ok(false);
        }
        seen.committed = true;
        storage.put(&key, &serde_json::to_string(&seen)?)?;
        Ok(true)
    }

    /// 聚合状态查询。
    pub fn status(storage: &S, now_ms: i64) -> Result<RecoveryStatus, RecoveryError> {
        let pending = Self::get_pending(storage)?;
        let ready_to_confirm = pending.as_ref().map_or(false, |p| {
            p.state == RecoveryState::Initiated
                && !p.vetoed
                && now_ms >= p.deadline
                && Self::get_veto_tombstone(storage, &p.request_id)
                    .ok()
                    .flatten()
                    .is_none()
        });
        Ok(RecoveryStatus {
            pending,
            ready_to_confirm,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn mem() -> MemoryStorage {
        MemoryStorage::new()
    }

    #[test]
    fn initiate_and_status() {
        let mut s = mem();
        let pending = RecoveryService::<MemoryStorage>::initiate(
            &mut s,
            1_000_000,
            RecoveryOp::ResetPassword,
            Some(1.0),
        )
        .unwrap();
        assert!(pending.request_id.starts_with("rc"));
        assert_eq!(pending.op, RecoveryOp::ResetPassword);
        assert_eq!(pending.deadline, 1_000_000 + 3_600_000);

        let status = RecoveryService::<MemoryStorage>::status(&s, 1_000_000).unwrap();
        assert!(status.pending.is_some());
        assert!(!status.ready_to_confirm);

        let status = RecoveryService::<MemoryStorage>::status(&s, pending.deadline).unwrap();
        assert!(status.ready_to_confirm);
    }

    #[test]
    fn pair_new_device_unsupported() {
        let mut s = mem();
        let err =
            RecoveryService::<MemoryStorage>::initiate(&mut s, 0, RecoveryOp::PairNewDevice, None)
                .unwrap_err();
        assert_eq!(err, RecoveryError::UnsupportedOp);
    }

    #[test]
    fn veto_window_expires() {
        let mut s = mem();
        let pending = RecoveryService::<MemoryStorage>::initiate(
            &mut s,
            0,
            RecoveryOp::ResetPassword,
            Some(0.001),
        )
        .unwrap();
        let window_ms = RecoveryService::<MemoryStorage>::recovery_window_ms(pending.deadline, 0);

        // 在窗口内否决 pending。
        let ok = RecoveryService::<MemoryStorage>::veto(
            &mut s,
            window_ms - 1,
            &pending.request_id,
            "d1",
        )
        .unwrap();
        assert!(ok);

        // 再次否决应失败（窗口已过）。
        let err = RecoveryService::<MemoryStorage>::veto(
            &mut s,
            window_ms + 2,
            &pending.request_id,
            "d1",
        )
        .unwrap_err();
        assert_eq!(err, RecoveryError::VetoWindowExpired);
    }

    #[test]
    fn inbound_initiated_then_vetoed() {
        let mut s = mem();
        let now = 1_000_000;
        let deadline = now + 3_600_000;
        let window_ms = RecoveryService::<MemoryStorage>::recovery_window_ms(deadline, now);

        let (seen, created, vetoed) = RecoveryService::<MemoryStorage>::apply_inbound_initiated(
            &mut s,
            now,
            "rc123",
            RecoveryOp::ResetPassword,
            "d1",
            window_ms,
        )
        .unwrap();
        assert!(created);
        assert!(!vetoed);
        assert_eq!(seen.local_arrival_ms, now);

        // 乱序否决到达。
        RecoveryService::<MemoryStorage>::apply_inbound_vetoed(&mut s, now + 1000, "rc123", "d2")
            .unwrap();
        let seen = RecoveryService::<MemoryStorage>::get_seen(&s, "rc123")
            .unwrap()
            .unwrap();
        assert!(seen.vetoed);
    }
}
