//! kernel 的 dm 入站处理器（[`DmHandler`] 实现）：把 dm 直连接收委托给
//! `inbound_dm` 纯逻辑编排（验签/落库/事件），并把需要 host/kernel 侧资源
//! （节点句柄/签名私钥/会话口令）的回发指令装配成信封 spawn 到 runtime 投递。
//!
//! 从 `host` 拆出的子模块（文件长度约束）：[`KernelDmHandler`] 字段全部为
//! `Arc`/`SledStorage` 克隆（`Send + Sync`），由事件循环 spawn 到阻塞线程池执行
//! （验签/落库等重 IO 不占事件循环线程）；本模块只做装配与错误映射，业务规则
//! 全在 `super::super::inbound_dm`。
//!
//! 代码组织：本文件为入口——处理器结构与 `handle_dm` 编排（含 pdsync 能力
//! 探测）；入站结果的回发信封 spawn（auto-accept/profile/contact/conv/device/
//! pdsync）在 [`replies`]，自设备资料快照的 LWW 应用与身份文件回写在
//! [`profile_apply`]。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::p2p::host::DmHandler;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::P2pNode;
use crate::storage::SledStorage;

use super::super::dm_envelope;

mod profile_apply;
mod replies;

/// kernel 的 dm 入站处理器：字段全部为 `Arc`/`SledStorage` 克隆，`Send + Sync`，
/// 由事件循环 spawn 到阻塞线程池执行（验签/落库等重 IO 不占事件循环线程）。
pub(crate) struct KernelDmHandler {
    pub(crate) storage: SledStorage,
    pub(crate) current_root_id: Arc<Mutex<Option<String>>>,
    pub(crate) nickname_shared: Arc<Mutex<String>>,
    pub(crate) avatar_shared: Arc<Mutex<String>>,
    pub(crate) event_tx: tokio::sync::broadcast::Sender<crate::p2p::P2pEvent>,
    pub(crate) node_shared: Arc<Mutex<Option<Arc<P2pNode>>>>,
    pub(crate) signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    pub(crate) password_shared: Arc<Mutex<Option<String>>>,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（收尾能力探测，§7.1，按设备粒度）。
    pub(crate) pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl KernelDmHandler {
    /// 收尾（§7.1）：标记某自设备（连接层 peerId）已证明支持 pdsync。保活据此
    /// 停止向其回退发旧快照。幂等（集合内重复无影响）。
    fn kernel_pdsync_capable_mark(&self, peer_id: &str) {
        self.pdsync_capable_self_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer_id.to_string());
    }

    /// 本地写入节点 id：p2p 运行中为 peerId；否则回退持久化 p2p 身份派生的
    /// 稳定 id（见 [`super::super::doc_ops::persisted_sync_node_id`]，避免多台离线
    /// 设备共用 `local-node` 被向量比较判为同源）。
    fn sync_node_id(&self) -> String {
        self.node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| super::super::doc_ops::persisted_sync_node_id(&self.storage))
    }

    /// 收尾（§7.1）能力标记判定：仅当 pdsync-* 信封**验签通过**且
    /// `from == 本机 rootId`（验签已把 from 绑定根公钥，确为自设备）时，
    /// 返回应标记的连接层 peerId。
    ///
    /// 两处防误标：
    /// - 验签前不可按 payload kind 字符串标记——伪造信封会欺骗能力探测；
    /// - 按连接层 peerId（每设备唯一）而非 rootId 键控——同身份所有设备共
    ///   享 rootId，按 rootId 标记会让一台新设备停掉所有自设备的旧快照
    ///   回退（旧版本设备从此收不到数据，§7.1 回退被破坏）。
    pub(crate) fn pdsync_capability_mark(
        payload: &Value,
        my_root_id: &str,
        remote_peer_id: &str,
    ) -> Option<String> {
        let kind = payload.get("kind").and_then(Value::as_str)?;
        if !matches!(
            kind,
            super::super::dm_envelope::KIND_PDSYNC_HELLO
                | super::super::dm_envelope::KIND_PDSYNC_NEED
                | super::super::dm_envelope::KIND_PDSYNC_DATA
        ) {
            return None;
        }
        let verified =
            dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
        (verified.from == my_root_id).then(|| remote_peer_id.to_string())
    }
}

impl DmHandler for KernelDmHandler {
    /// dm 直连接收：委托 kernel 入站编排（验签/落库/事件），应答帧回传
    /// 发送方；产出的事件逐个 emit 到壳层广播通道。
    fn handle_dm(
        &self,
        payload: Value,
        remote_peer_id: &str,
        online_peers: &HashSet<String>,
    ) -> std::result::Result<Value, String> {
        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        log::info!(
            "[HOST] handle_dm ENTRY | kind={} remote_peer={}",
            kind,
            &remote_peer_id[..std::cmp::min(16, remote_peer_id.len())]
        );
        let root_id = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| "no active identity".to_string())?;
        // 昵称为空时回退 rootId 前 8 位（与 kernel my_nickname 的出站口径一致）
        let nickname = {
            let shared = self
                .nickname_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if shared.trim().is_empty() {
                root_id.chars().take(8).collect()
            } else {
                shared
            }
        };
        let mut storage = self.storage.clone();
        // 收尾（§7.1）能力标记：验签通过且 from==本机 rootId 的 pdsync-* 信封
        // 才证明对端（该连接层 peerId 对应的设备）支持 pdsync——保活据此停止
        // 向该设备回退发旧快照。判定内部做完整验签，先于入站合入执行不影响
        // 正确性（验签不过不标记）。
        if let Some(peer) = Self::pdsync_capability_mark(&payload, &root_id, remote_peer_id) {
            self.kernel_pdsync_capable_mark(&peer);
        }
        // 入站落库整体在 io_lock 内执行（与 Tauri 命令线程的变更互斥）
        let result = {
            let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
            let node_id = self.sync_node_id();
            super::super::inbound_dm::handle_inbound_dm(
                &mut storage,
                &root_id,
                &nickname,
                payload,
                remote_peer_id,
                online_peers,
                system_now_ms(),
                &node_id,
            )
            .map_err(|e| e.to_string())?
        };
        for event in result.events {
            // 无订阅者时忽略发送失败
            let _ = self.event_tx.send(event);
        }
        if let Some(auto_accept) = result.auto_accept {
            // 配对回发通讯录 + 会话元数据快照：新设备（QR 恢复）立即拿到
            // 联系人和会话外壳/置顶/免打扰/草稿
            self.spawn_contact_sync_reply(&root_id, auto_accept.target.clone());
            self.spawn_conv_sync_reply(&root_id, auto_accept.target.clone());
            self.spawn_auto_accept(&root_id, &nickname, auto_accept);
        }
        // 自设备 profile-sync 回发分两种：
        // 1. 配对握手（handle_self_friend_request，unconditional=true）：
        //    无条件回发——P2P 启动时的一次性广播可能早于自记录 peer 填入，
        //    配对是首次可靠的回发时机；
        // 2. LWW 裁决（handle_profile_sync，unconditional=false）：
        //    仅本机较新时回发（对端快照较旧/残缺时补齐，收敛后相等不再
        //    互发，无 ping-pong）。
        if let Some(reply) = result.profile_sync_reply {
            if reply.unconditional {
                self.spawn_profile_sync_reply(&root_id, reply.target);
            } else if let Some(self_profile) = result.self_profile {
                let local_is_newer = self.apply_self_profile(&root_id, &self_profile);
                if local_is_newer {
                    self.spawn_profile_sync_reply(&root_id, reply.target);
                }
            }
        } else if let Some(self_profile) = result.self_profile {
            // 无回发指令但有快照：仅做 LWW 应用（对端较新时更新本机身份文件）
            self.apply_self_profile(&root_id, &self_profile);
        }
        // pdsync 合入 profile:self：回写身份文件（仅解锁态）
        if result.profile_applied {
            self.apply_profile_from_sled(&root_id);
        }
        // 自设备 device-sync 握手：回发本机设备记录
        if let Some(target) = result.device_sync_reply {
            self.spawn_device_sync_reply(&root_id, target);
        }
        // pdsync 出站：把纯逻辑层构建好的 body 装配成完整信封回投连接层对端
        if !result.pdsync_out.is_empty() {
            let target = PeerNodeInfo {
                peer_id: Some(remote_peer_id.to_string()),
                addresses: Vec::new(),
            };
            self.spawn_pdsync_reply(&root_id, target, result.pdsync_out);
        }
        Ok(result.response)
    }
}
