//! 自设备延迟恢复通道（`system/recovery`）入站处理。
//!
//! 规格（m4-m5-mobile-plan §4.3 / p2p-dm §19.2）：三条消息全走自设备 DM
//! （信封 `from == to == me`，验签即持根私钥 + 连接层黑名单拦 revoked，
//! 无需逐消息再验设备清单）；`fromDevice` 为发送方 peerId 标注，仅用于
//! 展示/日志。方向性原则：**veto fail-safe**（requestId 非空即无条件落
//! 墓碑，乱序兜底），**initiated/committed fail-closed**（字段齐 + op
//! 合法才生效，未知 requestId 的 committed 静默 ok）。
//!
//! 时钟安全（§4.4）：否决窗 = 本地到达时间 + `clamp(deadline - 信封 ts,
//! (0, 7d])`，接收方本机时钟独立起算，不信发起方单一时钟。

use serde_json::Value;

use super::{InboundContext, InboundDmResult, Result, done, fail_response, ok_response};
use crate::device::DeviceService;
use crate::p2p::P2pEvent;
use crate::recovery::{RecoveryOp, RecoveryService};
use crate::storage::StorageBackend;

/// 追加 M5 安全日志（入站侧 actor = 信封 fromDevice，与本地命令侧
/// actor="local" 区分；§4.6：每台涉事设备各落一条）。
fn append_log<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
    kind: &str,
    request_id: &str,
    op: &str,
    from_device: &str,
) -> Result<()> {
    let fields = serde_json::json!({
        "requestId": request_id,
        "op": op,
        "fromDevice": from_device,
        "actor": from_device,
    });
    DeviceService::append_security_log(storage, kind, fields, now_ms)?;
    Ok(())
}

fn updated_event(
    request_id: &str,
    state: &str,
    op: Option<String>,
    deadline: Option<i64>,
    from_device: &str,
) -> P2pEvent {
    P2pEvent::RecoveryUpdated {
        request_id: request_id.to_string(),
        state: state.to_string(),
        op,
        deadline,
        from_device: from_device.to_string(),
    }
}

pub(super) fn handle_recovery<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    envelope_ts: i64,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self"), Vec::new());
    }
    let Some(kind) = body.get("kind").and_then(Value::as_str) else {
        return done(fail_response("invalid-kind"), Vec::new());
    };
    match kind {
        "initiated" => handle_initiated(storage, ctx, envelope_ts, body),
        "vetoed" => handle_vetoed(storage, ctx, body),
        "committed" => handle_committed(storage, ctx, body),
        _ => done(fail_response("invalid-kind"), Vec::new()),
    }
}

/// `initiated`（fail-closed）：四字段齐 + op 合法 + deadline 正数 + fromDevice
/// 非空 → 写 seen（幂等，重复到达静默 ok）→ 首次到达发 RecoveryUpdated +
/// 安全日志。事件 deadline 带**本机否决窗终点**（localArrival + window），
/// 前端倒计时据此渲染，不消费发起方时钟。
fn handle_initiated<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    envelope_ts: i64,
    body: &Value,
) -> Result<InboundDmResult> {
    let op_str = body.get("op").and_then(Value::as_str).unwrap_or_default();
    let request_id = body
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let deadline = body.get("deadline").and_then(Value::as_i64);
    let from_device = body
        .get("fromDevice")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Ok(op) = op_str.parse::<RecoveryOp>() else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(deadline) = deadline else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if request_id.is_empty() || from_device.is_empty() || deadline <= 0 {
        return done(fail_response("invalid-body"), Vec::new());
    }

    let window_ms = RecoveryService::<S>::recovery_window_ms(deadline, envelope_ts);
    let (seen, created, _) = RecoveryService::<S>::apply_inbound_initiated(
        storage,
        ctx.now_ms,
        request_id,
        op.clone(),
        from_device,
        window_ms,
    )?;
    if !created {
        return done(ok_response(), Vec::new());
    }

    append_log(
        storage,
        ctx.now_ms,
        "recovery_initiated",
        request_id,
        op.as_str(),
        from_device,
    )?;
    let event = updated_event(
        request_id,
        "initiated",
        Some(op.as_str().to_string()),
        Some(seen.local_arrival_ms.saturating_add(seen.window_ms)),
        from_device,
    );
    done(ok_response(), vec![event])
}

/// `vetoed`（fail-safe）：requestId 非空即无条件落墓碑（乱序兜底），
/// seen/pending 命中者置 veto；墓碑新建或标记首次跃迁才发事件/写日志，
/// 重复 veto 幂等静默 ok。
fn handle_vetoed<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    body: &Value,
) -> Result<InboundDmResult> {
    let request_id = body
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if request_id.is_empty() {
        return done(fail_response("invalid-body"), Vec::new());
    }
    let from_device = body
        .get("fromDevice")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let changed =
        RecoveryService::<S>::apply_inbound_vetoed(storage, ctx.now_ms, request_id, from_device)?;
    if !changed {
        return done(ok_response(), Vec::new());
    }

    let op_label = RecoveryService::<S>::get_seen(storage, request_id)?
        .map(|s| s.op.as_str().to_string())
        .or_else(|| {
            RecoveryService::<S>::get_pending(storage)
                .ok()
                .flatten()
                .filter(|p| p.request_id == request_id)
                .map(|p| p.op.as_str().to_string())
        })
        .unwrap_or_default();
    append_log(
        storage,
        ctx.now_ms,
        "recovery_vetoed",
        request_id,
        &op_label,
        from_device,
    )?;
    let event = updated_event(
        request_id,
        "vetoed",
        if op_label.is_empty() {
            None
        } else {
            Some(op_label)
        },
        None,
        from_device,
    );
    done(ok_response(), vec![event])
}

/// `committed`（fail-closed）：op/requestId/fromDevice 齐 + op 合法；
/// seen 命中置 committed（同时标 pending 终态）并发事件/写日志；未知
/// requestId 静默 ok（补投场景无意义但不报错）。
fn handle_committed<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    body: &Value,
) -> Result<InboundDmResult> {
    let op_str = body.get("op").and_then(Value::as_str).unwrap_or_default();
    let request_id = body
        .get("requestId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let from_device = body
        .get("fromDevice")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let Ok(op) = op_str.parse::<RecoveryOp>() else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if request_id.is_empty() || from_device.is_empty() {
        return done(fail_response("invalid-body"), Vec::new());
    }

    let changed = RecoveryService::<S>::get_seen(storage, request_id)?
        .is_some_and(|s| !s.committed);
    if !changed {
        return done(ok_response(), Vec::new());
    }
    RecoveryService::<S>::mark_committed(storage, request_id)?;

    append_log(
        storage,
        ctx.now_ms,
        "recovery_committed",
        request_id,
        op.as_str(),
        from_device,
    )?;
    let event = updated_event(
        request_id,
        "committed",
        Some(op.as_str().to_string()),
        None,
        from_device,
    );
    done(ok_response(), vec![event])
}
