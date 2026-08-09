//! 自设备快照广播：通讯录/会话元数据/设备记录变更后，向已配对自设备
//! （自 FriendRecord 的 peer 寻址，缺失时回退 [`Kernel::self_device_peers`]
//! 逐设备广播）尽力投递 contact-sync / conv-sync / device-sync 全量快照。
//! 合入侧按 LWW 幂等裁决，重复投递/旧快照回灌无害。

use serde_json::Value;

use super::super::Kernel;
use super::super::dm_envelope::{KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_DEVICE_SYNC};
use crate::contact::ContactService;
use crate::p2p::PeerNodeInfo;

impl Kernel {
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
        // 优先用自 FriendRecord 的 peer 单播；peer 缺失时回退 self_device_peers
        let deliveries: Vec<(PeerNodeInfo, Value)> =
            match ContactService::get_friend(storage, &root_id) {
                Ok(Some(friend)) if friend.peer.is_some() => {
                    let peer = friend.peer.unwrap();
                    let target = PeerNodeInfo {
                        peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                        addresses: peer.addresses,
                    };
                    match self.build_dm_envelope(KIND_CONTACT_SYNC, &root_id, body) {
                        Ok(envelope) => vec![(target, envelope)],
                        Err(_) => Vec::new(),
                    }
                }
                _ => {
                    let Ok(envelope) = self.build_dm_envelope(KIND_CONTACT_SYNC, &root_id, body)
                    else {
                        return;
                    };
                    self.self_device_peers(&root_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|peer| (peer, envelope.clone()))
                        .collect()
                }
            };
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
        // 优先用自 FriendRecord 的 peer 单播；peer 缺失时回退 self_device_peers
        let deliveries: Vec<(PeerNodeInfo, Value)> =
            match ContactService::get_friend(storage, &root_id) {
                Ok(Some(friend)) if friend.peer.is_some() => {
                    let peer = friend.peer.unwrap();
                    let target = PeerNodeInfo {
                        peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                        addresses: peer.addresses,
                    };
                    match self.build_dm_envelope(KIND_CONV_SYNC, &root_id, body) {
                        Ok(envelope) => vec![(target, envelope)],
                        Err(_) => Vec::new(),
                    }
                }
                _ => {
                    let Ok(envelope) = self.build_dm_envelope(KIND_CONV_SYNC, &root_id, body)
                    else {
                        return;
                    };
                    self.self_device_peers(&root_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|peer| (peer, envelope.clone()))
                        .collect()
                }
            };
        self.spawn_deliveries(deliveries);
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
