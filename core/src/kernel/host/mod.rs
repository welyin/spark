//! kernel 的 P2pHost 实现：把 p2p 事件循环的业务回调接到内核存储与身份状态上。
//!
//! 已接线的回调：`current_root_id`、`evidence_head_hash`、`apply_remote_update`
//! （sync 模块远端应用 + purge 水位线拦截）、`recovery_view`（org 模块恢复视图）、
//! org-mail 入站（`handle_org_mail` → 网关代收/挑战拉取）。
//! （legacy org-share 接收/ack 与 org-pull 响应已随该平面退役删除。）
//!
//! 纯逻辑全在 org/sync 模块，本层只做编排与错误映射。
//!
//! 代码组织：本文件为 `P2pHost` 实现（[`KernelHost`]，组织/同步/存证回调）；
//! dm 入站处理器 [`KernelDmHandler`]（`DmHandler` 实现与各 spawn 回发）拆在
//! `dm_handler` 子模块（文件长度约束）。

mod dm_handler;

pub(crate) use dm_handler::KernelDmHandler;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::collection::{CollectionConfig, DocumentCollection};
use crate::contact::ContactService;
use crate::data_mgmt::watermark::StoragePurgeWatermark;
use crate::device::DeviceService;
use crate::evidence::get_evidence_head_hash;
use crate::org::OrganizationService;
use crate::org::gateway::OrgMemberHint;
use crate::org::recovery::RecoveryViewItem;
use crate::p2p::P2pNode;
use crate::p2p::host::{DmHandler, P2pHost};
use crate::p2p::node::system_now_ms;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::peer_activity::{NodeObservation, PeerActivityStore};
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::schema::CollectionSchemaDeclaration;
use crate::storage::{Backend, StorageBackend};
use crate::sync::apply::{ApplyRemoteOptions, apply_remote_update};
use crate::sync::meta::RemoteMeta;

use super::dm_envelope::{self, KIND_PROFILE_SYNC};

/// 集合配置注册表：`(domain, collection) → CollectionConfig`。
///
/// 远端应用路径的索引维护需要本集合的 `indexedFields`（TS 来自插件侧构造的
/// collection 实例）；kernel 侧以 doc_* 调用时登记的配置为准，未登记的集合
/// 按无索引字段处理（文档与 meta 仍落库，仅不建二级索引）。
pub(crate) type CollectionConfigs = Arc<Mutex<HashMap<(String, String), CollectionConfig>>>;

/// kernel 宿主：持有与门面共享的存储句柄与当前身份指针。
pub(crate) struct KernelHost {
    pub(crate) storage: Backend,
    pub(crate) current_root_id: Arc<Mutex<Option<String>>>,
    pub(crate) collection_configs: CollectionConfigs,
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
    /// 插件后台运行时宿主查询句柄（O3 filtered 权限钩子在 dm 入站执行）。
    pub(crate) plugin_host_query: crate::kernel::PluginHostQuery,
    /// Kverify 派生缓存（与 KernelDmHandler 共享同一 Arc；见 dm_handler.rs
    /// `derive_kverify_from_password_shared` 注释）。
    pub(crate) kverify_cache: Arc<Mutex<Option<(String, [u8; 32])>>>,
    /// indexer 角色配置共享格（affair-metadata §7/§8；kernel
    /// `set_indexer_enabled` / `set_indexer_coverage` 写、host 查询应答/
    /// 收录门控与 p2p 周期自公告读）。
    pub(crate) indexer_role_shared: Arc<Mutex<crate::index::directory::IndexRoleConfig>>,
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
            plugin_host_query: self.plugin_host_query.clone(),
            kverify_cache: Arc::clone(&self.kverify_cache),
        }
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
            &self.io_lock,
            &root_id,
            system_now_ms(),
            &node_id,
        )
        .unwrap_or_default()
    }

    /// 议题元数据公告入站（affair-metadata §4/§5）：完整线形校验 → 修订链
    /// 复算（本地有日志副本）→ 暂存区裁决 → 更新本地索引。与日志复算矛盾
    /// 的公告丢弃并告警（§4：公告 + 日志片段并排即证据）。暂存区是客户端
    /// 缓存语义（本地键不进同步），任何节点都收；索引查询应答由角色开关
    /// 另行门控。例外：角色启用且配置了子集覆盖时，收录面收窄到覆盖子集
    /// （§7 目录面——覆盖外公告不入暂存区/索引，索引内容与名片宣告一致）。
    fn on_affair_meta(&mut self, announce: Value) {
        let role = self
            .indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if role.enabled && !role.coverage.is_full() {
            // 覆盖过滤在完整线形校验前做粗筛：tags 提取失败按空集处理
            // （覆盖维度受限时空 tags 必不覆盖，等价于丢弃畸形公告）
            let tags: Vec<String> = announce
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let region = announce
                .get("region")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| crate::index::announce::extract_region(&tags));
            if !role.coverage.covers_announce(region.as_deref(), &tags) {
                return; // 覆盖外公告：静默跳过收录（目录宣告的子集之外不服务）
            }
        }
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match crate::index::query::ingest_announcement(
            &mut self.storage,
            &announce,
            system_now_ms(),
        ) {
            Ok(outcome) => outcome,
            Err(e) => {
                self.event_tx
                    .send(crate::p2p::P2pEvent::Warning(format!(
                        "affair-meta ingest failed: {e}"
                    )))
                    .ok();
                return;
            }
        };
        match outcome {
            crate::index::query::IngestOutcome::ConflictDropped => {
                self.event_tx
                    .send(crate::p2p::P2pEvent::Warning(
                        "affair-meta announce conflicts with local log replay; dropped".to_string(),
                    ))
                    .ok();
            }
            crate::index::query::IngestOutcome::Inserted
            | crate::index::query::IngestOutcome::Replaced => {
                let affair_id = announce
                    .get("affairId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.event_tx
                    .send(crate::p2p::P2pEvent::AffairMetaReceived { affair_id })
                    .ok();
            }
            crate::index::query::IngestOutcome::Kept => { /* 裁决保留既有，静默 */ }
        }
    }

    /// indexer 查询应答（affair-metadata §8）：角色开关门控（未启用回
    /// indexer-disabled），启用但查询超出子集覆盖回 indexer-not-covered
    /// （§7 目录面：客户端据此换目录内其他 indexer 重查），覆盖内则走与
    /// 本地直查完全相同的确定性分发。
    fn handle_affair_meta_query(&mut self, payload: &Value) -> Value {
        let role = self
            .indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !role.enabled {
            return serde_json::json!({ "error": "indexer-disabled" });
        }
        let Some(search) = crate::index::query::parse_search(payload) else {
            return serde_json::json!({ "error": "bad-query" });
        };
        if !role
            .coverage
            .covers_query(search.region.as_deref(), &search.tags)
        {
            return serde_json::json!({ "error": "indexer-not-covered" });
        }
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        match crate::index::query::run_search(&self.storage, &search, system_now_ms()) {
            Ok(payload) => payload,
            Err(e) => serde_json::json!({ "error": format!("indexer: {e}") }),
        }
    }

    /// indexer 角色配置（p2p tick 周期自公告读）：Some(覆盖) = 启用，
    /// None = 未启用。
    fn indexer_role(&mut self) -> Option<crate::index::directory::IndexCoverage> {
        let role = self
            .indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        role.enabled.then_some(role.coverage)
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

    /// org-mail 直连接收（阶段四E）：deliver/fetch 两 op 的网关侧处理——
    /// 轻量同步存储 IO（对齐 org-share 入站口径）。本机 peerId 取节点共享格。
    fn handle_org_mail(
        &mut self,
        payload: &Value,
        remote_peer_id: &str,
    ) -> std::result::Result<Value, String> {
        let my_root = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let my_peer = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_default();
        crate::kernel::org_mail_ops::handle_org_mail_inbound(
            &mut self.storage,
            my_root.as_deref(),
            &my_peer,
            payload,
            remote_peer_id,
        )
    }

    /// dm 入站重 IO（验签/落库）交给阻塞线程池执行的异步处理器。
    fn dm_handler(&self) -> Option<Arc<dyn DmHandler>> {
        Some(Arc::new(self.dm_handler_impl()))
    }

    /// 朋友应用层就绪（版本探测成功）：按 peer_id 扫描 `ct:friend:` 记录找匹配
    /// 的朋友，命中则向其尽力投递 profile-sync dm（`{"nickname", "avatar"?}`，
    /// 寻址用朋友记录的 peer 信息），并补投 `dm:pending:` 离线队列、自设备建连
    /// 时补发 M1 设备通知。事件循环线程内执行：只做 KV 扫描与共享格读取，信封
    /// 装配与投递 `tokio::spawn` 到 runtime（同 `spawn_auto_accept` 模式，
    /// 不能 block_on）；节点未回填/身份已锁/无匹配朋友时静默跳过。
    ///
    /// 业务投递的唯一触发信号（peer-app-ready-event §3.3）：transport 层
    /// `on_peer_connected` 不投业务，避免对 TCP 短暂连上/握手失败的地址反复拨号。
    /// profile-sync 失败直接放弃（不重试、不入 pending、不记状态，LWW 幂等）；
    /// `dm:pending:` 补投保留"入队等待下次连接"语义（与 profile-sync 不同策略）。
    fn on_peer_app_ready(&mut self, _version: &str, peer_id: &str) {
        // L4（mobile-leaf-mode §6）：orgsync 集合数据 pending 补投——按 peerId
        // 反查组织成员表端点（成员不必是联系人，下方朋友路径的 early-return
        // 不能拦这条），每个 (orgId, memberRootId) flush 组织空间
        // `org:dm:pending:` 队列。peer 已 app-ready（已连接），dm_direct 短路
        // 直发，寻址只需 peerId。
        // 稳态零成本：`org:dm:pending:` 前缀为空（绝大多数时刻）直接跳过成员表
        // 反查
        let has_org_pending = self
            .storage
            .scan(&crate::storage::ScanOptions::prefix(
                crate::dm_offline::ORG_PENDING_PREFIX,
            ))
            .map(|rows| !rows.is_empty())
            .unwrap_or(false);
        if has_org_pending {
            let orgs = crate::org::OrganizationService::read_all_organizations(&self.storage)
                .unwrap_or_default();
            let mut org_targets: Vec<(String, String)> = Vec::new();
            for record in &orgs {
                for member in &record.members {
                    let hit = member.node_info.as_ref().is_some_and(|set| {
                        set.iter()
                            .any(|info| info.peer_id.as_deref() == Some(peer_id))
                    });
                    if hit {
                        org_targets.push((record.org_id.clone(), member.root_id.clone()));
                    }
                }
            }
            // P2 L3：org-member-removed 补投——被移除者已出成员表，上方反查
            // 覆盖不到；按 pending 记录 body 内嵌的 targetPeerIds 快照匹配
            for (org_id, to_root_id) in
                super::dm_delivery::org_pending_removed_targets(&self.storage, peer_id)
            {
                org_targets.push((org_id, to_root_id));
            }
            if !org_targets.is_empty() {
                if let Some(node) = self
                    .node_shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    for (org_id, to_root_id) in org_targets {
                        let mut storage = self.storage.clone();
                        let node = Arc::clone(&node);
                        let event_tx = self.event_tx.clone();
                        let io_lock = Arc::clone(&self.io_lock);
                        let target = PeerNodeInfo {
                            peer_id: Some(peer_id.to_string()),
                            addresses: Vec::new(),
                        };
                        tokio::spawn(async move {
                            crate::kernel::dm_delivery::flush_pending_for_recipient(
                                &mut storage,
                                node,
                                event_tx,
                                io_lock,
                                crate::dm_offline::PendingSpace::Org(&org_id),
                                &to_root_id,
                                target,
                            )
                            .await;
                        });
                    }
                }
            }
        }
        let friend = ContactService::overview(&self.storage, "personal")
            .map(|view| view.friends)
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.peers.iter().any(|p| p.peer_id == peer_id));
        let Some(friend) = friend else {
            return;
        };
        let Some(peer) = friend.peers.iter().find(|p| p.peer_id == peer_id) else {
            return;
        };
        let peer = peer.clone();
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
        let from = my_root_id.clone();

        // M1 加入通知补发：自设备建连且在 24h 窗口内、未发送过，则广播
        if to == my_root_id {
            self.dm_handler_impl()
                .maybe_spawn_device_notice_broadcast(&my_root_id, peer_id);
        }
        // 离线补投（social-feed §6.3）：连接建立时按 rootId flush `dm:pending:`
        // 队列，重发密文信封；应答成功/终态拒绝出队并回写 chat 消息终态。
        // 事件循环线程不 block_on——与 profile-sync 同模式 spawn 到 runtime。
        // 用克隆避免与下方 profile-sync spawn 竞争 move 所有权。
        let flush_storage = self.storage.clone();
        let flush_node = Arc::clone(&node);
        let flush_event_tx = self.event_tx.clone();
        let flush_io_lock = Arc::clone(&self.io_lock);
        let flush_to = to.clone();
        let flush_target = target.clone();
        tokio::spawn(async move {
            let mut storage = flush_storage;
            crate::kernel::dm_delivery::flush_pending_for_recipient(
                &mut storage,
                flush_node,
                flush_event_tx,
                flush_io_lock,
                crate::dm_offline::PendingSpace::Personal,
                &flush_to,
                flush_target,
            )
            .await;
        });
        // profile-sync 尽力投递：应用层确认后投一次，失败直接放弃（不重试、
        // 不入 pending、不记状态），LWW 幂等，靠下次资料变更广播/下次应用层
        // 连接重新投递自然补上（peer-app-ready §3.4.1）。
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
    /// + 活跃度 'seen' 记账；组织成员关系以组织记录/成员条目为准（邀请流 +
    /// orgsync 收敛），信任边界不变。
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
                    None,
                    &std::collections::HashSet::new(),
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
        store.is_priority(peer_id).unwrap_or_else(|_| {
            eprintln!("[kernel] priority peer lookup failed for {peer_id}");
            false
        })
    }

    /// peer 是否已被本机撤销：查 `DeviceService` 看是否存在 peer_id 对应的
    /// 设备记录且 `revoked_at` 不为空。轻量 KV 级调用。
    fn is_revoked_peer(&mut self, peer_id: &str) -> bool {
        DeviceService::get(&self.storage, peer_id)
            .ok()
            .flatten()
            .is_some_and(|r| r.revoked_at.is_some())
    }

    /// R3 relay 候选梯队①判定（relay-implementation §2）：peer 是否属自设备
    /// （设备清单，已撤销不算）或本组织成员（组织成员表端点 peerId 命中）。
    /// KV/小扫描级（组织数量与成员端点数有界）。
    fn is_self_device_or_org_member(&mut self, peer_id: &str) -> bool {
        if DeviceService::get(&self.storage, peer_id)
            .ok()
            .flatten()
            .is_some_and(|r| r.revoked_at.is_none())
        {
            return true;
        }
        crate::org::OrganizationService::read_all_organizations(&self.storage)
            .unwrap_or_default()
            .iter()
            .any(|record| {
                record.members.iter().any(|m| {
                    m.node_info.as_ref().is_some_and(|set| {
                        set.iter()
                            .any(|info| info.peer_id.as_deref() == Some(peer_id))
                    })
                })
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
        let profile = dm_envelope::build_envelope(KIND_PROFILE_SYNC, &root, &root, now, body, &key);
        assert_eq!(
            KernelDmHandler::pdsync_capability_mark(&profile, &root, "peer-a"),
            None
        );
    }
}
