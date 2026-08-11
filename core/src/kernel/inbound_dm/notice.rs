//! 自设备新设备通知（`system/device-notice`）入站处理。
//!
//! 规格（M1 §3.3）：本函数只做校验 + 向上层发事件，**不允许**有任何写库或
//! 授权集变更副作用。撤销、断连、FriendRecord 迁移等逻辑全部在
//! `Kernel::revoke_device` / `handle_device_sync` / 连接层黑名单内处理。

use serde_json::{Value, json};

use super::{InboundContext, InboundDmResult, Result, fail_response, ok_response, done};
use crate::p2p::P2pEvent;

pub(crate) fn handle_device_notice(
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self"), Vec::new());
    }
    let Some(kind) = body.get("kind").and_then(Value::as_str) else {
        return done(fail_response("invalid-kind"), Vec::new());
    };
    if kind != "device_joined" {
        return done(fail_response("invalid-kind"), Vec::new());
    }
    let Some(device_id) = body.get("deviceId").and_then(Value::as_str) else {
        return done(fail_response("invalid-deviceId"), Vec::new());
    };
    if device_id.trim().is_empty() {
        return done(fail_response("invalid-deviceId"), Vec::new());
    }

    let device_name = body
        .get("deviceName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ts = body.get("ts").and_then(Value::as_i64).unwrap_or(ctx.now_ms);

    let event = P2pEvent::DeviceNoticeReceived(json!({
        "kind": "device_joined",
        "deviceId": device_id,
        "deviceName": device_name,
        "ts": ts,
    }));

    done(ok_response(), vec![event])
}
