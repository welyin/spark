//! 异步投递任务：把装配好的 dm 信封 spawn 到 kernel runtime 逐个投递
//! （不捕获 `&Kernel`——节点句柄为 Arc 克隆，避免 Tauri `Mutex<Kernel>`
//! 横跨 dm_direct 的 10s 等待）。chat 投递带状态回写 + ChatStatus 事件；
//! 控制信封/设备同步为尽力而为；邀请类「丢失即静默卡死」的信封走退避重试。

use std::sync::Arc;

use serde_json::Value;

use super::super::Kernel;
use crate::message::MessageService;
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::storage::StorageBackend;

/// `spawn_deliveries_with_retry` 的重试判定（语义见该函数文档）。
pub(crate) fn delivery_needs_retry(
    result: &std::result::Result<Option<Value>, crate::p2p::P2pError>,
) -> bool {
    match result {
        Ok(Some(resp)) => {
            if resp.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                false
            } else {
                matches!(
                    resp.get("reason").and_then(Value::as_str),
                    Some("rate-limited")
                )
            }
        }
        Ok(None) | Err(_) => true,
    }
}

impl Kernel {
    /// spawn 顺序投递任务（尽力而为；不捕获 `&Kernel`——节点句柄为 Arc 克隆，
    /// host `spawn_auto_accept` 同模式）。用于控制信封（read/recall/设备同步），
    /// 避免 Tauri `Mutex<Kernel>` 横跨 dm_direct 的 10s 等待。
    pub(crate) fn spawn_deliveries(&self, deliveries: Vec<(PeerNodeInfo, Value)>) {
        let Some(node) = self.p2p.clone() else {
            return;
        };
        self.runtime.handle().spawn(async move {
            for (peer, envelope) in deliveries {
                // 自设备投递（自消息/自回执/资料同步）原为完全静默——失败与成功
                // 都无法区分，移动端排障需要最小可观测性（格式对齐 org-sync 日志）
                let kind = envelope
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                match node.dm_direct(&peer, envelope).await {
                    Ok(Some(_)) => {}
                    Ok(None) => eprintln!(
                        "[deliver-to-devices] failed kind={} peerId={:?} addrs={}",
                        kind,
                        peer.peer_id,
                        peer.addresses.len()
                    ),
                    Err(error) => eprintln!(
                        "[deliver-to-devices] error kind={} peerId={:?} addrs={} error={}",
                        kind,
                        peer.peer_id,
                        peer.addresses.len(),
                        error
                    ),
                }
            }
        });
    }

    /// spawn 顺序投递任务（带退避重试）：首次未送达按 `retry_delays` 逐个等待
    /// 后重试。用于组织邀请/邀请应答这类**丢失即静默卡死**且无失败 UI 的
    /// 信封——预录成员后立即发邀请时，dm 拨号与 org-share 推送直连竞争可致
    /// 首投失败（瞬态：连接建立后 `begin_dm_attempt` 有 is_connected 短路，
    /// 重试基本必成）。
    ///
    /// 重试判定：应答 `ok:true` 停止；终态拒绝（blocked/invalid-body 等
    /// 语义性 reason）重试无意义直接放弃；`Ok(None)`（投递失败）/超时/
    /// `rate-limited` 属瞬态，进入下一次退避。
    pub(crate) fn spawn_deliveries_with_retry(
        &self,
        deliveries: Vec<(PeerNodeInfo, Value)>,
        retry_delays: &'static [std::time::Duration],
    ) {
        let Some(node) = self.p2p.clone() else {
            return;
        };
        self.runtime.handle().spawn(async move {
            for (peer, envelope) in deliveries {
                let mut result = node.dm_direct(&peer, envelope.clone()).await;
                for delay in retry_delays {
                    if !delivery_needs_retry(&result) {
                        break;
                    }
                    tokio::time::sleep(*delay).await;
                    result = node.dm_direct(&peer, envelope.clone()).await;
                }
            }
        });
    }

    /// spawn chat 投递任务：完成后在任务内回写消息状态（`delivered`）并 emit
    /// `ChatStatus` 事件；命令侧立即返回 `sending` 态视图，前端按事件更新。
    ///
    /// **离线补投（social-feed §6.4）**：投递失败（不可达/超时）时把密文信封
    /// 入 `dm:pending:` 离线队列（`dm_offline`），消息状态**保持 `sending`**
    /// 不置 failed——补投由 `on_peer_connected` flush 钩子**事件驱动**重发
    /// （M6 已删 60s 周期兜底，失败留队等下次连接），成功后置 `delivered`。
    /// `message_resend` 保留为手动兜底。
    ///
    /// 回写是 compare-and-set（仅当当前状态仍为 `sending`）：重发会重新置
    /// `sending` 并 spawn 新任务，旧任务的迟到回写不得覆盖新任务已写入的
    /// 终态；CAS 未命中时不发事件（状态属于另一次投递尝试，事件由它发出）。
    pub(crate) fn spawn_chat_delivery(
        &self,
        space: &str,
        conv_id: &str,
        message_id: &str,
        peer: PeerNodeInfo,
        envelope: Value,
    ) {
        let (Some(node), Some(mut storage)) = (self.p2p.clone(), self.storage.clone()) else {
            return;
        };
        let event_tx = self.event_tx.clone();
        let io_lock = Arc::clone(&self.io_lock);
        let node_id = self.sync_node_id();
        let space = space.to_string();
        let conv_id = conv_id.to_string();
        let message_id = message_id.to_string();
        self.runtime.handle().spawn(async move {
            // 投递前诊断（对齐 [deliver-to-devices] 风格）：chat 投递原完全静默，
            // 成功/失败只回写 DB，移动端排障需要投递起点与终点可观测。
            eprintln!(
                "[chat-delivery] send messageId={} convId={} peerId={:?} addrs={}",
                message_id,
                conv_id,
                peer.peer_id,
                peer.addresses.len()
            );
            let resp = node.dm_direct(&peer, envelope.clone()).await.ok().flatten();
            let resp_ok = resp
                .as_ref()
                .and_then(|r| r.get("ok").and_then(Value::as_bool))
                .unwrap_or(false);
            eprintln!(
                "[chat-delivery] result messageId={} resp_ok={}",
                message_id, resp_ok
            );
            if !resp_ok {
                // 终态拒绝（对端在线但语义性拒绝：blocked/invalid-body 等）：
                // 重试无意义，直接置 failed + ChatStatus，不入离线队列（I3）。
                let reason = resp
                    .as_ref()
                    .and_then(|r| r.get("reason").and_then(Value::as_str));
                if super::flush::is_terminal_rejection(reason) {
                    let wrote = {
                        let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                        MessageService::set_message_status_if_sending(
                            &mut storage,
                            &space,
                            &conv_id,
                            &message_id,
                            "failed",
                        )
                        .unwrap_or(false)
                    };
                    if wrote {
                        let _ = event_tx.send(P2pEvent::ChatStatus(serde_json::json!({
                            "spaceKey": space,
                            "convId": conv_id,
                            "messageId": message_id,
                            "status": "failed",
                        })));
                    }
                    return;
                }
                // 非终态（不可达/超时/rate-limited）：密文入离线队列自动补投，
                // 状态保持 sending（不置 failed）。入队失败不阻断——消息仍留在
                // sending，由 message_resend 手动兜底。
                enqueue_pending(&mut storage, &space, &conv_id, &message_id, &envelope, &node_id);
                return;
            }
            let status = "delivered";
            let wrote = {
                let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                MessageService::set_message_status_if_sending(
                    &mut storage,
                    &space,
                    &conv_id,
                    &message_id,
                    status,
                )
                .unwrap_or(false)
            };
            if wrote {
                let _ = event_tx.send(P2pEvent::ChatStatus(serde_json::json!({
                    "spaceKey": space,
                    "convId": conv_id,
                    "messageId": message_id,
                    "status": status,
                })));
            }
        });
    }
}

/// chat 投递失败时把密文信封入 `dm:pending:` 离线队列（social-feed §6.4）。
///
/// 从信封解析 `to`（收件人 rootId）与 `kind`；`space` 为 `personal` 或
/// `org:<orgId>` 决定用个人/组织 pending 键。入队走 `dm_offline::enqueue`，
/// 个人空间经 `put_personal` 携带 pmeta（pdsync `dm:pending` category）自设备
/// 扩散。任一步失败静默跳过（消息仍 sending，靠 `message_resend` 兜底）。
fn enqueue_pending<S: StorageBackend>(
    storage: &mut S,
    space: &str,
    conv_id: &str,
    message_id: &str,
    envelope: &Value,
    node_id: &str,
) {
    use crate::dm_offline::{PendingRecord, PendingSpace, enqueue};
    let Some(to) = envelope.get("to").and_then(Value::as_str) else {
        return;
    };
    let kind = envelope
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("chat")
        .to_string();
    let pending_space = if space == "personal" {
        PendingSpace::Personal
    } else if let Some(org_id) = space.strip_prefix("org:") {
        PendingSpace::Org(org_id)
    } else {
        return; // 非法空间：不入队
    };
    let now = crate::p2p::node::system_now_ms();
    let record = PendingRecord {
        to: to.to_string(),
        message_id: message_id.to_string(),
        kind,
        space_key: space.to_string(),
        conv_id: Some(conv_id.to_string()),
        envelope: envelope.clone(),
        created_at: now,
    };
    if let Err(e) = enqueue(storage, pending_space, to, &record, node_id, now) {
        eprintln!("[chat-delivery] offline enqueue failed: {e}");
    }
}
