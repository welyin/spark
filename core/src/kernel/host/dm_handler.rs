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
    /// 解锁期 BIP39 种子（O4 orgkey-deliver 入站解包 orgkey 用：组织域身份
    /// 私钥 seed 派生，见 [`crate::kernel::org_access_domain`]）。
    pub(crate) seed_shared: Arc<Mutex<Option<[u8; 64]>>>,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（收尾能力探测，§7.1，按设备粒度）。
    pub(crate) pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 已证明支持 orgsync 的成员设备 peerId 集合（O2b 能力探测，§20.8，按
    /// 设备粒度；org-share/org-pull 出站读取决定是否回退旧快照链路）。
    pub(crate) orgsync_capable_member_peers: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 插件后台运行时宿主查询句柄（O3 filtered 权限钩子在 dm 入站执行：
    /// orgq-req 的 canRead/canWrite 经此投递到插件 QuickJS 后台运行时）。
    pub(crate) plugin_host_query: crate::kernel::PluginHostQuery,
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

    /// 收尾（O2b §20.8）：标记某成员设备（连接层 peerId）已证明支持 orgsync。
    /// 出站据此对该端停用 org-share 快照/org-pull 反熵、只走 orgsync。
    /// 幂等（集合内重复无影响）。
    fn kernel_orgsync_capable_mark(&self, peer_id: &str) {
        self.orgsync_capable_member_peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer_id.to_string());
    }

    /// O2b §20.8 能力探测判定：仅当 orgsync-* 信封**验签通过**（from ∈
    /// 成员表由入站编排校验）时，返回应标记的连接层 peerId——收到对端
    /// orgsync-hello/need/data 即证明该设备支持 orgsync。
    ///
    /// 验签防误标：伪造信封（未验签通过）不得欺骗能力探测；按连接层
    /// peerId（每设备唯一）而非 rootId 键控——同 rootId 的每台设备各自
    /// 证明，避免一台新设备停掉同账号其他设备的旧快照回退。
    pub(crate) fn orgsync_capability_mark(
        payload: &Value,
        my_root_id: &str,
        remote_peer_id: &str,
    ) -> Option<String> {
        let kind = payload.get("kind").and_then(Value::as_str)?;
        if !matches!(
            kind,
            super::super::dm_envelope::KIND_ORGSYNC_HELLO
                | super::super::dm_envelope::KIND_ORGSYNC_NEED
                | super::super::dm_envelope::KIND_ORGSYNC_DATA
        ) {
            return None;
        }
        // 完整验签：`to == my_root_id` + 签名有效才证明对端是真实成员设备、
        // 支持 orgsync（防伪造信封欺骗能力探测）。成员资格由入站编排校验，
        // 此处只需确认信封合法——from 非本机（orgsync 来自其他成员）。
        let _verified =
            dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
        Some(remote_peer_id.to_string())
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
        // O2b §20.8 能力探测：收到验签通过的 orgsync-* 信封即证明对端（连接层
        // peerId 设备）支持 orgsync——出站据此对该端停用 org-share/org-pull
        // 旧链路、只走 orgsync。
        if let Some(peer) = Self::orgsync_capability_mark(&payload, &root_id, remote_peer_id) {
            self.kernel_orgsync_capable_mark(&peer);
        }
        // 入站落库整体在 io_lock 内执行（与 Tauri 命令线程的变更互斥）
        // S6 E2E（2026-08-11 架构师裁决：root 密钥直接转换）：取本机 **root**
        // 签名私钥（解锁态填入；锁定态 None，无法解密带 ephPub 的加密信封，
        // 回 internal-error）。不再用域身份派生。
        let my_signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let result = {
            let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
            let node_id = self.sync_node_id();
            let is_orgq_req =
                payload.get("kind").and_then(|v| v.as_str()) == Some(dm_envelope::KIND_ORGQ_REQ);
            if is_orgq_req {
                // O3 filtered 权限钩子接线：orgq-req 注入宿主钩子（数据账号侧
                // 经插件 QuickJS 后台运行时执行 canRead/canWrite）。插件未运行
                // 时 has_runtime=false → fail-closed 降级（只存不服务）。
                let hook = super::super::plugin_ops::QuickJsOrgqHook::new(
                    self.plugin_host_query.clone(),
                );
                super::super::inbound_dm::handle_inbound_dm_with_orgq_hooks(
                    &mut storage,
                    &root_id,
                    &nickname,
                    payload,
                    remote_peer_id,
                    online_peers,
                    system_now_ms(),
                    &node_id,
                    Some(&hook),
                )
                .map_err(|e| e.to_string())?
            } else {
                super::super::inbound_dm::handle_inbound_dm_with_e2e(
                    &mut storage,
                    &root_id,
                    &nickname,
                    payload,
                    remote_peer_id,
                    online_peers,
                    system_now_ms(),
                    &node_id,
                    my_signing_key.as_ref(),
                )
                .map_err(|e| e.to_string())?
            }
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
        // orgsync 出站：把纯逻辑层构建好的 body 装配成 orgsync-* 信封，
        // from=本机 rootId、to=对端成员 rootId（区别于 pdsync 的自设备语义）
        //
        // B1：每个 OrgsyncOut 携带目标成员 rootId（入站 handler 从信封 from
        // 透传），host 装配 to 时用它而非本机 rootId——否则 need/data 应答被
        // 对端 verify_envelope 拒收。
        if !result.orgsync_out.is_empty() {
            let target = PeerNodeInfo {
                peer_id: Some(remote_peer_id.to_string()),
                addresses: Vec::new(),
            };
            self.spawn_orgsync_reply(&root_id, target, result.orgsync_out);
        }
        // O4 orgkey-deliver 解包落库：纯逻辑层已验签/验 owner/验幂等，这里用
        // 本机组织域身份私钥（seed 派生）解 box 并写 personal 域 orgkey 表
        // （§20.6；密钥经 pdsync 自设备扩散，永不进 orgsync 组织流量）。
        if let Some(unbox) = result.orgkey_unbox {
            self.apply_orgkey_unbox(&root_id, &unbox);
        }
        // feed-blob 出站：把纯逻辑层构建好的 body 装配成 feed-blob-req/resp
        // 信封回投连接层对端（跨联系人分块传输通道）。
        if let Some(feed_blob) = result.feed_blob_out {
            let target = PeerNodeInfo {
                peer_id: Some(remote_peer_id.to_string()),
                addresses: Vec::new(),
            };
            self.spawn_feed_blob_reply(&root_id, target, feed_blob);
        }
        Ok(result.response)
    }
}

impl KernelDmHandler {
    /// O4 §20.6：orgkey-deliver 解包落库。用本机 `org-access:{orgId}` 域身份
    /// 私钥（seed 派生 X25519）+ sender（owner）组织身份公钥 X25519 解
    /// crypto_box，得 32B epoch 密钥后写 orgkey 表。seed 缺失（锁定态）或
    /// 解包失败 → 静默跳过（不落库；后续重新投递补投）。
    fn apply_orgkey_unbox(&self, my_root_id: &str, unbox: &crate::kernel::inbound_dm::OrgkeyUnbox) {
        let seed = self.seed_shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(seed) = seed else {
            log::info!("[ORGKEY] unbox skipped: no seed (locked) | col={}:{}", unbox.org_id, unbox.name);
            return;
        };
        let domain = crate::kernel::Kernel::org_access_domain(&unbox.org_id);
        let derived = crate::identity::derive_domain_identity(&seed, &domain);
        let my_x25519_priv = crate::sync::orgsync::ed_sk_to_x25519(&derived.signing_key.to_bytes());
        let col_full = format!("{}@v{}", unbox.name, unbox.version);
        let Some(epoch_key) = crate::sync::orgsync::unbox_epoch_key(
            &unbox.wrapped_key,
            &unbox.nonce24,
            &unbox.sender_x25519,
            &my_x25519_priv,
            &unbox.org_id,
            &col_full,
            &unbox.sender_root_id,
            my_root_id,
        ) else {
            log::info!(
                "[ORGKEY] unbox failed (bad key) | col={}:{} epoch={}",
                unbox.org_id,
                unbox.name,
                unbox.epoch
            );
            return;
        };
        let mut storage = self.storage.clone();
        crate::sync::orgsync::put_epoch_key(
            &mut storage,
            &unbox.org_id,
            &unbox.name,
            &unbox.version,
            unbox.epoch,
            &epoch_key,
        );
        log::info!(
            "[ORGKEY] unboxed epoch={} | col={}:{}@v{}",
            unbox.epoch,
            unbox.org_id,
            unbox.name,
            unbox.version
        );
        let _ = my_root_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use serde_json::json;
    use sha2::Digest;

    /// 构造合法签名的 orgsync 信封：from = sha256hex(pubKey)（verify_envelope
    /// 要求 from == sha256hex(pubKey)），to = my_root，ts = 当前时间（避免 stale）。
    fn orgsync_env(kind: &str, to: &str, key: &SigningKey) -> Value {
        let from = hex::encode(sha2::Sha256::digest(key.verifying_key().to_bytes()));
        dm_envelope::build_envelope(
            kind,
            &from,
            to,
            crate::p2p::node::system_now_ms(),
            json!({}),
            key,
        )
    }

    /// O2b §20.8：收到验签通过的 orgsync-hello/need/data → 标记该连接层
    /// peerId 支持 orgsync；非 orgsync 信封 / 验签失败不标记。
    #[test]
    fn orgsync_capability_mark_on_verified_orgsync_envelopes() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let from = hex::encode(sha2::Sha256::digest(key.verifying_key().to_bytes()));
        let my_root = from.clone(); // 本机 rootId（to 须 == my_root）
        let peer = "peer-device-x";
        // orgsync-hello：标记
        let e = orgsync_env(dm_envelope::KIND_ORGSYNC_HELLO, &my_root, &key);
        assert_eq!(
            KernelDmHandler::orgsync_capability_mark(&e, &my_root, peer),
            Some(peer.to_string())
        );
        // orgsync-need / orgsync-data 同样标记
        let e = orgsync_env(dm_envelope::KIND_ORGSYNC_NEED, &my_root, &key);
        assert_eq!(
            KernelDmHandler::orgsync_capability_mark(&e, &my_root, peer),
            Some(peer.to_string())
        );
        let e = orgsync_env(dm_envelope::KIND_ORGSYNC_DATA, &my_root, &key);
        assert_eq!(
            KernelDmHandler::orgsync_capability_mark(&e, &my_root, peer),
            Some(peer.to_string())
        );
        // 非 orgsync 信封不标记
        let e = orgsync_env(dm_envelope::KIND_PDSYNC_HELLO, &my_root, &key);
        assert_eq!(
            KernelDmHandler::orgsync_capability_mark(&e, &my_root, peer),
            None
        );
    }

    /// O2b §20.8：验签失败（to 非本机）不得标记——防伪造信封欺骗能力探测。
    #[test]
    fn orgsync_capability_mark_rejects_unverified() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let from = hex::encode(sha2::Sha256::digest(key.verifying_key().to_bytes()));
        let my_root = from.clone(); // 本机 rootId
        // 信封 to 指向"另一台设备"（非本机 rootId）→ 验签失败（not-for-me）→ 不标记
        let e = orgsync_env(dm_envelope::KIND_ORGSYNC_HELLO, "some-other-member", &key);
        assert_eq!(
            KernelDmHandler::orgsync_capability_mark(&e, &my_root, "peer-x"),
            None
        );
    }
}
