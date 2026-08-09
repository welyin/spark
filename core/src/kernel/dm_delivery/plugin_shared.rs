//! 投递能力的共享句柄实现：[`Kernel::deliver_to_devices`] 门面与插件后台
//! 运行时的 bot 回复路径（`bot_reply_shared`）共用。与门面各成员方法语义
//! 一致，仅句柄来源从 `&Kernel` 换成共享格（p2p 节点/签名私钥/存储镜像/
//! runtime handle）——各格与门面字段同源同生命周期（start 回填、
//! stop/lock 清空）。

use serde_json::Value;

use super::addressing::{heal_self_pointing_friend_record, self_device_peer_infos};
use super::super::{KernelError, Result};
use super::super::dm_envelope;
use crate::contact::ContactService;
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;
use crate::plugin::PluginHostShared;

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
        let local_peer_id = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string());
        // 自愈：同 Kernel::self_device_peers（自指污染记录先按设备清单改写）
        if let Some(local) = local_peer_id.as_deref() {
            heal_self_pointing_friend_record(
                &mut storage.clone(),
                my_root_id,
                local,
                local,
                system_now_ms(),
            );
        }
        let friends = ContactService::overview(&storage, "personal")?.friends;
        let peers = self_device_peer_infos(
            friends,
            my_root_id,
            local_peer_id.as_deref(),
        );
        // 回退：FriendRecord 内 peer 缺失时，直接从 DeviceService::list 取
        // 配对设备 peerId 兜底（对齐 Kernel::self_device_peers 回退语义）
        if !peers.is_empty() {
            return Ok(peers);
        }
        if let Some(local) = local_peer_id.as_deref() {
            if let Ok(devices) = crate::device::DeviceService::list(&storage) {
                return Ok(devices
                    .into_iter()
                    .filter(|r| !r.peer_id.trim().is_empty() && r.peer_id != local)
                    .map(|r| PeerNodeInfo {
                        peer_id: Some(r.peer_id),
                        addresses: Vec::new(),
                    })
                    .collect());
            }
        }
        Ok(Vec::new())
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
