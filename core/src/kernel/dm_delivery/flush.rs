//! 离线补投（social-feed §6.3）：把 `dm:pending:` 队列里的密文信封重发出去。
//!
//! 编排层职责：读队列 → 拨号重发 → 按应答出队/留队 → chat 回写消息终态。
//! 存储语义（键构造/容量/TTL）在 `dm_offline` 纯逻辑层，本模块只做编排。
//!
//! 补投触发：**PeerAppReady 钩子**（`host` `on_peer_app_ready`）——应用层就绪
//! （版本探测成功）时按 peerId 反查 rootId（朋友表；L4 增补组织成员表端点），
//! flush 该 recipient 的 pending 队列（`flush_pending_for_recipient`）。
//! 原 60s 周期兜底（`flush_pending_personal_full`）已随 connection-policy M6
//! 删 keepalive tick 内主动外联一并删除——补投不再后台周期兜底，纯事件驱动
//! （连接事件触发；失败留队等下次连接）。
//!
//! 应答语义（对齐 [`spawn`](super::spawn) `delivery_needs_retry` 的终态判定）：
//!
//! - `ok:true` → 补投成功：出队 + chat 回写 `delivered` + ChatStatus；
//! - `ok:false` 且 reason 属终态拒绝（`blocked` / `invalid-body` /
//!   `unknown-kind`）→ 出队不再重投，chat 回写 `failed` + ChatStatus；
//! - 其余（`Ok(None)` / 超时 / `rate-limited`）→ 瞬态，留队等下轮。

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::dm_offline::{PendingRecord, PendingSpace, list_for_recipient, remove};
use crate::message::MessageService;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode, PeerNodeInfo};
use crate::storage::StorageBackend;

/// 补投应答的终态拒绝 reason：命中直接出队不再重试（语义性拒绝，重试无意义）。
///
/// 集合（M-A）：chat 通用（`blocked`/`invalid-body`/`unknown-kind`）+
/// orgsync 语义拒绝（`rejected`/`invalid-collection`/`key-out-of-collection`/
/// `acl-scope-mismatch`）+ pdsync 语义拒绝（`not-self-device`/
/// `category-mismatch`）——缺这些会把对端明确拒收的集合数据留队重投到 TTL。
///
/// `pub(crate)`：供 `spawn`（chat 首投）与 `feed_ops`（feed 首投）复用——对端
/// 在线但回终态拒绝时，首投也应直接置 failed / 丢弃，而非入离线队列等无意义
/// 重试（I3）。
pub(crate) fn is_terminal_rejection(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some(
            "blocked"
                | "invalid-body"
                | "unknown-kind"
                | "rejected"
                | "invalid-collection"
                | "key-out-of-collection"
                | "acl-scope-mismatch"
                | "not-self-device"
                | "category-mismatch"
        )
    )
}

/// 单条补投成功/终态拒绝后的 chat 回写（compare-and-set，对齐 spawn.rs 口径）：
/// 仅 chat 通道（`conv_id` 非空）回写消息状态并 emit `ChatStatus` 事件。
/// CAS 未命中（状态已被新投递尝试改走）时不发事件——状态属于另一次尝试。
fn write_back<S: StorageBackend>(
    storage: &mut S,
    record: &PendingRecord,
    status: &str,
    event_tx: &broadcast::Sender<P2pEvent>,
    io_lock: &Arc<Mutex<()>>,
) {
    let Some(conv_id) = &record.conv_id else {
        return; // 非 chat（feed/friend-request 等）无消息状态可回写
    };
    let wrote = {
        let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::set_message_status_if_sending(
            storage,
            &record.space_key,
            conv_id,
            &record.message_id,
            status,
        )
        .unwrap_or(false)
    };
    if wrote {
        let _ = event_tx.send(P2pEvent::ChatStatus(serde_json::json!({
            "spaceKey": record.space_key,
            "convId": conv_id,
            "messageId": record.message_id,
            "status": status,
        })));
    }
}

/// 补投单个 recipient 的 pending 队列（`peer` 为该 recipient 当前可寻址的
/// 端点）。逐条重发，按应答出队/留队。供 `on_peer_app_ready` 钩子用。
///
/// `space`：Personal（chat/feed/pdsync，`on_peer_app_ready` 朋友路径）或
/// Org(orgId)（orgsync 集合数据暂存，mobile-leaf-mode §6 L4——按 peerId
/// 反查组织成员表端点触发，成员不必是联系人）。
pub(crate) async fn flush_pending_for_recipient<S: StorageBackend>(
    storage: &mut S,
    node: Arc<P2pNode>,
    event_tx: broadcast::Sender<P2pEvent>,
    io_lock: Arc<Mutex<()>>,
    space: PendingSpace<'_>,
    to_root_id: &str,
    peer: PeerNodeInfo,
) {
    let now = system_now_ms();
    let records = match list_for_recipient(storage, space, to_root_id, now) {
        Ok(records) => records,
        Err(e) => {
            eprintln!("[dm-flush] list_for_recipient failed to={to_root_id}: {e}");
            return;
        }
    };
    for (key, record) in records {
        let resp = node
            .dm_direct(&peer, record.envelope.clone())
            .await
            .ok()
            .flatten();
        let ok = resp
            .as_ref()
            .and_then(|r| r.get("ok").and_then(Value::as_bool))
            .unwrap_or(false);
        let reason = resp
            .as_ref()
            .and_then(|r| r.get("reason").and_then(Value::as_str));
        if ok {
            let _ = remove(storage, &key);
            write_back(storage, &record, "delivered", &event_tx, &io_lock);
        } else if is_terminal_rejection(reason) {
            let _ = remove(storage, &key);
            write_back(storage, &record, "failed", &event_tx, &io_lock);
        }
        // 瞬态（不可达/超时/rate-limited）：留队等下轮
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_rejection_classifies_semantic_reasons() {
        // 终态拒绝：出队不再重投（chat 通用 + orgsync/pdsync 语义拒绝，M-A）
        for reason in [
            "blocked",
            "invalid-body",
            "unknown-kind",
            "rejected",
            "invalid-collection",
            "key-out-of-collection",
            "acl-scope-mismatch",
            "not-self-device",
            "category-mismatch",
        ] {
            assert!(is_terminal_rejection(Some(reason)), "{reason} 应终态拒绝");
        }
        // 成功（非拒绝）
        assert!(!is_terminal_rejection(None), "无 reason（成功）非拒绝");
        // 瞬态：留队下轮
        assert!(!is_terminal_rejection(Some("rate-limited")));
        assert!(!is_terminal_rejection(Some("not-connected")));
    }
}
