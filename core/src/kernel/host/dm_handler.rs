//! kernel 的 dm 入站处理器（[`DmHandler`] 实现）：把 dm 直连接收委托给
//! `inbound_dm` 纯逻辑编排（验签/落库/事件），并把需要 host/kernel 侧资源
//! （节点句柄/签名私钥/会话口令）的回发指令装配成信封 spawn 到 runtime 投递。
//!
//! 从 `host` 拆出的子模块（文件长度约束）：[`KernelDmHandler`] 字段全部为
//! `Arc`/`Backend` 克隆（`Send + Sync`），由事件循环 spawn 到阻塞线程池执行
//! （验签/落库等重 IO 不占事件循环线程）；本模块只做装配与错误映射，业务规则
//! 全在 `super::super::inbound_dm`。
//!
//! 代码组织：本文件为入口——处理器结构与 `handle_dm` 编排（含 pdsync 能力
//! 探测）；入站结果的回发信封 spawn（auto-accept/profile/contact/conv/device/
//! pdsync）在 [`replies`]，自设备资料快照的 LWW 应用与身份文件回写在
//! [`profile_apply`]。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::Value;

use crate::p2p::P2pNode;
use crate::p2p::host::DmHandler;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::Backend;

use super::super::dm_envelope;

mod profile_apply;
mod replies;

/// 从 `password_shared` 与当前 `pwv:self` 派生 `Kverify`（32B）——**按 salt
/// 缓存**。
///
/// - 未解锁（password 缺失）或无 V → `None`。
/// - Kverify 仅在内存中，不落入存储。
///
/// 缓存必要性：每个 dm 入站信封都会取 Kverify（批尾 ack 锚定 / epoch 门控
/// 授予），而 `derive_kverify` 是 scrypt——真机实测逐信封重跑是**连接态
/// CPU 100% 的直接根因**（~1 信封/4s × 单次数百 ms ≈ 满核）。口令变更必
/// 伴随 pwv salt 轮换，键控 salt 天然含失效语义；password_shared 本就常驻
/// 内存，缓存不引入新的暴露面。
fn derive_kverify_from_password_shared(
    storage: &Backend,
    password_shared: &Arc<Mutex<Option<String>>>,
    cache: &Arc<Mutex<Option<(String, [u8; 32])>>>,
) -> Option<[u8; 32]> {
    let password = password_shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    let pwv = crate::pw::get_pwv(storage).ok().flatten()?;
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_salt, k)) = cache.as_ref() {
        if *cached_salt == pwv.salt {
            return Some(*k);
        }
    }
    let salt_bytes = base64::engine::general_purpose::STANDARD
        .decode(&pwv.salt)
        .ok()?;
    let salt: [u8; 16] = salt_bytes.try_into().ok()?;
    let k = crate::pw::derive_kverify(&password, &salt).ok()?;
    *cache = Some((pwv.salt.clone(), k));
    Some(k)
}

/// kernel 的 dm 入站处理器：字段全部为 `Arc`/`Backend` 克隆，`Send + Sync`，
/// 由事件循环 spawn 到阻塞线程池执行（验签/落库等重 IO 不占事件循环线程）。
pub(crate) struct KernelDmHandler {
    pub(crate) storage: Backend,
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
    /// 插件后台运行时宿主查询句柄（O3 filtered 权限钩子在 dm 入站执行：
    /// orgq-req 的 canRead/canWrite 经此投递到插件 QuickJS 后台运行时）。
    pub(crate) plugin_host_query: crate::kernel::PluginHostQuery,
    /// Kverify 派生缓存（按 pwv salt 键控；scrypt 单次数百 ms，逐信封重跑
    /// 是连接态 CPU 100% 的实测根因——见 `derive_kverify_from_password_shared`）。
    pub(crate) kverify_cache: Arc<Mutex<Option<(String, [u8; 32])>>>,
}

impl KernelDmHandler {
    /// 每子批独立持有 io_lock 处理一段记录（UI 命令可在子批间获得锁）。
    /// 初始同步突发（单信封上千条记录）若整批持锁，UI 全部冻结到批处理
    /// 结束——拆批是治本的让步（真机 Android 实测场景；§X 不变量不受影响，
    /// 仍全程裸存储）。阈值与子批大小取 40。
    const INBOUND_CHUNK_SIZE: usize = 40;

    /// 在独立 io_lock 持期内处理一个（子）信封：验签 + 合入 + 事件聚合。
    fn process_one_inbound(
        &self,
        storage: &mut Backend,
        root_id: &str,
        nickname: &str,
        payload: Value,
        remote_peer_id: &str,
        online_peers: &HashSet<String>,
        node_id: &str,
        kverify: Option<&[u8; 32]>,
        my_signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> std::result::Result<crate::kernel::inbound_dm::InboundDmResult, String> {
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let is_orgq_req =
            payload.get("kind").and_then(|v| v.as_str()) == Some(dm_envelope::KIND_ORGQ_REQ);
        if is_orgq_req {
            // O3 filtered 权限钩子接线：orgq-req 注入宿主钩子（数据账号侧
            // 经插件 QuickJS 后台运行时执行 canRead/canWrite）。插件未运行
            // 时 has_runtime=false → fail-closed 降级（只存不服务）。
            let hook =
                super::super::plugin_ops::QuickJsOrgqHook::new(self.plugin_host_query.clone());
            super::super::inbound_dm::handle_inbound_dm_with_orgq_hooks(
                storage,
                root_id,
                nickname,
                payload,
                remote_peer_id,
                online_peers,
                system_now_ms(),
                node_id,
                kverify,
                Some(&hook),
            )
            .map_err(|e| e.to_string())
        } else {
            super::super::inbound_dm::handle_inbound_dm_with_e2e(
                storage,
                root_id,
                nickname,
                payload,
                remote_peer_id,
                online_peers,
                system_now_ms(),
                node_id,
                kverify,
                my_signing_key,
            )
            .map_err(|e| e.to_string())
        }
    }

    /// 大宗 pdsync-data 拆子批处理：每子批独立持 io_lock，UI 可插队。
    /// 非 pdsync-data 或记录量低于阈值 → 直接单次处理（原路径）。
    fn maybe_chunked_process(
        &self,
        storage: &mut Backend,
        root_id: &str,
        nickname: &str,
        payload: Value,
        remote_peer_id: &str,
        online_peers: &HashSet<String>,
        node_id: &str,
        kverify: Option<&[u8; 32]>,
        my_signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> std::result::Result<crate::kernel::inbound_dm::InboundDmResult, String> {
        let is_pdsync_data =
            payload.get("kind").and_then(|v| v.as_str()) == Some(dm_envelope::KIND_PDSYNC_DATA);
        let record_count = payload
            .get("body")
            .and_then(|b| b.get("records"))
            .and_then(|r| r.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if !is_pdsync_data || record_count <= Self::INBOUND_CHUNK_SIZE {
            return self.process_one_inbound(
                storage,
                root_id,
                nickname,
                payload,
                remote_peer_id,
                online_peers,
                node_id,
                kverify,
                my_signing_key,
            );
        }
        // 拆批：records 数组按 INBOUND_CHUNK_SIZE 切片，其余字段原样
        // （parse_data 只读 category/records；逐条记录的 dseq 各自携带）。
        let body = payload.get("body").cloned().unwrap_or(Value::Null);
        let records = body
            .get("records")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut merged: Option<crate::kernel::inbound_dm::InboundDmResult> = None;
        for chunk in records.chunks(Self::INBOUND_CHUNK_SIZE) {
            let mut chunk_payload = payload.clone();
            if let Some(obj) = chunk_payload
                .get_mut("body")
                .and_then(|b| b.as_object_mut())
            {
                obj.insert("records".to_string(), Value::Array(chunk.to_vec()));
            }
            let r = self.process_one_inbound(
                storage,
                root_id,
                nickname,
                chunk_payload,
                remote_peer_id,
                online_peers,
                node_id,
                kverify,
                my_signing_key,
            )?;
            merged = Some(match merged {
                None => r,
                Some(mut acc) => {
                    // 聚合：事件与出站指令拼接；布尔按或；Option 取后值
                    acc.events.extend(r.events);
                    acc.pdsync_out.extend(r.pdsync_out);
                    acc.orgsync_out.extend(r.orgsync_out);
                    acc.device_notice_broadcast |= r.device_notice_broadcast;
                    acc.profile_applied |= r.profile_applied;
                    if r.auto_accept.is_some() {
                        acc.auto_accept = r.auto_accept;
                    }
                    if r.self_profile.is_some() {
                        acc.self_profile = r.self_profile;
                    }
                    if r.device_sync_reply.is_some() {
                        acc.device_sync_reply = r.device_sync_reply;
                    }
                    if r.profile_sync_reply.is_some() {
                        acc.profile_sync_reply = r.profile_sync_reply;
                    }
                    acc.orgkey_unbox.extend(r.orgkey_unbox);
                    if r.feed_blob_out.is_some() {
                        acc.feed_blob_out = r.feed_blob_out;
                    }
                    acc.response = r.response;
                    acc
                }
            });
        }
        merged.ok_or_else(|| "empty pdsync batch".to_string())
    }

    /// 收尾（§7.1）：标记某自设备（连接层 peerId）已证明支持 pdsync。保活据此
    /// 停止向其回退发旧快照。幂等（集合内重复无影响）。
    fn kernel_pdsync_capable_mark(&self, peer_id: &str) {
        self.pdsync_capable_self_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer_id.to_string());
    }

    /// F3（§20.6 离线补投）：验签通过的 orgsync-hello → 提取 (orgId, from)，
    /// 供 [`Self::spawn_orgkey_pending_resend`] 重投该成员的 orgkey pending
    /// （离线/无 accessKey 期间 grant/revoke 的密钥补投）。
    ///
    /// 完整验签先于触发——伪造信封不得驱动出站投递（重投本身幂等无害，
    /// 但验签节流避免被恶意信封反复触发扫描/装配）。成员资格由入站编排
    /// 校验；pending 收件人本就来自成员表（plan 内 find_member 兜底）。
    pub(crate) fn orgkey_resend_trigger(
        payload: &Value,
        my_root_id: &str,
    ) -> Option<(String, String)> {
        let kind = payload.get("kind").and_then(Value::as_str)?;
        if kind != super::super::dm_envelope::KIND_ORGSYNC_HELLO {
            return None;
        }
        let verified = dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
        let (org_id, ..) = crate::sync::orgsync::parse_orgsync_hello(&verified.body)?;
        Some((org_id, verified.from))
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
        let verified = dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
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
        // F3（§20.6 离线补投）：验签通过的 orgsync-hello = 该成员上线——
        // 重投其在本机的 orgkey pending。稳态零成本（无 pending 直接跳过）。
        if let Some((org_id, from)) = Self::orgkey_resend_trigger(&payload, &root_id) {
            self.spawn_orgkey_pending_resend(&root_id, &org_id, &from);
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
        let node_id = self.sync_node_id();
        let kverify = derive_kverify_from_password_shared(
            &self.storage,
            &self.password_shared,
            &self.kverify_cache,
        );
        // 大宗 pdsync-data 拆子批逐段持锁（UI 可插队，消除初始同步突发
        // 的界面冻结）；小信封保持原单锁路径。见 maybe_chunked_process。
        let result = self.maybe_chunked_process(
            &mut storage,
            &root_id,
            &nickname,
            payload,
            remote_peer_id,
            online_peers,
            &node_id,
            kverify.as_ref(),
            my_signing_key.as_ref(),
        )?;
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
        // 自设备加入通知广播（M1：friend-accept 自身份分支触发）
        if result.device_notice_broadcast {
            self.spawn_device_notice_broadcast(&root_id);
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
        // F3 残余：Vec——含暂存重评估一次产出的多条 unbox 指令。
        for unbox in result.orgkey_unbox {
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
    /// M1 补发窗口内：若自设备 peer 首次连接且窗口未过期，广播本机
    /// device_joined 通知。幂等键 `p2p:device:noticeSent:{peerId}` 防止重复发送。
    pub(crate) fn maybe_spawn_device_notice_broadcast(&self, my_root_id: &str, peer_id: &str) {
        let now_ms = crate::p2p::node::system_now_ms();
        if !replies::device_notice_window_open(&self.storage, now_ms) {
            return;
        }
        if replies::device_notice_sent(&self.storage, peer_id) {
            return;
        }
        self.spawn_device_notice_broadcast(my_root_id);
    }

    /// O4 §20.6：orgkey-deliver 解包落库。用本机 `org-access:{orgId}` 域身份
    /// 私钥（seed 派生 X25519）+ sender（owner）组织身份公钥 X25519 解
    /// crypto_box，得 32B epoch 密钥后写 orgkey 表。seed 缺失（锁定态）或
    /// 解包失败 → 静默跳过（不落库；后续重新投递补投）。
    pub(crate) fn apply_orgkey_unbox(
        &self,
        my_root_id: &str,
        unbox: &crate::kernel::inbound_dm::OrgkeyUnbox,
    ) {
        let seed = self
            .seed_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(seed) = seed else {
            log::info!(
                "[ORGKEY] unbox skipped: no seed (locked) | col={}:{}",
                unbox.org_id,
                unbox.name
            );
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

    /// F3：验签通过的 orgsync-hello → 提取 (orgId, from) 供 pending 重投；
    /// 非 hello kind / body 畸形 / 验签失败均不触发。
    #[test]
    fn orgkey_resend_trigger_on_verified_orgsync_hello() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let from = hex::encode(sha2::Sha256::digest(key.verifying_key().to_bytes()));
        let my_root = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
        let org_id = "org_0123456789abcdef";
        let body = crate::sync::orgsync::build_orgsync_hello(
            org_id,
            serde_json::Map::new(),
            &["data".to_string()],
            "pc",
        );
        let hello = dm_envelope::build_envelope(
            dm_envelope::KIND_ORGSYNC_HELLO,
            &from,
            my_root,
            crate::p2p::node::system_now_ms(),
            body,
            &key,
        );
        assert_eq!(
            KernelDmHandler::orgkey_resend_trigger(&hello, my_root),
            Some((org_id.to_string(), from.clone())),
            "合法 hello 触发 (orgId, from)"
        );

        // 非 hello 的 orgsync kind 不触发
        let need = dm_envelope::build_envelope(
            dm_envelope::KIND_ORGSYNC_NEED,
            &from,
            my_root,
            crate::p2p::node::system_now_ms(),
            json!({}),
            &key,
        );
        assert_eq!(KernelDmHandler::orgkey_resend_trigger(&need, my_root), None);

        // hello kind 但 body 畸形（缺 orgId/collections）不触发
        let bad_body = dm_envelope::build_envelope(
            dm_envelope::KIND_ORGSYNC_HELLO,
            &from,
            my_root,
            crate::p2p::node::system_now_ms(),
            json!({}),
            &key,
        );
        assert_eq!(
            KernelDmHandler::orgkey_resend_trigger(&bad_body, my_root),
            None
        );

        // 验签失败（to 非本机）不触发——伪造信封不得驱动出站重投
        let not_for_me = dm_envelope::build_envelope(
            dm_envelope::KIND_ORGSYNC_HELLO,
            &from,
            "some-other-member",
            crate::p2p::node::system_now_ms(),
            crate::sync::orgsync::build_orgsync_hello(org_id, serde_json::Map::new(), &[], "pc"),
            &key,
        );
        assert_eq!(
            KernelDmHandler::orgkey_resend_trigger(&not_for_me, my_root),
            None
        );
    }
}
