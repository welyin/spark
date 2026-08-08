//! kernel 的 P2pHost 实现：把 p2p 事件循环的业务回调接到内核存储与身份状态上。
//!
//! 已接线的回调：`current_root_id`、`evidence_head_hash`、`apply_remote_update`
//! （sync 模块远端应用 + purge 水位线拦截）、`recovery_view`（org 模块恢复视图）、
//! org-share 接收应答（`apply_incoming_org_share`：快照合并 → 落库 → pluginDocs
//! → ack）、org-pull-list/org 响应（`handle_org_pull_*`，org::pull 纯逻辑）、
//! org-share-ack 唤醒（`on_org_share_ack` → 推送编排的等待器注册表）。
//!
//! 纯逻辑全在 org 模块（snapshot/pull/plugin_docs），本层只做编排与错误映射。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::collection::{CollectionConfig, DocumentCollection};
use crate::contact::ContactService;
use crate::data_mgmt::watermark::StoragePurgeWatermark;
use crate::evidence::get_evidence_head_hash;
use crate::org::gateway::OrgMemberHint;
use crate::org::recovery::RecoveryViewItem;
use crate::org::{
    OrganizationService, PluginDocSyncItem, apply_plugin_doc_sync_items, handle_pull_list_request,
    handle_pull_org_request, validate_incoming_share_payload,
};
use crate::p2p::host::{DmHandler, OrgShareAck, P2pHost};
use crate::p2p::node::system_now_ms;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::peer_activity::{NodeObservation, PeerActivityStore};
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::P2pNode;
use crate::schema::CollectionSchemaDeclaration;
use crate::storage::{StorageBackend, SledStorage};
use crate::sync::apply::{ApplyRemoteOptions, apply_remote_update};
use crate::sync::meta::RemoteMeta;

use super::dm_envelope::{
    self, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_FRIEND_ACCEPT, KIND_PROFILE_SYNC,
};
use super::inbound_dm::AutoAccept;

/// 集合配置注册表：`(domain, collection) → CollectionConfig`。
///
/// 远端应用路径的索引维护需要本集合的 `indexedFields`（TS 来自插件侧构造的
/// collection 实例）；kernel 侧以 doc_* 调用时登记的配置为准，未登记的集合
/// 按无索引字段处理（文档与 meta 仍落库，仅不建二级索引）。
pub(crate) type CollectionConfigs = Arc<Mutex<HashMap<(String, String), CollectionConfig>>>;

/// org-share-ack 等待器注册表（对齐 TS OrgShareSessionState）：
/// 推送编排在 pubsub 重试节奏中按 syncId 注册 oneshot 等待器；pubsub 收到
/// `org-share-ack` 时由 [`KernelHost::on_org_share_ack`] 按 syncId 唤醒。
/// ack 先于等待器注册到达时进竞态缓存（org-share-session.ts:11-38 的
/// early-ack 语义）；无等待器且缓存满时丢弃。
#[derive(Default)]
pub(crate) struct OrgShareAckTracker {
    waiters: HashMap<String, tokio::sync::oneshot::Sender<()>>,
    early_acks: std::collections::HashSet<String>,
}

impl OrgShareAckTracker {
    /// 注册等待器（调用方随后 await 返回的接收端）。
    pub(crate) fn register(&mut self, sync_id: &str) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.insert(sync_id.to_string(), tx);
        rx
    }

    /// 超时清理等待器（避免泄漏）。
    pub(crate) fn remove_waiter(&mut self, sync_id: &str) {
        self.waiters.remove(sync_id);
    }

    /// 竞态缓存查询：ack 先于 register 到达时命中（一次性消费）。
    pub(crate) fn take_early_ack(&mut self, sync_id: &str) -> bool {
        self.early_acks.remove(sync_id)
    }

    /// ack 到达：有等待器则唤醒，否则进竞态缓存。
    pub(crate) fn mark_ack(&mut self, sync_id: &str) {
        if let Some(tx) = self.waiters.remove(sync_id) {
            let _ = tx.send(());
            return;
        }
        if self.early_acks.len() >= 256 {
            self.early_acks.clear();
        }
        self.early_acks.insert(sync_id.to_string());
    }
}

/// 共享 ack 注册表（host 与 org-sync worker 跨线程）。
pub(crate) type SharedOrgShareAckTracker = Arc<Mutex<OrgShareAckTracker>>;

/// kernel 宿主：持有与门面共享的存储句柄与当前身份指针。
pub(crate) struct KernelHost {
    pub(crate) storage: SledStorage,
    pub(crate) current_root_id: Arc<Mutex<Option<String>>>,
    pub(crate) collection_configs: CollectionConfigs,
    pub(crate) org_acks: SharedOrgShareAckTracker,
    /// claim 落库后的推送通知（org-sync 请求队列）：host 处于同步上下文，
    /// 异步推送由 kernel 的 org-sync worker 消费（对齐 service.ts:450）。
    pub(crate) push_notify: tokio::sync::mpsc::UnboundedSender<super::org_sync::OrgSyncRequest>,
    /// dm 入站事件的广播通道（ChatReceived/ChatStatus/FriendRequest* 由
    /// [`super::inbound_dm`] 产出，host 在此 emit 给壳层订阅者）。
    pub(crate) event_tx: tokio::sync::broadcast::Sender<crate::p2p::P2pEvent>,
    /// 当前身份昵称共享格（kernel 在解锁/资料更新时刷新、lock 清空；
    /// 避免事件循环线程逐条 dm 读身份文件）。
    pub(crate) nickname_shared: Arc<Mutex<String>>,
    /// 当前身份头像共享格（data URL，空串=无头像；口径同 nickname_shared）。
    pub(crate) avatar_shared: Arc<Mutex<String>>,
    /// p2p 节点句柄共享格（start 后由 kernel 回填；auto_accept 回发
    /// friend-accept 用——host 在事件循环线程内不能 block_on，改为
    /// `tokio::spawn` 驱动节点命令通道）。
    pub(crate) node_shared: Arc<Mutex<Option<Arc<P2pNode>>>>,
    /// 解锁期签名私钥共享格（auto_accept 回发信封签名用；lock 时清除）。
    pub(crate) signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    /// 解锁期会话口令共享格（自设备 profile-sync 全量快照应用身份文件时
    /// 重封加密 payload 用；lock 时清除）。
    pub(crate) password_shared: Arc<Mutex<Option<String>>>,
    /// 数据目录（身份文件读写路径推导用，与 kernel `config.data_dir` 同源）。
    pub(crate) data_dir: std::path::PathBuf,
    /// 存储读写互斥（与 kernel 变更类门面方法同一把；`handle_dm` 的入站
    /// 落库在锁内执行，避免与 Tauri 命令线程的 read-modify-write 交错）。
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（收尾能力探测，§7.1；按连接层
    /// peerId 键控=按设备粒度，与 kernel 共享，host `handle_dm` 写入、
    /// org-sync 保活读取）。
    pub(crate) pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl KernelHost {
    /// 按登记配置构造集合适配器（pluginDocs 应用与远端应用共用）。
    fn make_collection(&self, domain: &str, collection: &str) -> DocumentCollection {
        let config = self
            .collection_configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(domain.to_string(), collection.to_string()))
            .cloned()
            .unwrap_or_default();
        DocumentCollection::new(domain, collection, config)
    }

    /// 由共享字段组装可在事件循环线程外执行的 dm 入站处理器。
    fn dm_handler_impl(&self) -> KernelDmHandler {
        KernelDmHandler {
            storage: self.storage.clone(),
            current_root_id: Arc::clone(&self.current_root_id),
            nickname_shared: Arc::clone(&self.nickname_shared),
            avatar_shared: Arc::clone(&self.avatar_shared),
            event_tx: self.event_tx.clone(),
            node_shared: Arc::clone(&self.node_shared),
            signing_key_shared: Arc::clone(&self.signing_key_shared),
            password_shared: Arc::clone(&self.password_shared),
            data_dir: self.data_dir.clone(),
            io_lock: Arc::clone(&self.io_lock),
            pdsync_capable_self_devices: Arc::clone(&self.pdsync_capable_self_devices),
        }
    }
}

/// kernel 的 dm 入站处理器：字段全部为 `Arc`/`SledStorage` 克隆，`Send + Sync`，
/// 由事件循环 spawn 到阻塞线程池执行（验签/落库等重 IO 不占事件循环线程）。
pub(crate) struct KernelDmHandler {
    storage: SledStorage,
    current_root_id: Arc<Mutex<Option<String>>>,
    nickname_shared: Arc<Mutex<String>>,
    avatar_shared: Arc<Mutex<String>>,
    event_tx: tokio::sync::broadcast::Sender<crate::p2p::P2pEvent>,
    node_shared: Arc<Mutex<Option<Arc<P2pNode>>>>,
    signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    password_shared: Arc<Mutex<Option<String>>>,
    data_dir: std::path::PathBuf,
    io_lock: Arc<Mutex<()>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（收尾能力探测，§7.1，按设备粒度）。
    pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
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
    /// 稳定 id（见 [`super::doc_ops::persisted_sync_node_id`]，避免多台离线
    /// 设备共用 `local-node` 被向量比较判为同源）。
    fn sync_node_id(&self) -> String {
        self.node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| super::doc_ops::persisted_sync_node_id(&self.storage))
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
    fn pdsync_capability_mark(
        payload: &Value,
        my_root_id: &str,
        remote_peer_id: &str,
    ) -> Option<String> {
        let kind = payload.get("kind").and_then(Value::as_str)?;
        if !matches!(
            kind,
            super::dm_envelope::KIND_PDSYNC_HELLO
                | super::dm_envelope::KIND_PDSYNC_NEED
                | super::dm_envelope::KIND_PDSYNC_DATA
        ) {
            return None;
        }
        let verified =
            dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
        (verified.from == my_root_id).then(|| remote_peer_id.to_string())
    }

    /// 自动接受/重确认的回发：取本机节点信息装配 friend-accept 信封
    /// （设备配对 from==to==我；重确认 to=请求方 rootId），经节点命令通道
    /// 尽力投递。
    ///
    /// 本方法在阻塞线程池线程内运行（不能 `block_on`）——`tokio::spawn`
    /// 到同一 runtime 驱动（事件循环空闲时处理 DmDirect 命令）；节点未回填或
    /// 身份已锁（无签名私钥）时静默跳过，不影响已完成的本地落库。
    fn spawn_auto_accept(&self, my_root_id: &str, nickname: &str, auto_accept: AutoAccept) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let from = my_root_id.to_string();
        let to = auto_accept.to_root_id.clone();
        let nickname = nickname.to_string();
        // 头像共享格（空串=无头像，body 省略 avatar 字段）
        let avatar = {
            let shared = self
                .avatar_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (!shared.trim().is_empty()).then_some(shared)
        };
        tokio::spawn(async move {
            let local = node.local_node_info().await.ok();
            let mut body = serde_json::json!({
                "requestId": auto_accept.request_id,
                "nickname": nickname,
            });
            if let Some(avatar) = avatar {
                body["avatar"] = serde_json::Value::from(avatar);
            }
            if let Some(info) = local {
                body["nodeInfo"] = serde_json::json!({
                    "peerId": info.peer_id,
                    "addresses": info.addresses,
                });
            }
            let envelope = dm_envelope::build_envelope(
                KIND_FRIEND_ACCEPT,
                &from,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&auto_accept.target, envelope).await;
        });
    }

    /// 自设备 profile-sync 全量快照应用：以会话口令重封身份文件，完成
    /// 「我的资料」跨设备同步（wiki/design/sync-and-evidence.md「个人空间
    /// 同步口径」：个人资料在个人设备间全量同步；identity.md §5「恢复后
    /// 头像经 profile-sync 找回」）。
    ///
    /// LWW 三向裁决（向量时钟为后续专项）：
    /// - 对端较新（`updatedAt` 严格大于本地）：应用快照，刷新共享槽并通知
    ///   前端（SelfProfileSynced）——防离线设备上线后以旧快照回灌；
    /// - 本机较新（严格小于）：返回 true，调用方据此向对端回发本机全量
    ///   快照（握手式交换——对端较旧/残缺时补齐，如 QR 恢复的新设备
    ///   updatedAt=0，其残缺快照不会覆盖本机资料，本机回发使其收敛）；
    /// - 相等：收敛态，不动（也不回发，无 ping-pong）。
    ///
    /// 身份已锁（无口令）/文件缺失/校验失败时静默跳过（返回 false），
    /// 不影响朋友记录已完成的更新。
    fn apply_self_profile(&self, root_id: &str, body: &Value) -> bool {
        let password = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(password) = password else {
            return false;
        };
        let Some(updated_at) = body.get("updatedAt").and_then(Value::as_i64) else {
            return false;
        };
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw) else {
            return false;
        };
        if updated_at < file.updated_at as i64 {
            // 本机资料较新：提示调用方回发本机快照补齐对端
            return true;
        }
        if updated_at == file.updated_at as i64 {
            return false;
        }
        // 线形三态 → update_profile 参数三态：字符串=设置，显式 null=清除，缺省=不变
        let nickname = body.get("nickname").and_then(Value::as_str);
        let tri_state = |key: &str| -> Option<Option<&str>> {
            match body.get(key) {
                Some(Value::Null) => Some(None),
                Some(Value::String(s)) => Some(Some(s.as_str())),
                _ => None,
            }
        };
        let avatar = tri_state("avatar");
        // gender/region/signature 的内核清除语义是 Some("")（空串=清除）
        let extra = |key: &str| -> Option<&str> {
            match body.get(key) {
                Some(Value::Null) => Some(""),
                Some(Value::String(s)) => Some(s.as_str()),
                _ => None,
            }
        };
        if crate::identity::update_profile(
            &mut file,
            &password,
            nickname,
            avatar,
            extra("gender"),
            extra("region"),
            extra("signature"),
        )
        .is_err()
        {
            return false;
        }
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return false;
        };
        if super::identity::write_identity_file_atomic(&path, &text).is_err() {
            return false;
        }
        // 共享格刷新（dm 应答/出站口径）+ 前端通知
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
        false
    }

    /// pdsync 合入 `profile:self` 后回写身份文件（P2）。
    ///
    /// sled 镜像已由 `handle_pdsync_data` LWW 落地；这里把 sled 的最新资料
    /// 同步回身份文件（保证两处一致）。仅解锁态（有口令）可重封身份文件；
    /// 锁定态跳过——下次 unlock 时以 sled 覆盖（见 login）。写失败静默。
    fn apply_profile_from_sled(&self, root_id: &str) {
        let Some(password) = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        // 读 sled profile:self
        let Some(raw) = self
            .storage
            .get(super::identity::PROFILE_SELF_KEY)
            .ok()
            .flatten()
        else {
            return;
        };
        let Ok(profile) = serde_json::from_str::<super::identity::SyncableProfile>(&raw) else {
            return;
        };
        let info = profile.to_profile_info();
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw_file) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw_file) else {
            return;
        };
        // 以 sled 为源（pdsync 已 LWW 裁决，sled profile:self 是本次合入胜者），
        // 回写身份文件资料字段。
        if crate::identity::update_profile(
            &mut file,
            &password,
            info.nickname.as_deref(),
            match info.avatar.as_deref() {
                Some(a) if !a.is_empty() => Some(Some(a)),
                _ => Some(None),
            },
            Some(info.gender.as_deref().unwrap_or("")),
            Some(info.region.as_deref().unwrap_or("")),
            Some(info.signature.as_deref().unwrap_or("")),
        )
        .is_err()
        {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return;
        };
        if super::identity::write_identity_file_atomic(&path, &text).is_err() {
            return;
        }
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        // 前端通知（与 apply_self_profile 同口径）：我的资料已被自设备同步更新
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
    }

    /// profile-sync 握手回发：读身份文件的全量资料快照装配 profile-sync 信封
    /// 单点回投（本机资料较对端新时由 `apply_self_profile` 裁决触发；spawn
    /// 模式同 `spawn_device_sync_reply`，失败静默）。
    fn spawn_profile_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{my_root_id}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(file) = crate::identity::IdentityFile::from_json(&raw) else {
            return;
        };
        // 昵称为空时回退 rootId 前 8 位（与出站口径一致）
        let nickname = file.nickname.clone().unwrap_or_default();
        let nickname = if nickname.trim().is_empty() {
            my_root_id.chars().take(8).collect::<String>()
        } else {
            nickname
        };
        let body = serde_json::json!({
            "nickname": nickname,
            "avatar": file.avatar,
            "gender": file.gender,
            "region": file.region,
            "signature": file.signature,
            "updatedAt": file.updated_at,
        });
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_PROFILE_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// contact-sync 配对回发：自动接受自设备配对的 friend-request 后，把
    /// 本机通讯录全量快照（朋友/申请/标签/分组/拉黑）回投给请求方设备——
    /// QR 恢复的新设备立即拿到通讯录，不必等断→连跳变或本地下一次变更
    /// （spawn 模式同 `spawn_auto_accept`，失败静默；对端 LWW 幂等合入）。
    fn spawn_contact_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let Ok(body) = crate::contact::build_contact_sync_snapshot(&self.storage, my_root_id)
        else {
            return;
        };
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_CONTACT_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// conv-sync 配对回发：自动接受自设备配对的 friend-request 后，把
    /// 本机会话元数据快照（direct 会话外壳 + 置顶/免打扰/草稿）回投给
    /// 请求方设备——新设备立即拿到会话列表，不必等断→连跳变或本地下一次
    /// 变更（spawn 模式同 `spawn_contact_sync_reply`，失败静默）。
    fn spawn_conv_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let Ok(body) = crate::message::build_conv_sync_snapshot(&self.storage) else {
            return;
        };
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_CONV_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// device-sync 握手回发：取本机设备记录（sled 设备清单的本机条目）装配
    /// device-sync 信封尽力回投——对端上线推送其记录时本机回推，双方设备
    /// 清单双向齐全（spawn 模式同 `spawn_auto_accept`，失败静默）。
    fn spawn_device_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let storage = self.storage.clone();
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let Ok(info) = node.local_node_info().await else {
                return;
            };
            let Some(peer_id) = info.peer_id else {
                return;
            };
            let Ok(Some(record)) = crate::device::DeviceService::get(&storage, &peer_id) else {
                return;
            };
            let Ok(body) = serde_json::to_value(&record) else {
                return;
            };
            let envelope = dm_envelope::build_envelope(
                super::dm_envelope::KIND_DEVICE_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// pdsync 出站投递：把纯逻辑层构建好的 hello/need/data body 装配成完整
    /// pdsync-* 信封，逐个 `dm_direct` 回投连接层对端（spawn 模式同
    /// `spawn_device_sync_reply`，失败静默）。
    fn spawn_pdsync_reply(
        &self,
        my_root_id: &str,
        target: PeerNodeInfo,
        outputs: Vec<super::inbound_dm::PdsyncOut>,
    ) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            for output in outputs {
                let kind = match &output {
                    super::inbound_dm::PdsyncOut::Push { .. } => {
                        super::dm_envelope::KIND_PDSYNC_DATA
                    }
                    super::inbound_dm::PdsyncOut::Need { .. } => {
                        super::dm_envelope::KIND_PDSYNC_NEED
                    }
                    super::inbound_dm::PdsyncOut::Data { .. } => {
                        super::dm_envelope::KIND_PDSYNC_DATA
                    }
                };
                let body = match &output {
                    super::inbound_dm::PdsyncOut::Push { body }
                    | super::inbound_dm::PdsyncOut::Need { body }
                    | super::inbound_dm::PdsyncOut::Data { body } => body.clone(),
                };
                let envelope = dm_envelope::build_envelope(
                    kind,
                    &to,
                    &to,
                    system_now_ms(),
                    body,
                    &signing_key,
                );
                let _ = node.dm_direct(&target, envelope).await;
            }
        });
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
            super::inbound_dm::handle_inbound_dm(
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

impl P2pHost for KernelHost {
    fn current_root_id(&mut self) -> Option<String> {
        self.current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn evidence_head_hash(&mut self) -> Option<String> {
        get_evidence_head_hash(&self.storage).ok().flatten()
    }

    fn apply_remote_update(
        &mut self,
        domain: &str,
        collection: &str,
        id: &str,
        payload: Value,
        meta: Value,
        schema: Option<Value>,
    ) -> std::result::Result<(), String> {
        let adapter = self.make_collection(domain, collection);
        let remote_meta: RemoteMeta =
            serde_json::from_value(meta).map_err(|e| format!("invalid remote meta: {e}"))?;
        let schema_decl: Option<CollectionSchemaDeclaration> = schema
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| format!("invalid schema hint: {e}"))?
            .flatten();
        // delete 消息 payload 为 null → None
        let payload_opt = if payload.is_null() {
            None
        } else {
            Some(payload)
        };
        apply_remote_update(
            &mut self.storage,
            &adapter,
            domain,
            collection,
            id,
            payload_opt.as_ref(),
            &remote_meta,
            ApplyRemoteOptions {
                schema: schema_decl,
                watermark: Some(&StoragePurgeWatermark),
                now_ms: system_now_ms(),
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// org-share 接收（org-share-sync.ts:178-252）：定向校验 → 快照合并落库
    /// → pluginDocs 应用 → ack。校验不命中按 TS 语义静默跳过（`Ok(None)`）。
    fn apply_incoming_org_share(
        &mut self,
        payload: Value,
        _source: &'static str,
    ) -> std::result::Result<Option<OrgShareAck>, String> {
        let current = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Ok((target_root_id, organization, sync_id, plugin_docs)) =
            validate_incoming_share_payload(&payload, current.as_deref())
        else {
            // TS：invalid payload / target mismatch / 非成员 → console.warn 后 accepted:false
            return Ok(None);
        };
        let now = system_now_ms();
        let merged =
            OrganizationService::apply_incoming_snapshot(&mut self.storage, &organization, now)
                .map_err(|e| e.to_string())?;
        // pluginDocs 随快照捎带（plugin-org-sync.ts `applyPluginDocSyncItems`）
        if !plugin_docs.is_empty() {
            let items: Vec<PluginDocSyncItem> = plugin_docs
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            let configs = Arc::clone(&self.collection_configs);
            apply_plugin_doc_sync_items(
                &mut self.storage,
                &items,
                |domain, collection| {
                    let config = configs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&(domain.to_string(), collection.to_string()))
                        .cloned()
                        .unwrap_or_default();
                    DocumentCollection::new(domain, collection, config)
                },
                now,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Some(OrgShareAck {
            sync_id,
            org_id: merged.org_id,
            target_root_id,
            receiver_root_id: current.expect("validated above"),
        }))
    }

    /// org-pull-list 响应（org-pull-sync.ts:149-198）：先处理 claim（仅已知
    /// 成员）→ 重读记录 → 成员身份过滤。claim 落库的组织经 push_notify 通知
    /// org-sync worker 推送（service.ts:450 落库后推送的异步化）。
    fn handle_org_pull_list(
        &mut self,
        payload: Value,
        remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        let current = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let now = system_now_ms();
        let (response, applied_orgs) = handle_pull_list_request(
            &mut self.storage,
            &payload,
            current.as_deref(),
            remote_peer_id.as_deref(),
            now,
        )
        .map_err(|e| e.to_string())?;
        // claim 落库后向已知成员推送（actor = 本机当前用户，service.ts:450-451）
        if let Some(actor) = current {
            for org_id in applied_orgs {
                let _ = self
                    .push_notify
                    .send(super::org_sync::OrgSyncRequest::PushOrg {
                        org_id,
                        actor_root_id: actor.clone(),
                    });
            }
        }
        Ok(response)
    }

    /// org-pull-org 响应（org-pull-sync.ts:200-241）。纯逻辑层
    /// `handle_pull_org_request` 保持只读。传入本机身份用于自设备
    /// claim 验明（放开 peer-mismatch，见 org/pull.rs）。
    fn handle_org_pull_org(
        &mut self,
        payload: Value,
        remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        let current = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let now = system_now_ms();
        let response = handle_pull_org_request(
            &self.storage,
            &payload,
            remote_peer_id.as_deref(),
            current.as_deref(),
            now,
        )
        .map_err(|e| e.to_string())?;
        Ok(response)
    }

    fn recovery_view(&mut self) -> Vec<RecoveryViewItem> {
        let Some(root_id) = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return Vec::new();
        };
        // nodeId：p2p 运行中为 peerId，否则持久化身份派生（同 dm 入站口径）
        let node_id = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| super::doc_ops::persisted_sync_node_id(&self.storage));
        OrganizationService::get_recovery_view(
            &mut self.storage,
            &root_id,
            system_now_ms(),
            &node_id,
        )
        .unwrap_or_default()
    }

    /// org-share-ack 唤醒：按 syncId 匹配推送编排注册的等待器（含竞态缓存）。
    fn on_org_share_ack(&mut self, payload: Value) {
        let Some(sync_id) = payload.get("syncId").and_then(Value::as_str) else {
            return;
        };
        self.org_acks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_ack(sync_id);
    }

    /// dm 直连接收（同步回退路径）：事件循环优先走 [`Self::dm_handler`]
    /// 的异步处理器；此路径仅在宿主句柄不可用时触发，在线集合不可得，
    /// 传空集（ChatReceived 事件 online 恒 false，仅影响展示）。
    fn handle_dm(
        &mut self,
        payload: Value,
        remote_peer_id: &str,
    ) -> std::result::Result<Value, String> {
        self.dm_handler_impl()
            .handle_dm(payload, remote_peer_id, &HashSet::new())
    }

    /// dm 入站重 IO（验签/落库）交给阻塞线程池执行的异步处理器。
    fn dm_handler(&self) -> Option<Arc<dyn DmHandler>> {
        Some(Arc::new(self.dm_handler_impl()))
    }

    /// 朋友建连：按 peer_id 扫描 `ct:friend:` 记录找匹配的朋友，命中则向其
    /// 尽力投递 profile-sync dm（`{"nickname", "avatar"?}`，寻址用朋友记录
    /// 的 peer 信息）。事件循环线程内执行：只做 KV 扫描与共享格读取，信封
    /// 装配与投递 `tokio::spawn` 到 runtime（同 `spawn_auto_accept` 模式，
    /// 不能 block_on）；节点未回填/身份已锁/无匹配朋友时静默跳过。
    fn on_peer_connected(&mut self, peer_id: &str) {
        let friend = ContactService::overview(&self.storage, "personal")
            .map(|view| view.friends)
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.peer.as_ref().is_some_and(|p| p.peer_id == peer_id));
        let Some(friend) = friend else {
            return;
        };
        let Some(peer) = friend.peer else {
            return;
        };
        let Some(my_root_id) = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        // 昵称为空时回退 rootId 前 8 位（与 dm 入站应答/出站口径一致）
        let nickname = {
            let shared = self
                .nickname_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if shared.trim().is_empty() {
                my_root_id.chars().take(8).collect()
            } else {
                shared
            }
        };
        let avatar = {
            let shared = self
                .avatar_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (!shared.trim().is_empty()).then_some(shared)
        };
        let target = PeerNodeInfo {
            peer_id: (!peer.peer_id.is_empty()).then_some(peer.peer_id),
            addresses: peer.addresses,
        };
        let to = friend.root_id;
        let from = my_root_id;
        tokio::spawn(async move {
            let mut body = serde_json::json!({ "nickname": nickname });
            if let Some(avatar) = avatar {
                body["avatar"] = serde_json::Value::from(avatar);
            }
            let envelope = dm_envelope::build_envelope(
                KIND_PROFILE_SYNC,
                &from,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// 组织私有 DHT 成员提示回填（p2p-messages.md §15）：按未验证口径入邻居池
    /// + 活跃度 'seen' 记账；组织校验仍走 pull/claim 链路，信任边界不变。
    fn on_org_member_hints(&mut self, hints: &[OrgMemberHint]) {
        let now = system_now_ms();
        for hint in hints {
            if hint.peer_id.trim().is_empty() {
                continue;
            }
            let info = PeerNodeInfo {
                peer_id: Some(hint.peer_id.clone()),
                addresses: hint.addresses.clone(),
            };
            {
                let mut store = OverlayPeerStore::new(&mut self.storage);
                if let Err(e) = store.remember(
                    &hint.peer_id,
                    &hint.addresses,
                    OverlayPeerSource::Exchange,
                    false,
                    now,
                ) {
                    eprintln!("[kernel] org member hint overlay store failed: {e}");
                }
            }
            {
                let mut store = PeerActivityStore::new(&mut self.storage);
                if let Err(e) = store.remember_node_info(&info, NodeObservation::Seen, None, now) {
                    eprintln!("[kernel] org member hint activity store failed: {e}");
                }
            }
        }
    }

    /// peer 是否属于优先类目（自设备 / 好友）——peer-rediscovery §4.4。
    /// 查 sled 中的 `PriorityPeerStore`；集合仅存本地，不上网。
    fn is_priority_peer(&mut self, peer_id: &str) -> bool {
        let mut store = crate::p2p::priority_peers::PriorityPeerStore::new(&mut self.storage);
        store
            .is_priority(peer_id)
            .unwrap_or_else(|_| {
                eprintln!("[kernel] priority peer lookup failed for {peer_id}");
                false
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};

    /// rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致）。
    fn identity(seed: u8) -> (SigningKey, String) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
        (key, root_id)
    }

    /// 能力标记（§7.1）：只有验签通过且 from==本机 rootId 的 pdsync-* 信封
    /// 才标记，且按连接层 peerId 键控（每设备独立）。
    #[test]
    fn pdsync_capability_mark_requires_verified_self_envelope() {
        let (key, root) = identity(1);
        let now = system_now_ms();
        let body = serde_json::json!({ "categories": {} });

        // 合法自设备 pdsync-hello：标记连接层 peerId
        let hello = dm_envelope::build_envelope(
            dm_envelope::KIND_PDSYNC_HELLO,
            &root,
            &root,
            now,
            body.clone(),
            &key,
        );
        assert_eq!(
            KernelDmHandler::pdsync_capability_mark(&hello, &root, "peer-a"),
            Some("peer-a".to_string())
        );

        // 伪造信封：from 填本机 rootId 但用他人密钥签名（pubKey 哈希 != from）
        // → 验签失败，不标记
        let (other_key, other_root) = identity(2);
        let forged = dm_envelope::build_envelope(
            dm_envelope::KIND_PDSYNC_DATA,
            &root,
            &root,
            now,
            body.clone(),
            &other_key,
        );
        assert_eq!(
            KernelDmHandler::pdsync_capability_mark(&forged, &root, "peer-a"),
            None,
            "验签失败的信封不得标记能力"
        );

        // 合法签名但 from != 本机 rootId（非自设备）：不标记
        let foreign = dm_envelope::build_envelope(
            dm_envelope::KIND_PDSYNC_DATA,
            &other_root,
            &root,
            now,
            body.clone(),
            &other_key,
        );
        assert_eq!(
            KernelDmHandler::pdsync_capability_mark(&foreign, &root, "peer-a"),
            None,
            "from 非本机 rootId 不得标记能力"
        );

        // 自设备但非 pdsync kind：不标记
        let profile = dm_envelope::build_envelope(
            KIND_PROFILE_SYNC,
            &root,
            &root,
            now,
            body,
            &key,
        );
        assert_eq!(
            KernelDmHandler::pdsync_capability_mark(&profile, &root, "peer-a"),
            None
        );
    }
}
