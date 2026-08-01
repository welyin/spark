//! dm 投递编排（`Kernel` 的内部方法）：信封构造/签名、对端寻址解析
//! （会话 peer → 朋友记录 → 组织 nodeInfo 择优回退）、spawn 异步投递
//! （chat 带状态回写 + ChatStatus 事件；控制信封与设备同步为尽力而为）。
//!
//! 全部方法为 `pub(crate)`/私有，供 `message_ops`/`contact_ops` 复用；
//! 异步投递 spawn 到 kernel runtime（不捕获 `&Kernel`），避免 Tauri
//! `Mutex<Kernel>` 横跨 dm_direct 的等待。

use std::sync::Arc;

use serde_json::Value;

use super::dm_envelope::{self, KIND_CHAT, KIND_PROFILE_SYNC};
use super::{Kernel, KernelError, Result};

/// 退避重试节奏（[`Kernel::spawn_deliveries_with_retry`]）：首次失败后 +2s、+5s。
pub(crate) const DM_RETRY_DELAYS: [std::time::Duration; 2] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(5),
];
use crate::contact::ContactService;
use crate::message::{ConversationRecord, MessageRecord, MessageService};
use crate::org::OrganizationService;
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 以当前已解锁身份构造并签名 dm 信封。
    pub(crate) fn build_dm_envelope(&self, kind: &str, to: &str, body: Value) -> Result<Value> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        Ok(dm_envelope::build_envelope(
            kind,
            &unlocked.root_id(),
            to,
            system_now_ms(),
            body,
            &unlocked.identity.signing_key,
        ))
    }

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

    /// spawn chat 投递任务：完成后在任务内回写消息状态（delivered/failed）
    /// 并 emit `ChatStatus` 事件；命令侧立即返回 `sending` 态视图，前端按
    /// 事件更新。所需数据（存储克隆、event_tx、io_lock）先取好再 move。
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
        let space = space.to_string();
        let conv_id = conv_id.to_string();
        let message_id = message_id.to_string();
        self.runtime.handle().spawn(async move {
            let resp = node.dm_direct(&peer, envelope).await.ok().flatten();
            let status: &str = match resp {
                Some(resp) if resp.get("ok").and_then(Value::as_bool) == Some(true) => "delivered",
                _ => "failed",
            };
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

    /// 本机节点信息（friend-request/friend-accept 信封捎带；p2p 未启动为 None）。
    pub(crate) fn local_node_info_json(&self) -> Option<Value> {
        let info = self.p2p_status().ok().flatten()?;
        Some(serde_json::json!({
            "peerId": info.peer_id,
            "addresses": info.addresses,
        }))
    }

    /// 已配对设备的 p2p 寻址信息列表：rootId==我 且 peer 非空的 FriendRecord
    /// （同身份的其他设备；存储模型每 rootId 一条记录，当前至多一台，按
    /// 列表返回以为多设备留口）。
    pub(crate) fn self_device_peers(&self, my_root_id: &str) -> Result<Vec<PeerNodeInfo>> {
        let friends = ContactService::overview(self.require_storage()?, "personal")?.friends;
        Ok(friends
            .into_iter()
            .filter(|f| f.root_id == my_root_id)
            .filter_map(|f| f.peer)
            .map(|p| PeerNodeInfo {
                peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                addresses: p.addresses,
            })
            .collect())
    }

    /// 向所有已配对设备尽力投递 dm 信封（自消息/自回执的「同步到其他节点」
    /// 路径；单设备失败静默——离线设备恢复后的历史同步依赖后续个人空间
    /// 同步机制）。投递 spawn 到 kernel runtime，不阻塞命令线程。
    pub(crate) fn deliver_to_devices(&self, my_root_id: &str, kind: &str, body: Value) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(peers) = self.self_device_peers(my_root_id) else {
            return;
        };
        if peers.is_empty() {
            eprintln!("[deliver-to-devices] no paired devices rootId={} kind={}", my_root_id, kind);
        }
        for peer in &peers {
            if peer.addresses.is_empty() {
                eprintln!(
                    "[deliver-to-devices] device has no addresses rootId={} kind={} peerId={:?}",
                    my_root_id, kind, peer.peer_id
                );
            }
        }
        let deliveries: Vec<(PeerNodeInfo, Value)> = peers
            .into_iter()
            .filter_map(|peer| {
                self.build_dm_envelope(kind, my_root_id, body.clone())
                    .ok()
                    .map(|envelope| (peer, envelope))
            })
            .collect();
        self.spawn_deliveries(deliveries);
    }

    /// 解析会话对端的 p2p 寻址信息：会话自带 peer 与回退来源（个人空间朋友
    /// peer / 组织空间成员 nodeInfo）择优——任一候选带地址即优先返回（入站
    /// 建的会话 peer.addresses 为空，不回退会「谁先开口，对方永远回不了」）；
    /// 均无返回 `Ok(None)`。
    pub(crate) fn resolve_conv_peer(&self, space: &str, conv: &ConversationRecord) -> Result<Option<PeerNodeInfo>> {
        let to_node_info = |p: &crate::message::PeerRef| PeerNodeInfo {
            peer_id: (!p.peer_id.is_empty()).then(|| p.peer_id.clone()),
            addresses: p.addresses.clone(),
        };
        let conv_peer = conv.peer.as_ref().map(to_node_info);
        let storage = self.require_storage()?;
        let fallback = if space == "personal" {
            ContactService::get_friend(storage, &conv.peer_root_id)?
                .and_then(|f| f.peer)
                .map(|p| to_node_info(&p))
        } else if let Some(org_id) = space.strip_prefix("org:") {
            OrganizationService::get_record(storage, org_id)?
                .and_then(|r| r.find_member(&conv.peer_root_id).and_then(|m| m.node_info.clone()))
                .map(|info| PeerNodeInfo {
                    peer_id: info.peer_id,
                    addresses: info.addresses,
                })
        } else {
            None
        };
        // 择优：有地址的候选优先；都无地址时会话 peer 优先（peer_id 可用于
        // 已连接短路）
        for candidate in [&conv_peer, &fallback].into_iter().flatten() {
            if !candidate.addresses.is_empty() {
                return Ok(Some(candidate.clone()));
            }
        }
        Ok(conv_peer.or(fallback))
    }

    /// 解析对端并构造 chat 信封（发送的同步部分）；对端无地址或 p2p 未
    /// 运行返回 `Ok(None)`（调用方按 failed 处理）。
    pub(crate) fn prepare_chat_delivery(
        &self,
        space: &str,
        conv: &ConversationRecord,
        record: &MessageRecord,
    ) -> Result<Option<(PeerNodeInfo, Value)>> {
        let Some(peer) = self.resolve_conv_peer(space, conv)? else {
            return Ok(None);
        };
        if self.p2p.is_none() {
            return Ok(None);
        }
        let body = serde_json::json!({
            "spaceKey": space,
            "message": serde_json::to_value(record)?,
        });
        let envelope = self.build_dm_envelope(KIND_CHAT, &conv.peer_root_id, body)?;
        Ok(Some((peer, envelope)))
    }

    /// 尽力向会话对端投递 read/recall 控制信封（失败静默；投递 spawn 到
    /// kernel runtime，不阻塞命令线程）。
    pub(crate) fn notify_peer(&self, space: &str, conv: &ConversationRecord, kind: &str, body: Value) {
        let Ok(Some(peer)) = self.resolve_conv_peer(space, conv) else {
            return;
        };
        if self.p2p.is_none() {
            return;
        }
        let Ok(envelope) = self.build_dm_envelope(kind, &conv.peer_root_id, body) else {
            return;
        };
        self.spawn_deliveries(vec![(peer, envelope)]);
    }

    /// 资料变更后向所有有寻址信息的朋友（含同身份已配对设备）逐个尽力投递
    /// profile-sync dm（`{"nickname", "avatar"?}`；单点失败静默，不阻塞资料
    /// 更新本身；p2p 未运行/无投递目标时为空操作）。
    pub(crate) fn broadcast_profile_sync(&self, nickname: &str, avatar: Option<&str>) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let friends = ContactService::overview(storage, "personal")
            .map(|view| view.friends)
            .unwrap_or_default();
        let deliveries: Vec<(PeerNodeInfo, Value)> = friends
            .into_iter()
            .filter_map(|friend| {
                let peer = friend.peer?;
                let mut body = serde_json::json!({ "nickname": nickname });
                if let Some(avatar) = avatar {
                    body["avatar"] = Value::from(avatar);
                }
                let envelope = self
                    .build_dm_envelope(KIND_PROFILE_SYNC, &friend.root_id, body)
                    .ok()?;
                let target = PeerNodeInfo {
                    peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                    addresses: peer.addresses,
                };
                Some((target, envelope))
            })
            .collect();
        self.spawn_deliveries(deliveries);
    }

}

/// `spawn_deliveries_with_retry` 的重试判定（语义见该函数文档）。
fn delivery_needs_retry(
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
