//! 资料同步广播：资料变更/启动补推时把全量资料快照投递给朋友（互推展示
//! 字段）与自设备（完整资料，隐私字段仅自设备间），并把资料镜像写入 sled
//! `profile:self` 供 pdsync 自设备同步与锁定态读源。

use serde_json::Value;

use super::super::Kernel;
use super::super::dm_envelope::KIND_PROFILE_SYNC;
use crate::contact::ContactService;
use crate::p2p::PeerNodeInfo;

impl Kernel {
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
                let peers = friend.peers;
                if peers.is_empty() {
                    return None;
                }
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
                // 多设备寻址：向该好友的每台已知设备投递
                Some(
                    peers
                        .into_iter()
                        .map(|peer| {
                            let target = PeerNodeInfo {
                                peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
                                addresses: peer.addresses,
                            };
                            (target, envelope.clone())
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
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
        let body = serde_json::json!({
            "nickname": nickname,
            "avatar": avatar,
            "gender": gender,
            "region": region,
            "signature": signature,
            "updatedAt": updated_at,
        });
        // 优先用自 FriendRecord 的 peers 多播（已配对设备最快路径）；
        // 若无 peer（历史记录不含寻址信息），回退 self_device_peers
        // 逐设备广播（含自愈 self-pointing），避免静默投递失败。
        let envelope = match self.build_dm_envelope(KIND_PROFILE_SYNC, &root_id, body) {
            Ok(e) => e,
            Err(_) => return,
        };
        let own_peers: Vec<PeerNodeInfo> = ContactService::get_friend(storage, &root_id)
            .ok()
            .flatten()
            .map(|f| {
                f.peers
                    .into_iter()
                    .map(|p| PeerNodeInfo {
                        peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                        addresses: p.addresses,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let targets = if !own_peers.is_empty() {
            own_peers
        } else {
            // peer 缺失/不存在：用 self_device_peers（含自愈）兜底
            self.self_device_peers(&root_id).unwrap_or_default()
        };
        self.spawn_deliveries(
            targets
                .into_iter()
                .map(|peer| (peer, envelope.clone()))
                .collect(),
        );
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
        let profile = super::super::identity::SyncableProfile::from_options(
            nickname, avatar, gender, region, signature,
        );
        let key = super::super::identity::PROFILE_SELF_KEY;
        let json = serde_json::to_string(&profile).unwrap_or_default();
        let Ok(storage) = self.require_storage_mut() else {
            return;
        };
        let old_ts = crate::sync::personal::get_personal_meta(storage, key)
            .ok()
            .flatten()
            .map(|m| m.ts);
        let meta = crate::sync::put_personal(storage, &node_id, key, &json, now);
        if let Ok(m) = meta {
            log::info!(
                "[PROFILE_CHAIN] sled mirror written | pmeta.ts old={:?} new={} vv={:?}",
                old_ts,
                m.ts,
                m.vv,
            );
        }
    }
}
