//! 对端寻址解析：会话 peer → 朋友记录 → 组织 nodeInfo 的择优回退，以及
//! 自设备（同 rootId 的已配对设备）peer 列表提取——含自指污染的防御性
//! 过滤与按设备清单的自愈重写（历史 pdsync 互灌可能把自记录 peer 污染成
//! 指向本机，见 [`heal_self_pointing_friend_record`]）。

use serde_json::Value;

use super::super::{Kernel, Result};
use crate::contact::ContactService;
use crate::device::{DeviceRecord, DeviceService};
use crate::message::{ConversationRecord, PeerRef};
use crate::org::OrganizationService;
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;
use crate::storage::StorageBackend;

/// 纯函数：列出同身份（rootId == 本机）的对端设备节点信息，供
/// `Kernel::self_device_peers` 与插件共享句柄使用。
///
/// 逻辑：
/// 1. 优先取 self FriendRecord 的 `peer`（含地址）。
/// 2. FriendRecord 无 peer 时，回退到 `devices` 中配对的健康设备。
/// 3. 过滤本机 local peerId（避免 DialError::LocalPeerId）。
/// 4. 过滤已撤销设备（`DeviceRecord.revoked_at.is_some()`）。
/// 5. `local_peer_id` 为 None（p2p 未运行）时不做自指过滤。
pub(crate) fn list_self_device_peer_infos(
    friends: Vec<crate::contact::FriendRecord>,
    devices: Vec<DeviceRecord>,
    my_root_id: &str,
    local_peer_id: Option<&str>,
) -> Vec<PeerNodeInfo> {
    let revoked: std::collections::HashSet<String> = devices
        .iter()
        .filter(|d| d.revoked_at.is_some())
        .map(|d| d.peer_id.clone())
        .collect();

    let from_friend = friends
        .iter()
        .find(|f| f.root_id == my_root_id)
        .and_then(|f| f.peer.as_ref())
        .and_then(|p| {
            if p.peer_id.trim().is_empty() {
                return None;
            }
            if local_peer_id == Some(p.peer_id.as_str()) {
                return None;
            }
            if revoked.contains(&p.peer_id) {
                return None;
            }
            Some(PeerNodeInfo {
                peer_id: Some(p.peer_id.clone()),
                addresses: p.addresses.clone(),
            })
        });

    if let Some(peer) = from_friend {
        return vec![peer];
    }

    devices
        .into_iter()
        .filter_map(|d| {
            let peer_id = d.peer_id;
            if peer_id.trim().is_empty() {
                return None;
            }
            if local_peer_id == Some(peer_id.as_str()) {
                return None;
            }
            if d.revoked_at.is_some() {
                return None;
            }
            if d.device_uid.is_none() {
                return None;
            }
            Some(PeerNodeInfo {
                peer_id: Some(peer_id),
                addresses: Vec::new(),
            })
        })
        .collect()
}

/// 兼容薄包装：仅从 FriendRecord 提取（不携带 devices 回退），供既有测试/旧
/// 调用点使用。生产路径请走 [`list_self_device_peer_infos`]。
#[allow(dead_code)]
pub(crate) fn self_device_peer_infos(
    friends: Vec<crate::contact::FriendRecord>,
    my_root_id: &str,
    local_peer_id: Option<&str>,
) -> Vec<PeerNodeInfo> {
    list_self_device_peer_infos(friends, Vec::new(), my_root_id, local_peer_id)
}

/// 自记录污染自愈：自 FriendRecord 的 peer 被历史 pdsync 互灌污染指向本机
/// （`peer_id == local_peer_id`）时，从设备清单取另一台配对设备的 peerId 重写
/// 该记录（走 [`ContactService::upsert_friend_pdsync`]，保持 pdsync 口径——
/// 自愈写 bump pmeta 属正常演进；该键对称排除于折叠/增量，重写不会传播）。
///
/// 同时覆盖自 FriendRecord 的 peer 为 `None` 的场景（设备配对后 peer 尚未落库），
/// 从 `DeviceService::list` 取另一台设备填入——避免 deliver_to_devices 因 peer
/// 缺失而静默丢弃投递。
///
/// 正确 peer 值来源：`DeviceService` 的配对设备记录（`list` 按 last_seen 降序，
/// 取最近在线的非本机设备）。旧 addresses 属于被污染的值（指向本机监听地址），
/// 一并清除——已连接时 dm_direct 按 peerId 短路直发，未连接由保活候选的
/// DeviceRecord 来源兜底补拨。
///
/// 返回修复后的投递目标；记录非自指 / 无对端设备记录 / 存储失败时返回
/// `None`（调用方保持 6aca43f 的过滤现状，防御不变）。幂等：修复后记录
/// 不再自指，重复调用为空操作。
pub(crate) fn heal_self_pointing_friend_record<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    local_peer_id: &str,
    node_id: &str,
    now_ms: i64,
) -> Option<PeerNodeInfo> {
    let friend = ContactService::get_friend(storage, my_root_id).ok()??;
    if let Some(ref peer) = friend.peer {
        if peer.peer_id != local_peer_id {
            return None;
        }
    }
    let other = crate::device::DeviceService::list(storage)
        .ok()?
        .into_iter()
        .find(|r| {
            !r.peer_id.trim().is_empty()
                && r.peer_id != local_peer_id
                && r.revoked_at.is_none()
        })?;
    let mut friend = friend;
    friend.peer = Some(PeerRef {
        peer_id: other.peer_id.clone(),
        addresses: Vec::new(),
    });
    friend.updated_at = now_ms;
    ContactService::upsert_friend_pdsync(storage, &friend, now_ms, node_id).ok()?;
    eprintln!(
        "[deliver-to-devices] self-heal self-pointing friend record: rootId={} peer {} -> {}",
        my_root_id, local_peer_id, other.peer_id
    );
    Some(PeerNodeInfo {
        peer_id: Some(other.peer_id),
        addresses: Vec::new(),
    })
}

/// 将 self FriendRecord 的 peer 收敛到最新健康自设备：用于设备撤销后把
/// 指向已撤销设备的 peer 切走，无健康设备时清空 peer。返回选用的目标。
pub(crate) fn heal_self_friend_to_healthy_device<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    local_peer_id: &str,
    node_id: &str,
    now_ms: i64,
) -> Option<PeerNodeInfo> {
    let friend = ContactService::get_friend(storage, my_root_id).ok()?;
    let devices = crate::device::DeviceService::list(storage).ok()?;
    let healthy = list_self_device_peer_infos(
        friend.clone().into_iter().collect(),
        devices,
        my_root_id,
        Some(local_peer_id),
    );
    if let Some(peer) = healthy.first() {
        let current_peer_id = friend.as_ref().and_then(|f| f.peer.as_ref()).map(|p| p.peer_id.as_str());
        if current_peer_id != peer.peer_id.as_deref() {
            let mut friend = friend?;
            friend.peer = peer.peer_id.as_ref().map(|pid| PeerRef {
                peer_id: pid.clone(),
                addresses: peer.addresses.clone(),
            });
            friend.updated_at = now_ms;
            ContactService::upsert_friend_pdsync(storage, &friend, now_ms, node_id).ok()?;
            return Some(peer.clone());
        }
        return Some(peer.clone());
    }
    // 无健康设备：清空 peer
    if friend.as_ref().is_some_and(|f| f.peer.is_some()) {
        let mut friend = friend?;
        friend.peer = None;
        friend.updated_at = now_ms;
        ContactService::upsert_friend_pdsync(storage, &friend, now_ms, node_id).ok()?;
    }
    None
}

impl Kernel {
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
        let local_peer_id = self
            .p2p_status()
            .ok()
            .flatten()
            .and_then(|info| info.peer_id);
        // 自愈：自记录被历史 pdsync 互灌污染指向本机时，先按设备清单改写回
        // 对端设备再走常规提取——修复成功投递目标即恢复（自愈写 bump pmeta
        // 属正常演进，该键对称排除于折叠/增量，不传播）
        if let Some(local) = local_peer_id.as_deref()
            && let Ok(storage) = self.require_storage()
        {
            heal_self_pointing_friend_record(
                &mut storage.clone(),
                my_root_id,
                local,
                local,
                system_now_ms(),
            );
        }
        let storage = self.require_storage()?;
        let friends = ContactService::overview(storage, "personal")?.friends;
        let devices = DeviceService::list(storage).unwrap_or_default();
        Ok(list_self_device_peer_infos(
            friends,
            devices,
            my_root_id,
            local_peer_id.as_deref(),
        ))
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
            // 端点化：遍历成员端点集取首个端点作为 dm 寻址线索。
            OrganizationService::get_record(storage, org_id)?
                .and_then(|r| r.find_member(&conv.peer_root_id).and_then(|m| m.node_info.clone()))
                .and_then(|set| {
                    set.iter().next().map(|info| PeerNodeInfo {
                        peer_id: info.peer_id.clone(),
                        addresses: info.addresses.clone(),
                    })
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

    #[test]
    fn list_self_device_peer_infos_filters_revoked() {
        // self FriendRecord 指向一台已撤销设备 → 排除；另一台健康设备入选。
        let friends = vec![friend("root-self", "peer-revoked")];
        let mut revoked = device_record("peer-revoked");
        revoked.revoked_at = Some(100);
        let devices = vec![revoked, device_record("peer-healthy")];
        let peers = list_self_device_peer_infos(friends, devices, "root-self", Some("peer-local"));
        assert_eq!(peers.len(), 1, "已撤销设备应被排除，仅剩健康设备");
        assert_eq!(peers[0].peer_id.as_deref(), Some("peer-healthy"));

        // 全部 revoked → 空列表（不回退到 revoked）。
        let friends = vec![friend("root-self", "peer-revoked")];
        let mut revoked = device_record("peer-revoked");
        revoked.revoked_at = Some(100);
        let peers = list_self_device_peer_infos(friends, vec![revoked], "root-self", Some("peer-local"));
        assert!(peers.is_empty(), "全部已撤销则无可投递目标");
    }

    #[test]
    fn list_self_device_peer_infos_excludes_local_peer() {
        // 本机（local_peer_id）不出现在投递目标里（M1 sender 排除本机）。
        let friends = vec![
            friend("root-self", "peer-local"),
            friend("root-self", "peer-other"),
        ];
        let devices = vec![device_record("peer-local"), device_record("peer-other")];
        let peers = list_self_device_peer_infos(friends, devices, "root-self", Some("peer-local"));
        assert_eq!(peers.len(), 1, "本机 peer 应从投递目标排除");
        assert_eq!(peers[0].peer_id.as_deref(), Some("peer-other"));
    }

    #[test]
    fn self_device_peer_infos_filters_self_pointing_record() {
        let friends = vec![
            friend("root-self", "peer-local"),
            friend("root-other", "peer-other"),
        ];
        let peers = self_device_peer_infos(friends.clone(), "root-self", Some("peer-local"));
        assert!(peers.is_empty(), "自指记录不得作为投递目标");
        let friends = vec![friend("root-self", "peer-device-b")];
        let peers = self_device_peer_infos(friends, "root-self", Some("peer-local"));
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id.as_deref(), Some("peer-device-b"));
        let friends = vec![friend("root-self", "peer-local")];
        let peers = self_device_peer_infos(friends, "root-self", None);
        assert_eq!(peers.len(), 1, "本机 peerId 未知时不误杀");
    }

    fn device_record(peer_id: &str) -> crate::device::DeviceRecord {
        crate::device::DeviceRecord {
            peer_id: peer_id.to_string(),
            device_uid: Some(format!("uid-{peer_id}")),
            device_name: "对端设备".to_string(),
            os: "Android".to_string(),
            os_version: "14".to_string(),
            arch: "aarch64".to_string(),
            macs: Vec::new(),
            app_version: String::new(),
            updated_at: 100,
            last_seen_at: 100,
            revoked_at: None,
        }
    }

    #[test]
    fn heal_self_pointing_record_rewrites_to_other_device() {
        let mut storage = crate::storage::MemoryStorage::new();
        let mut f = friend("root-self", "peer-local");
        f.peer.as_mut().unwrap().addresses = vec!["/ip4/1.2.3.4/tcp/1".to_string()];
        ContactService::upsert_friend_pdsync(&mut storage, &f, 100, "peer-local").unwrap();
        crate::device::DeviceService::upsert_pdsync(
            &mut storage,
            &device_record("peer-local"),
            100,
            "peer-local",
        )
        .unwrap();
        crate::device::DeviceService::upsert_pdsync(
            &mut storage,
            &device_record("peer-device-b"),
            100,
            "peer-local",
        )
        .unwrap();

        let healed = heal_self_pointing_friend_record(
            &mut storage,
            "root-self",
            "peer-local",
            "peer-local",
            200,
        )
        .expect("有对端设备记录应自愈成功");
        assert_eq!(healed.peer_id.as_deref(), Some("peer-device-b"));
        let stored = ContactService::get_friend(&storage, "root-self").unwrap().unwrap();
        let peer = stored.peer.clone().unwrap();
        assert_eq!(peer.peer_id, "peer-device-b");
        assert!(peer.addresses.is_empty());
        assert_eq!(stored.updated_at, 200);
        let peers = self_device_peer_infos(vec![stored], "root-self", Some("peer-local"));
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id.as_deref(), Some("peer-device-b"));
        assert!(
            heal_self_pointing_friend_record(
                &mut storage,
                "root-self",
                "peer-local",
                "peer-local",
                300
            )
            .is_none()
        );
    }

    #[test]
    fn heal_self_pointing_record_bails_without_other_device() {
        let mut storage = crate::storage::MemoryStorage::new();
        let f = friend("root-self", "peer-local");
        ContactService::upsert_friend_pdsync(&mut storage, &f, 100, "peer-local").unwrap();
        assert!(
            heal_self_pointing_friend_record(&mut storage, "root-self", "peer-local", "peer-local", 200)
                .is_none()
        );
        let stored = ContactService::get_friend(&storage, "root-self").unwrap().unwrap();
        assert_eq!(stored.peer.unwrap().peer_id, "peer-local", "无对端记录不得改写");
        crate::device::DeviceService::upsert_pdsync(
            &mut storage,
            &device_record("peer-local"),
            100,
            "peer-local",
        )
        .unwrap();
        assert!(
            heal_self_pointing_friend_record(&mut storage, "root-self", "peer-local", "peer-local", 200)
                .is_none()
        );
        let f = friend("root-self", "peer-device-b");
        ContactService::upsert_friend_pdsync(&mut storage, &f, 100, "peer-local").unwrap();
        assert!(
            heal_self_pointing_friend_record(&mut storage, "root-self", "peer-local", "peer-local", 200)
                .is_none()
        );
        let stored = ContactService::get_friend(&storage, "root-self").unwrap().unwrap();
        assert_eq!(stored.peer.unwrap().peer_id, "peer-device-b");
    }
}
