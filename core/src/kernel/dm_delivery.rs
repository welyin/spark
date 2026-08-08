//! dm 投递编排（`Kernel` 的内部方法）：信封构造/签名、对端寻址解析
//! （会话 peer → 朋友记录 → 组织 nodeInfo 择优回退）、spawn 异步投递
//! （chat 带状态回写 + ChatStatus 事件；控制信封与设备同步为尽力而为）。
//!
//! 全部方法为 `pub(crate)`/私有，供 `message_ops`/`contact_ops` 复用；
//! 异步投递 spawn 到 kernel runtime（不捕获 `&Kernel`），避免 Tauri
//! `Mutex<Kernel>` 横跨 dm_direct 的等待。

use std::sync::Arc;

use serde_json::Value;

use super::dm_envelope::{
    self, KIND_CHAT, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_DEVICE_SYNC, KIND_PROFILE_SYNC,
};
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
use crate::plugin::PluginHostShared;

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
        let local_peer_id = self
            .p2p_status()
            .ok()
            .flatten()
            .and_then(|info| info.peer_id);
        Ok(self_device_peer_infos(
            friends,
            my_root_id,
            local_peer_id.as_deref(),
        ))
    }

    /// 向所有已配对设备尽力投递 dm 信封（自消息/自回执的「同步到其他节点」
    /// 路径；单设备失败静默——离线设备恢复后的历史同步依赖后续个人空间
    /// 同步机制）。投递 spawn 到 kernel runtime，不阻塞命令线程。
    pub(crate) fn deliver_to_devices(&self, my_root_id: &str, kind: &str, body: Value) {
        self.plugin_host.deliver_to_devices(my_root_id, kind, body);
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
    /// profile-sync dm（单点失败静默，不阻塞资料更新本身；p2p 未运行/无投递
    /// 目标时为空操作）。
    ///
    /// body 为**全量资料快照**：`nickname` 恒在；`avatar`/`gender`/`region`/
    /// `signature` 以显式 null 表达「当前未设置」（清除语义随同步传播，修复
    /// 旧版只在 Some 时携带导致「清除头像不同步」的缺口）；`updatedAt` 为身份
    /// 文件资料更新时间，自设备接收侧据此做新覆盖旧裁决（防离线设备上线后
    /// 以旧资料回灌）。
    /// 朋友资料互推：向所有普通朋友（排除自记录）广播 profile-sync。
    /// 普通好友收到后更新朋友记录（昵称/头像等展示字段）。
    pub(crate) fn broadcast_profile_to_friends(
        &self,
        nickname: &str,
        avatar: Option<&str>,
        gender: Option<&str>,
        region: Option<&str>,
        signature: Option<&str>,
        updated_at: u64,
    ) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Ok(Some(my_root_id)) = self.current_root_id() else {
            return;
        };
        let friends = ContactService::overview(storage, "personal")
            .map(|view| view.friends)
            .unwrap_or_default();
        let deliveries: Vec<(PeerNodeInfo, Value)> = friends
            .into_iter()
            .filter(|friend| friend.root_id != my_root_id) // 排除自记录
            .filter_map(|friend| {
                let peer = friend.peer?;
                let body = serde_json::json!({
                    "nickname": nickname,
                    "avatar": avatar,
                    "gender": gender,
                    "region": region,
                    "signature": signature,
                    "updatedAt": updated_at,
                });
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

    /// 自设备资料同步：只向配对设备（自 FriendRecord.peer）发 profile-sync。
    /// 完整资料（含性别/地区/签名）属隐私数据，仅自设备间同步，绝不发给朋友。
    pub(crate) fn broadcast_profile_to_self_device(
        &self,
        nickname: &str,
        avatar: Option<&str>,
        gender: Option<&str>,
        region: Option<&str>,
        signature: Option<&str>,
        updated_at: u64,
    ) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(Some(root_id)) = self.current_root_id() else {
            return;
        };
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Ok(Some(friend)) = ContactService::get_friend(storage, &root_id) else {
            return;
        };
        let Some(peer) = friend.peer else {
            return;
        };
        let body = serde_json::json!({
            "nickname": nickname,
            "avatar": avatar,
            "gender": gender,
            "region": region,
            "signature": signature,
            "updatedAt": updated_at,
        });
        let Ok(envelope) = self.build_dm_envelope(KIND_PROFILE_SYNC, &root_id, body) else {
            return;
        };
        let target = PeerNodeInfo {
            peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
            addresses: peer.addresses,
        };
        self.spawn_deliveries(vec![(target, envelope)]);
    }

    /// 把资料镜像写入 sled `profile:self`（pdsync P2）。
    ///
    /// 身份文件仍是权威存储；这里把资料另存为 sled 明文记录，bump pmeta，
    /// 使个人资料可经 pdsync 自设备同步，也作锁定态读源。写失败静默（资料
    /// 更新成功不因镜像失败回滚）。node_id 取本机同步节点。
    pub(crate) fn sync_profile_to_sled(
        &mut self,
        nickname: Option<&str>,
        avatar: Option<&str>,
        gender: Option<&str>,
        region: Option<&str>,
        signature: Option<&str>,
    ) {
        let Ok(Some(_root_id)) = self.current_root_id() else {
            return;
        };
        let now = crate::p2p::node::system_now_ms();
        let node_id = self.sync_node_id();
        let profile = super::identity::SyncableProfile::from_options(
            nickname,
            avatar,
            gender,
            region,
            signature,
        );
        let key = super::identity::PROFILE_SELF_KEY;
        let json = serde_json::to_string(&profile).unwrap_or_default();
        let Ok(storage) = self.require_storage_mut() else {
            return;
        };
        let _ = crate::sync::put_personal(storage, &node_id, key, &json, now);
    }

    /// 通讯录快照广播：本机联系人数据变更后向自设备（自 FriendRecord 的
    /// peer 寻址）投递 contact-sync 全量快照。合入侧按 LWW 幂等裁决，重复
    /// 投递/旧快照回灌无害。p2p 未启动/未配对/未解锁时为空操作。
    pub(crate) fn broadcast_contact_sync(&self) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(Some(root_id)) = self.current_root_id() else {
            return;
        };
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Ok(body) = crate::contact::build_contact_sync_snapshot(storage, &root_id) else {
            return;
        };
        let Ok(envelope) = self.build_dm_envelope(KIND_CONTACT_SYNC, &root_id, body) else {
            return;
        };
        // 仅投自设备：rootId==自己 且带 peer 寻址的朋友记录（配对设备）
        let deliveries: Vec<(PeerNodeInfo, Value)> = ContactService::get_friend(storage, &root_id)
            .ok()
            .flatten()
            .and_then(|friend| friend.peer)
            .map(|peer| {
                let target = PeerNodeInfo {
                    peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                    addresses: peer.addresses,
                };
                vec![(target, envelope)]
            })
            .unwrap_or_default();
        self.spawn_deliveries(deliveries);
    }

    /// 会话元数据快照广播：置顶/免打扰/草稿变更后向自设备投递 conv-sync
    /// 快照（LWW 幂等；p2p 未启动/未配对时为空操作）。
    pub(crate) fn broadcast_conv_sync(&self) {
        if self.p2p.is_none() {
            return;
        }
        let Ok(Some(root_id)) = self.current_root_id() else {
            return;
        };
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Ok(body) = crate::message::build_conv_sync_snapshot(storage) else {
            return;
        };
        let Ok(envelope) = self.build_dm_envelope(KIND_CONV_SYNC, &root_id, body) else {
            return;
        };
        let deliveries: Vec<(PeerNodeInfo, Value)> = ContactService::get_friend(storage, &root_id)
            .ok()
            .flatten()
            .and_then(|friend| friend.peer)
            .map(|peer| {
                let target = PeerNodeInfo {
                    peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                    addresses: peer.addresses,
                };
                vec![(target, envelope)]
            })
            .unwrap_or_default();
        self.spawn_deliveries(deliveries);
    }

    /// 启动补推：读身份文件的全量资料快照，同时向朋友和自设备广播
    /// （p2p start 后调用一次）。朋友收到更新展示字段；自设备收到做 LWW
    /// 裁决补齐离线期间错过的资料变更。身份文件缺失/未解锁时为空操作。
    pub(crate) fn broadcast_self_profile_snapshot(&self) {
        let root_id = match self.current_root_id() {
            Ok(Some(id)) => id,
            _ => return,
        };
        let Ok(Some(file)) = self.read_identity_file(&root_id) else {
            return;
        };
        let nickname = self.my_nickname(&root_id);
        // 朋友互推（不含自记录）
        self.broadcast_profile_to_friends(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
        // 自设备同步（完整资料，隐私字段仅自设备间）
        self.broadcast_profile_to_self_device(
            &nickname,
            file.avatar.as_deref(),
            file.gender.as_deref(),
            file.region.as_deref(),
            file.signature.as_deref(),
            file.updated_at,
        );
    }

    /// 向全部已配对自设备尽力投递本机设备记录（device-sync；body 为完整
    /// [`crate::device::DeviceRecord`] 线形）。投递时机：p2p 启动后补推、
    /// 收到对端 device-sync 时回发（握手式交换）。无配对设备/未解锁/未启动
    /// p2p 时为空操作。
    pub(crate) fn broadcast_device_sync(&self, record: &crate::device::DeviceRecord) {
        if self.p2p.is_none() {
            return;
        }
        let root_id = match self.current_root_id() {
            Ok(Some(id)) => id,
            _ => return,
        };
        let Ok(peers) = self.self_device_peers(&root_id) else {
            return;
        };
        let Ok(body) = serde_json::to_value(record) else {
            return;
        };
        let deliveries: Vec<(PeerNodeInfo, Value)> = peers
            .into_iter()
            .filter_map(|peer| {
                self.build_dm_envelope(KIND_DEVICE_SYNC, &root_id, body.clone())
                    .ok()
                    .map(|envelope| (peer, envelope))
            })
            .collect();
        self.spawn_deliveries(deliveries);
    }

}

/// 投递能力的共享句柄实现：`Kernel::deliver_to_devices` 门面与插件后台运行
/// 时的 bot 回复路径（`bot_reply_shared`）共用。与门面各成员方法语义一致，
/// 仅句柄来源从 `&Kernel` 换成共享格（p2p 节点/签名私钥/存储镜像/runtime
/// handle）——各格与门面字段同源同生命周期（start 回填、stop/lock 清空）。
impl PluginHostShared {
    /// 见 [`Kernel::deliver_to_devices`]（语义一致）。
    pub(crate) fn deliver_to_devices(&self, my_root_id: &str, kind: &str, body: Value) {
        if self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
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

    /// 见 [`Kernel::spawn_deliveries`]（语义一致）。
    pub(crate) fn spawn_deliveries(&self, deliveries: Vec<(PeerNodeInfo, Value)>) {
        let Some(node) = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        self.runtime.spawn(async move {
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

    /// 见 [`Kernel::self_device_peers`]（存储来源换为宿主镜像格）。
    fn self_device_peers(&self, my_root_id: &str) -> Result<Vec<PeerNodeInfo>> {
        let storage = self.require_storage()?;
        let friends = ContactService::overview(&storage, "personal")?.friends;
        let local_peer_id = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string());
        Ok(self_device_peer_infos(
            friends,
            my_root_id,
            local_peer_id.as_deref(),
        ))
    }

    /// 见 [`Kernel::build_dm_envelope`]（签名私钥来源换为解锁期共享格；
    /// 自设备投递场景 from==to==my_root_id）。
    fn build_dm_envelope(&self, kind: &str, my_root_id: &str, body: Value) -> Result<Value> {
        let signing_key = self
            .signing_key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(KernelError::Locked)?;
        Ok(dm_envelope::build_envelope(
            kind,
            my_root_id,
            my_root_id,
            system_now_ms(),
            body,
            &signing_key,
        ))
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

/// 自设备 peer 列表提取：rootId==我 且 peer 非空的 FriendRecord（配对设备）。
///
/// 防御性过滤 `peer_id == 本机 peerId` 的自指记录——自 FriendRecord 的 peer
/// 是设备相对值，历史 pdsync 互灌可能把它污染成指向本机；不自指过滤会让
/// 自消息投递拨自己（DialError::LocalPeerId）。`local_peer_id` 为 None
/// （p2p 未运行）时无从判定，不过滤。
fn self_device_peer_infos(
    friends: Vec<crate::contact::FriendRecord>,
    my_root_id: &str,
    local_peer_id: Option<&str>,
) -> Vec<PeerNodeInfo> {
    friends
        .into_iter()
        .filter(|f| f.root_id == my_root_id)
        .filter_map(|f| f.peer)
        .filter(|p| local_peer_id != Some(p.peer_id.as_str()))
        .map(|p| PeerNodeInfo {
            peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
            addresses: p.addresses,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn friend(root_id: &str, peer_id: &str) -> crate::contact::FriendRecord {
        crate::contact::FriendRecord {
            root_id: root_id.to_string(),
            nickname: String::new(),
            avatar: None,
            signature: String::new(),
            gender: None,
            added_at: 0,
            peer: Some(crate::message::PeerRef {
                peer_id: peer_id.to_string(),
                addresses: Vec::new(),
            }),
            remark: String::new(),
            phones: Vec::new(),
            tag_ids: Vec::new(),
            group_id: String::new(),
            memo: String::new(),
            photos: Vec::new(),
            permission: "open".to_string(),
            blocked: false,
            updated_at: 0,
        }
    }

    /// 自指过滤：自记录 peer 被污染指向本机时不作为自设备投递目标；
    /// 正常指向对端设备的记录保留；他人 rootId 本就不入列。
    #[test]
    fn self_device_peer_infos_filters_self_pointing_record() {
        let friends = vec![
            friend("root-self", "peer-local"),
            friend("root-other", "peer-other"),
        ];
        // 本机 peerId = peer-local：自指记录被过滤
        let peers = self_device_peer_infos(friends.clone(), "root-self", Some("peer-local"));
        assert!(peers.is_empty(), "自指记录不得作为投递目标");
        // 记录指向对端设备：保留
        let friends = vec![friend("root-self", "peer-device-b")];
        let peers = self_device_peer_infos(friends, "root-self", Some("peer-local"));
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id.as_deref(), Some("peer-device-b"));
        // p2p 未运行（本机 peerId 未知）：无从判定，不过滤
        let friends = vec![friend("root-self", "peer-local")];
        let peers = self_device_peer_infos(friends, "root-self", None);
        assert_eq!(peers.len(), 1, "本机 peerId 未知时不误杀");
    }
}
