//! P2P 门面（`Kernel` 的网络 API）：p2p 起停/状态/DHT 模式、事件订阅与广播、
//! 节点名片（org.md §17）、组织地址解析/搜索（org.md §16.4）与节点活跃度
//! 记录维护。全部同步方法，内部以 `Handle::block_on` 驱动 [`P2pNode`]。

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::broadcast;

use super::host::KernelHost;
use super::org_sync::{self, OrgSyncContext, OrgSyncRequest};
use super::{Kernel, KernelError, Result};
use crate::org::{OrgAddressRecord, OrganizationService};
use crate::p2p::constants::{P2P_DHT_MODE_KEY, P2P_PEER_RECORD_PREFIX};
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_activity::PeerActivityStore;
use crate::p2p::{
    DhtMode, LocalP2PNodeInfo, P2pConfig, P2pError, P2pEvent, P2pNode, PeerNodeInfo,
    node_presence_record_key, verify_announce_text,
};
use crate::storage::{ScanOptions, StorageBackend};

/// `import_node_card` 的结果（org.md §17.4）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeCardImport {
    /// 名片发布方 peerId（已验签）。
    pub peer_id: String,
    /// 名片是否附带 org-recovery token。
    pub has_recovery_token: bool,
    /// 发起连接的 best-effort 结果（`None` = 已连接；`Some` = 失败原因或
    /// P2P 未启动）；连接失败不使导入失败——未验证条目已入邻居池，
    /// keepalive 会重试。
    pub connect_error: Option<String>,
}

impl Kernel {
    // ------------------------------------------------------------------
    // 组织地址（org.md §16.4：公开组织的发现入口）
    // ------------------------------------------------------------------

    /// 解析组织地址（org.md §16.4）：本地缓存 → DHT。
    ///
    /// 缓存命中且未过期直接返回；否则在 p2p 运行中向 DHT 查询
    /// （key = orgAddress 内嵌 digest），命中记录过五步校验链且 orgAddress
    /// 与查询一致才沉淀缓存并返回；其他情况返回 `Ok(None)`。
    pub fn resolve_org_address(&self, org_address: &str) -> Result<Option<OrgAddressRecord>> {
        use crate::org::org_address as oa;

        let normalized = org_address.trim();
        let Some(dht_key) = oa::org_address_dht_key(normalized) else {
            return Err(KernelError::Internal("Invalid org address".to_string()));
        };
        let now = system_now_ms();
        let storage = self.require_storage()?;
        if let Some(cached) = oa::read_cached_org_address_record(storage, normalized)
            && !oa::org_address_record_expired(&cached, now)
        {
            return Ok(Some(cached));
        }

        let Some(node) = &self.p2p else {
            return Ok(None);
        };
        let found = self
            .runtime
            .handle()
            .block_on(node.dht_get_record(&dht_key))?;
        let Some(value) = found else {
            return Ok(None);
        };
        let Ok(record) = serde_json::from_slice::<OrgAddressRecord>(&value) else {
            return Ok(None);
        };
        if record.org_address != normalized || !oa::verify_org_address_record(&record, now).is_ok()
        {
            return Ok(None);
        }
        let mut storage = storage.clone();
        let _ = oa::cache_org_address_record(&mut storage, &record);
        Ok(Some(record))
    }

    /// 本地搜索已知组织（org.md §16.4）：缓存按 displayName/orgAddress 子串
    /// 匹配，纯本地查询（备注为客户端本地概念，本期缓存只有 displayName）。
    pub fn search_known_orgs(&self, keyword: &str) -> Result<Vec<OrgAddressRecord>> {
        Ok(crate::org::org_address::search_cached_org_address_records(
            self.require_storage()?,
            keyword,
            system_now_ms(),
        ))
    }

    // ------------------------------------------------------------------
    // P2P API
    // ------------------------------------------------------------------

    /// 启动 P2P 节点（内部 tokio runtime 托管；幂等，重复调用返回现有 peerId）。
    /// 需要存储已打开（libp2p 身份/端口/邻居表持久化在库内）。
    ///
    /// 同时装配：事件泵（node 事件 → kernel 广播通道，`KeepaliveTick` 拦截为
    /// 组织保活触发）与 org-sync worker（推送/保活串行队列，org_sync/）。
    pub fn start_p2p(&mut self) -> Result<String> {
        // 首启时把 log crate 接到 stderr（内部 Once，幂等）；使 DM 投递等链路
        // 的 log::info! 在无 logger 时不再被静默丢弃。
        crate::log_bridge::init_logger();
        if let Some(node) = &self.p2p {
            return Ok(node.peer_id().to_string());
        }
        let storage = self.require_storage()?.clone();
        // 原始句柄：p2p 节点（邻居表/身份持久化）与 host（入站合入路径）
        // 专用——合入写的是对端版本的数据，不得经中间件二次 bump
        let raw = storage.raw().clone();
        let mut config = self.config.p2p.clone().unwrap_or_else(|| P2pConfig {
            app_version: self.config.app_version.clone(),
            ..Default::default()
        });
        // DHT 模式以持久化配置为准（p2p_set_dht_mode 写入；缺省沿用 config）
        if let Some(mode) = storage
            .get(P2P_DHT_MODE_KEY)?
            .as_deref()
            .and_then(DhtMode::parse)
        {
            config.dht_mode = mode;
        }
        let (org_sync_tx, org_sync_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = Box::new(KernelHost {
            storage: raw.clone(),
            current_root_id: Arc::clone(&self.current_root_id_shared),
            collection_configs: Arc::clone(&self.collection_configs),
            org_acks: Arc::clone(&self.org_acks),
            push_notify: org_sync_tx.clone(),
            event_tx: self.event_tx.clone(),
            nickname_shared: Arc::clone(&self.nickname_shared),
            avatar_shared: Arc::clone(&self.avatar_shared),
            node_shared: Arc::clone(&self.p2p_node_shared),
            signing_key_shared: Arc::clone(&self.signing_key_shared),
            password_shared: Arc::clone(&self.password_shared),
            seed_shared: Arc::clone(&self.seed_shared),
            data_dir: self.config.data_dir.clone(),
            io_lock: Arc::clone(&self.io_lock),
            pdsync_capable_self_devices: Arc::clone(&self.pdsync_capable_self_devices),
            orgsync_capable_member_peers: Arc::clone(&self.orgsync_capable_member_peers),
            plugin_host_query: self.plugin_host_query_handle(),
        });
        let mut node =
            self.runtime
                .handle()
                .block_on(P2pNode::start(config, raw.clone(), host))?;
        let peer_id = node.peer_id().to_string();
        // 版本化中间件的 node_id 切换为运行态 peerId（stop 时回退持久化 id）
        if let Some(cell) = &self.sync_node_cell {
            *cell.lock().unwrap_or_else(|e| e.into_inner()) = peer_id.clone();
        }
        self.p2p_start_error = None;
        // 存量好友回填优先集合：pdsync/历史导入的好友在启动时统一补入，
        // 保证断开后竞速识别立即可用（§4.4；已拉黑或无线索的不入）
        self.backfill_priority_peers()?;
        let mut events = node.take_events();
        let node = Arc::new(node);
        *self.p2p_node_shared.lock().unwrap() = Some(Arc::clone(&node));

        // org-sync worker：推送/保活串行消费
        let ctx = OrgSyncContext {
            storage,
            node: Arc::clone(&node),
            current_root_id: Arc::clone(&self.current_root_id_shared),
            signing_key: Arc::clone(&self.signing_key_shared),
            collection_configs: Arc::clone(&self.collection_configs),
            org_acks: Arc::clone(&self.org_acks),
            event_tx: self.event_tx.clone(),
            recovery_trigger: Arc::clone(&self.recovery_trigger),
            org_address_publish: Arc::clone(&self.org_address_publish),
            data_dir: self.config.data_dir.clone(),
            self_device_link: Arc::clone(&self.self_device_link),
            // 自设备连接状态必须全 kernel 共享——worker 持有的若不复位，
            // 自设备断连后"即时 hello"等触发路径仍向旧连接发（静默失败，
            // 要等下轮 keepalive 才收敛）
            self_device_links: Arc::clone(&self.self_device_links),
            pdsync_capable_self_devices: Arc::clone(&self.pdsync_capable_self_devices),
            orgsync_capable_member_peers: Arc::clone(&self.orgsync_capable_member_peers),
            // 稳态 hello 触发状态仅 keepalive tick 消费：worker 上下文（start_p2p
            // 装配，随 p2p 会话存活）持有即够；门面即席上下文新建空状态即可
            self_hello_state: Arc::new(std::sync::Mutex::new(org_sync::SelfHelloState::default())),
            self_hello_immediate: Arc::new(std::sync::Mutex::new(
                org_sync::ImmediateHelloState::default(),
            )),
            filter_caps: Arc::clone(&self.plugin_host.filter_caps),
            replica_check: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        };
        let worker = org_sync::spawn_worker(self.runtime.handle(), ctx, org_sync_rx);

        // 变更信号观察：版本化中间件的受管本地写入（put/delete）→ 防抖后
        // 向已连接自设备即时补发 pdsync-hello。替代分散在各业务操作里的
        // 手动 notify 调用点——任何本地写入（含未来新增功能）自动获得
        // 秒级同步触发。远端合入走 raw 句柄不触发信号（防回声）。
        let watch = {
            let watch_storage = self.require_storage()?.clone();
            let watch_tx = org_sync_tx.clone();
            self.runtime.handle().spawn(async move {
                let mut last = watch_storage.last_local_write_ms();
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    let cur = watch_storage.last_local_write_ms();
                    if cur > last {
                        last = cur;
                        let _ = watch_tx.send(OrgSyncRequest::SelfHelloNow);
                    }
                }
            })
        };
        self.sync_watch = Some(watch);

        // 事件泵：node 事件流 → kernel 广播通道（壳层订阅）；
        // KeepaliveTick 拦截为组织保活触发（覆盖网维护已在事件循环内完成）；
        // Connected/Disconnected 维护自设备连接集（即时 hello 消费——断连
        // 不剔除会让触发路径向旧连接静默发丢，要等下轮 keepalive 才收敛）
        let tx = self.event_tx.clone();
        let org_tx = org_sync_tx.clone();
        let links = Arc::clone(&self.self_device_links);
        let my_peer = peer_id.clone();
        let pump = self.runtime.handle().spawn(async move {
            while let Some(event) = events.recv().await {
                match &event {
                    P2pEvent::KeepaliveTick(_) => {
                        let _ = org_tx.send(OrgSyncRequest::KeepaliveTick);
                    }
                    P2pEvent::PeerConnected { peer_id } => {
                        if peer_id != &my_peer {
                            links.lock().unwrap_or_else(|e| e.into_inner()).insert(peer_id.clone());
                        }
                    }
                    P2pEvent::PeerDisconnected { peer_id } => {
                        links
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(peer_id.as_str());
                    }
                    _ => {}
                }
                // 无订阅者时忽略发送失败
                let _ = tx.send(event);
            }
        });
        self.p2p = Some(node.clone());
        self.p2p_started_at = Some(system_now_ms());

        // M3：p2p 启动成功后若 epoch 状态完全缺失，做 init 轮换（幂等）。
        if let Err(e) = crate::kernel::epoch_ops::maybe_init_epoch_state(self) {
            log::error!("[start-p2p] epoch init failed: {e}");
            let _ = crate::device::DeviceService::append_security_log(
                self.require_storage_mut()?,
                "rotation_failed",
                serde_json::json!({"reason": "init", "error": format!("{e}")}),
                system_now_ms(),
            );
        }

        self.p2p_pump = Some(pump);
        self.org_sync_worker = Some(worker);
        self.org_sync_tx = Some(org_sync_tx);

        // 设备管理（多设备同步）：本机设备信息采集落库（设备清单的本机条目），
        // 随后向全部已配对自设备补推 device-sync + 全量 profile-sync——对端离线
        // 错过的变更借此补齐（对端收 device-sync 会回发其记录，双向齐全）。
        let now = system_now_ms();
        if let Ok(mut storage) = self.require_storage().map(|s| s.clone()) {
            let node_id = self.sync_node_id();
            let device_pub_key = Some(node.device_pub_key().to_string())
                .filter(|s| !s.is_empty());
            if let Ok(record) = crate::device::DeviceService::upsert_self(
                &mut storage,
                &peer_id,
                now,
                &node_id,
                &self.config.app_version,
                device_pub_key,
            ) {
                if let Ok(root_id) = self.require_unlocked_root_id() {
                    let kverify = crate::kernel::pw_ops::derive_session_kverify(self)
                        .ok()
                        .flatten();
                    let _ = crate::epoch::EpochService::maybe_grant_epoch_key(
                        &mut storage,
                        &root_id,
                        &node_id,
                        &node_id,
                        now,
                        &record.peer_id,
                        record.device_pub_key.as_deref(),
                        record.revoked_at,
                        kverify.as_ref(),
                    );
                }
                if let Ok(data) = serde_json::to_value(&record) {
                    let _ = self.event_tx.send(P2pEvent::DeviceUpdated(data));
                }
                self.broadcast_device_sync(&record);
            }
        }
        self.broadcast_self_profile_snapshot();
        // M2（connection-policy §3.2）：登录一次性拨号——读设备清单 → DHT 刷新
        // → 拨一次（失败即沉默）。置于设备广播之后，设备清单本机条目已落库。
        let _ = self.bootstrap_login_dials();

        Ok(peer_id)
    }

    /// M2（connection-policy §3.2）：登录/启动一次性拨号——读设备清单
    /// （自设备 DeviceRecord + 联系人 FriendRecord.peers）→ 对每个 peerId 查
    /// DHT `spark:node:{peerId}` 拿新鲜地址（验签）→ 未命中用清单旧地址兜底 →
    /// 对每个 peer 按地址拨一次，失败即沉默（不进入任何周期重试队列）。
    ///
    /// 仅改登录/启动路径，不删周期拨号（删周期属 M3–M5）。已连接的 peer 由
    /// `connect_peer` 短路跳过。p2p 未启动/未解锁/存储不可用时静默跳过。
    ///
    /// **异步化**：整段拨号在 tokio 后台任务异步执行，本方法立即返回。登录
    /// 路径（`unlock` → `start_p2p`）不再因 DHT 查询/拨号的网络超时而被阻塞
    /// （dht_get_record 15s、connect_peer 默认 10s）——UI 先进入主界面，拨号
    /// 在后台进行（失败即沉默，语义与原先 block_on 完全一致）。
    fn bootstrap_login_dials(&self) -> Result<()> {
        let Some(node) = &self.p2p else {
            return Ok(());
        };
        if self.current_root_id()?.is_none() {
            return Ok(());
        }
        let storage = self.require_storage()?;
        let local_peer_id = self.p2p_status().ok().flatten().and_then(|i| i.peer_id);

        // 1. 收集目标 peerId → 兜底地址（去重：同 peerId 合并地址集）。
        //    - 自设备 DeviceRecord（不含本机；DHT 是主源，无旧地址兜底）
        //    - 联系人 FriendRecord.peers（好友 + 自记录；带清单旧地址作兜底）
        let mut by_peer: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for d in crate::device::DeviceService::list(storage)?.into_iter() {
            if local_peer_id.as_deref() != Some(d.peer_id.as_str())
                && !d.peer_id.trim().is_empty()
            {
                by_peer.entry(d.peer_id).or_default();
            }
        }
        let friends = crate::contact::ContactService::overview(storage, "personal")?.friends;
        for f in friends {
            for p in f.peers {
                if local_peer_id.as_deref() != Some(p.peer_id.as_str())
                    && !p.peer_id.trim().is_empty()
                {
                    by_peer.entry(p.peer_id.clone()).or_default().extend(p.addresses);
                }
            }
        }
        if by_peer.is_empty() {
            return Ok(());
        }

        let node = Arc::clone(node);
        let handle = self.runtime.handle().clone();
        // 2. 后台异步逐 peer 查 DHT 拿新鲜地址（验签）→ 未命中用清单旧地址
        //    兜底 → 拨一次。整段在后台任务执行，不阻塞登录路径。
        handle.spawn(async move {
            for (peer_id, fallback) in by_peer {
                let fresh = Kernel::query_node_presence_async(&node, &peer_id).await;
                let addresses = if fresh.is_empty() { fallback } else { fresh };
                if addresses.is_empty() {
                    continue;
                }
                let info = PeerNodeInfo {
                    peer_id: Some(peer_id),
                    addresses,
                };
                // 失败即沉默：一次性尝试，不进入任何周期重试队列
                let _ = node.connect_peer(&info).await;
            }
        });
        Ok(())
    }

    /// 查询并验签 DHT 节点存在记录（`spark:node:{peerId}`，peer-rediscovery
    /// §4.2），返回验签通过的新鲜地址列表；未命中/验签失败/记录 peerId 不匹配
    /// 返回空（调用方用清单旧地址兜底）。
    ///
    /// 异步版本：直接 await `P2pNode::dht_get_record`（其内部自带超时），不再
    /// `block_on`，供后台拨号任务调用。
    async fn query_node_presence_async(node: &P2pNode, peer_id: &str) -> Vec<String> {
        let key = node_presence_record_key(peer_id);
        let Ok(Some(raw)) = node.dht_get_record(key.as_bytes()).await else {
            return Vec::new();
        };
        let Ok(text) = String::from_utf8(raw) else {
            return Vec::new();
        };
        let Some(announce) = verify_announce_text(&text) else {
            return Vec::new();
        };
        if announce.peer_id != peer_id {
            return Vec::new();
        }
        announce.addresses
    }

    /// 停止 P2P 节点（幂等）：org-sync worker / 事件泵一并停止。
    pub fn stop_p2p(&mut self) -> Result<()> {
        self.org_sync_tx = None;
        *self.p2p_node_shared.lock().unwrap() = None;
        if let Some(watch) = self.sync_watch.take() {
            watch.abort();
        }
        if let Some(worker) = self.org_sync_worker.take() {
            worker.abort();
        }
        if let Some(pump) = self.p2p_pump.take() {
            pump.abort();
        }
        if let Some(node) = self.p2p.take() {
            self.runtime.handle().block_on(node.stop());
        }
        // node_id 回退持久化派生 id（离线写入仍可稳定归因）
        if let (Some(cell), Some(storage)) = (&self.sync_node_cell, &self.storage) {
            *cell.lock().unwrap_or_else(|e| e.into_inner()) =
                super::doc_ops::persisted_sync_node_id(storage.raw());
        }
        self.p2p_started_at = None;
        self.p2p_start_error = None;
        Ok(())
    }

    /// 登录链路自动启动 p2p 的失败原因（无则 `None`）。
    pub fn p2p_start_error(&self) -> Option<String> {
        self.p2p_start_error.clone()
    }

    /// 组装 org-sync 编排上下文（p2p 运行期可用）。
    pub(crate) fn org_sync_context(&self) -> Option<OrgSyncContext> {
        let node = self.p2p.as_ref()?;
        Some(OrgSyncContext {
            storage: self.storage.as_ref()?.clone(),
            node: Arc::clone(node),
            current_root_id: Arc::clone(&self.current_root_id_shared),
            signing_key: Arc::clone(&self.signing_key_shared),
            collection_configs: Arc::clone(&self.collection_configs),
            org_acks: Arc::clone(&self.org_acks),
            event_tx: self.event_tx.clone(),
            recovery_trigger: Arc::clone(&self.recovery_trigger),
            org_address_publish: Arc::clone(&self.org_address_publish),
            data_dir: self.config.data_dir.clone(),
            self_device_link: Arc::clone(&self.self_device_link),
            self_device_links: Arc::clone(&self.self_device_links),
            pdsync_capable_self_devices: Arc::clone(&self.pdsync_capable_self_devices),
            orgsync_capable_member_peers: Arc::clone(&self.orgsync_capable_member_peers),
            // 稳态 hello 触发状态仅 keepalive tick 消费：worker 上下文（start_p2p
            // 装配，随 p2p 会话存活）持有即够；门面即席上下文新建空状态即可
            self_hello_state: Arc::new(std::sync::Mutex::new(org_sync::SelfHelloState::default())),
            self_hello_immediate: Arc::new(std::sync::Mutex::new(
                org_sync::ImmediateHelloState::default(),
            )),
            filter_caps: Arc::clone(&self.plugin_host.filter_caps),
            replica_check: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        })
    }

    /// P2P 是否运行中。
    pub fn p2p_running(&self) -> bool {
        self.p2p.is_some()
    }

    /// 读取 DHT 模式配置（sled 配置键；缺省 Server）。
    pub fn p2p_dht_mode(&self) -> Result<DhtMode> {
        let storage = self.require_storage()?;
        Ok(storage
            .get(P2P_DHT_MODE_KEY)?
            .as_deref()
            .and_then(DhtMode::parse)
            .unwrap_or_default())
    }

    /// 写入 DHT 模式配置；p2p 运行中时重启节点使其生效（连接会断开重连）。
    ///
    /// 重启失败时配置已落盘不丢：报错文案注明"配置已保存，将在下次启动生效"，
    /// 并记入 `p2p_start_error` 供壳层展示（与登录链路自动启动失败同口径）。
    pub fn p2p_set_dht_mode(&mut self, mode: DhtMode) -> Result<()> {
        self.require_storage_mut()?
            .put(P2P_DHT_MODE_KEY, mode.as_str())?;
        if self.p2p.is_some() {
            self.stop_p2p()?;
            if let Err(e) = self.start_p2p() {
                let msg = format!("{e}（DHT 模式配置已保存，将在下次启动生效）");
                self.p2p_start_error = Some(msg.clone());
                return Err(KernelError::Internal(msg));
            }
        }
        Ok(())
    }

    /// 存量好友回填优先类目集合（§4.4）：遍历个人空间好友表，把未拉黑且
    /// 有 peerId 的好友加入集合；自设备由 handle_self_friend_request 单独维护。
    fn backfill_priority_peers(&mut self) -> Result<()> {
        let prefix = crate::contact::FRIEND_PREFIX;
        let rows = self
            .require_storage()?
            .scan(&ScanOptions::prefix(prefix))?;
        for (key, _) in rows {
            let Some(root_id) = key.strip_prefix(prefix) else {
                continue;
            };
            if root_id.is_empty() {
                continue;
            }
            let friend =
                crate::contact::ContactService::get_friend(self.require_storage()?, root_id)?;
            let Some(friend) = friend else {
                continue;
            };
            if friend.blocked {
                continue;
            }
            // 多设备寻址：该好友每台已知设备均入优先集合
            let peer_ids: Vec<String> = friend
                .peers
                .into_iter()
                .map(|p| p.peer_id)
                .filter(|id| !id.trim().is_empty())
                .collect();
            let mut priority =
                crate::p2p::priority_peers::PriorityPeerStore::new(self.require_storage_mut()?);
            for peer_id in peer_ids {
                if let Err(e) = priority.add(&peer_id) {
                    eprintln!("[p2p] backfill priority peer add failed: {e}");
                }
            }
        }
        Ok(())
    }

    /// 通知内核网络接口变化（WiFi↔蜂窝切换等；peer-rediscovery §4.1.3）。
    /// 壳层在检测到连接变更后调用；P2P 未启动时静默忽略。
    pub fn p2p_network_changed(&self) -> Result<()> {
        match &self.p2p {
            None => Ok(()),
            Some(node) => {
                self.runtime.handle().block_on(node.network_changed())?;
                Ok(())
            }
        }
    }

    /// P2P 状态快照（未启动返回 `Ok(None)`）。
    pub fn p2p_status(&self) -> Result<Option<LocalP2PNodeInfo>> {
        match &self.p2p {
            None => Ok(None),
            Some(node) => Ok(Some(
                self.runtime.handle().block_on(node.local_node_info())?,
            )),
        }
    }

    /// 生成节点名片串（org.md §17）：本机 libp2p 私钥签名的 base64url 名片，
    /// 供线下渠道（二维码/粘贴）分享，帮助失联成员手动找回本节点。
    ///
    /// 带 `org_id` 时附当前时间桶的 recoveryToken（`sha256hex(orgId:
    /// recoverySecret:timeBucket)`，org.md §10），面向"帮组织恢复"场景。
    /// 需要 P2P 已启动（名片携带本机监听地址；未启动报 `p2p node not
    /// started`）；组织不存在报 `Organization not found`，组织缺
    /// recoverySecret（存量组织未被管理员补齐）报专用中文文案。
    pub fn make_node_card(&mut self, org_id: Option<&str>) -> Result<String> {
        let node = self.p2p.as_ref().ok_or(P2pError::NotStarted)?;
        let local = self.runtime.handle().block_on(node.local_node_info())?;
        let peer_id = local
            .peer_id
            .ok_or_else(|| KernelError::Internal("p2p node not started".to_string()))?;
        let now = system_now_ms();
        let recovery_token = match org_id {
            Some(org_id) => {
                let record = OrganizationService::get_record(self.require_storage()?, org_id)?
                    .ok_or(crate::org::OrgError::OrganizationNotFound)?;
                let secret = record.recovery_secret().ok_or_else(|| {
                    KernelError::Internal(
                        "该组织暂无恢复密钥，请稍后重试或不附带恢复 token".to_string(),
                    )
                })?;
                Some(crate::org::recovery_token(
                    org_id,
                    secret,
                    crate::org::recovery_time_bucket(now),
                ))
            }
            None => None,
        };
        let keypair =
            crate::p2p::identity_store::get_or_create_libp2p_keypair(self.require_storage_mut()?)?;
        crate::org::make_node_card(&keypair, &peer_id, &local.addresses, now, recovery_token)
            .map_err(|e| KernelError::Internal(format!("node card signing failed: {e}")))
    }

    /// 导入节点名片（org.md §17.3-4）：完整校验链（结构 → 新鲜度 → token
    /// 形状 → 验签）→ **一律未验证口径**入覆盖网邻居池 → best-effort 发起
    /// 连接（失败不使导入失败，错误记入返回值）。后续组织校验照旧走
    /// pull/claim 链路，不在本命令内做（信任边界不变）。
    pub fn import_node_card(&mut self, card: &str) -> Result<NodeCardImport> {
        let now = system_now_ms();
        let parsed = crate::org::parse_and_verify_node_card(card, now)
            .map_err(|e| KernelError::Internal(e.to_string()))?;
        {
            let storage = self.require_storage_mut()?;
            let mut store = crate::p2p::OverlayPeerStore::new(storage);
            store.remember(
                &parsed.peer_id,
                &parsed.addresses,
                crate::p2p::OverlayPeerSource::Exchange,
                false,
                now,
                None,
                &std::collections::HashSet::new(),
            )?;
        }
        let mut connect_error = None;
        match &self.p2p {
            Some(node) => {
                let target = PeerNodeInfo {
                    peer_id: Some(parsed.peer_id.clone()),
                    addresses: parsed.addresses.clone(),
                };
                if let Err(e) = self.runtime.handle().block_on(node.connect_peer(&target)) {
                    connect_error = Some(e.to_string());
                }
            }
            // P2P 未启动：条目已入池，keepalive 启动后会重试，如实告知 UI
            None => connect_error = Some("p2p node not started".to_string()),
        }
        Ok(NodeCardImport {
            peer_id: parsed.peer_id,
            has_recovery_token: parsed.recovery_token.is_some(),
            connect_error,
        })
    }

    /// 广播任意 pubsub 消息（ipc/p2p.ts `p2p-broadcast`）：body 原样进信封
    /// （version/evidenceHeadHash/timestamp/pubKey/signature 由节点补充）。
    /// spark-sync 的 update/delete 消息体构造用 `build_update_body` /
    /// `build_delete_body`（doc_* 写路径内部已走该组合）。p2p 未启动报
    /// `NotStarted`（TS `p2p node not started`）。
    pub fn p2p_broadcast(&self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let node = self.p2p.as_ref().ok_or(P2pError::NotStarted)?;
        self.runtime
            .handle()
            .block_on(node.broadcast(topic, body))?;
        Ok(())
    }

    /// 订阅 P2P 事件流（壳层消费；慢订阅者收到 `Lagged` 表示丢事件）。
    pub fn subscribe_p2p_events(&self) -> broadcast::Receiver<P2pEvent> {
        self.event_tx.subscribe()
    }

    // ------------------------------------------------------------------
    // 节点活跃度记录（ipc/p2p.ts 测试页通道）
    // ------------------------------------------------------------------

    /// `p2p-clear-peer-records`（ipc/p2p.ts:100-107）：清空节点活跃度记录，
    /// 返回删除条数（测试页快速重置用）。
    pub fn clear_peer_records(&self) -> Result<u64> {
        let mut storage = self.require_storage()?.clone();
        let mut store = PeerActivityStore::new(&mut storage);
        Ok(store.clear_all_records()? as u64)
    }

    /// 列出全部节点活跃度记录的原始键值对（`p2p:peer:record:` 前缀，
    /// 值为序列化 JSON 字符串）。壳层测试页邻居列表用——对齐 TS 测试页
    /// `db.query('p2p:peer:record:')` 的读法，避免向渲染端暴露裸 KV。
    pub fn list_peer_records(&self) -> Result<Vec<(String, String)>> {
        let storage = self.require_storage()?;
        Ok(storage.scan(&ScanOptions::prefix(P2P_PEER_RECORD_PREFIX))?)
    }

    /// `db-scan`：按存储键前缀扫描，返回原始键值对（测试页 peer 目录用——
    /// 聚合邻居池 `p2p:overlay:peer:`、联系人 `ct:friend:`、优先类目表
    /// `p2p:priority:peer:` 等前缀）。只读操作，不暴露裸 KV 之外的任何能力。
    pub fn scan_storage_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let storage = self.require_storage()?;
        Ok(storage.scan(&ScanOptions::prefix(prefix))?)
    }
}
