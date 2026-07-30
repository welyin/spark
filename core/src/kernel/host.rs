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
use crate::storage::SledStorage;
use crate::sync::apply::{ApplyRemoteOptions, apply_remote_update};
use crate::sync::meta::RemoteMeta;

use super::dm_envelope::{self, KIND_FRIEND_ACCEPT, KIND_PROFILE_SYNC};
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
    /// 存储读写互斥（与 kernel 变更类门面方法同一把；`handle_dm` 的入站
    /// 落库在锁内执行，避免与 Tauri 命令线程的 read-modify-write 交错）。
    pub(crate) io_lock: Arc<Mutex<()>>,
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
            io_lock: Arc::clone(&self.io_lock),
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
    io_lock: Arc<Mutex<()>>,
}

impl KernelDmHandler {
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
        // 入站落库整体在 io_lock 内执行（与 Tauri 命令线程的变更互斥）
        let result = {
            let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
            super::inbound_dm::handle_inbound_dm(
                &mut storage,
                &root_id,
                &nickname,
                payload,
                remote_peer_id,
                online_peers,
                system_now_ms(),
            )
            .map_err(|e| e.to_string())?
        };
        for event in result.events {
            // 无订阅者时忽略发送失败
            let _ = self.event_tx.send(event);
        }
        if let Some(auto_accept) = result.auto_accept {
            self.spawn_auto_accept(&root_id, &nickname, auto_accept);
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
    /// `handle_pull_org_request` 保持只读。
    fn handle_org_pull_org(
        &mut self,
        payload: Value,
        remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        let response = handle_pull_org_request(&self.storage, &payload, remote_peer_id.as_deref())
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
        OrganizationService::get_recovery_view(&mut self.storage, &root_id, system_now_ms())
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
}
