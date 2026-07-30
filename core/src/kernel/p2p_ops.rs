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
use crate::p2p::{DhtMode, LocalP2PNodeInfo, P2pConfig, P2pError, P2pEvent, P2pNode, PeerNodeInfo};
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
        if let Some(node) = &self.p2p {
            return Ok(node.peer_id().to_string());
        }
        let storage = self.require_storage()?.clone();
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
            storage: storage.clone(),
            current_root_id: Arc::clone(&self.current_root_id_shared),
            collection_configs: Arc::clone(&self.collection_configs),
            org_acks: Arc::clone(&self.org_acks),
            push_notify: org_sync_tx.clone(),
            event_tx: self.event_tx.clone(),
            nickname_shared: Arc::clone(&self.nickname_shared),
            avatar_shared: Arc::clone(&self.avatar_shared),
            node_shared: Arc::clone(&self.p2p_node_shared),
            signing_key_shared: Arc::clone(&self.signing_key_shared),
            io_lock: Arc::clone(&self.io_lock),
        });
        let mut node =
            self.runtime
                .handle()
                .block_on(P2pNode::start(config, storage.clone(), host))?;
        let peer_id = node.peer_id().to_string();
        self.p2p_start_error = None;
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
        };
        let worker = org_sync::spawn_worker(self.runtime.handle(), ctx, org_sync_rx);

        // 事件泵：node 事件流 → kernel 广播通道（壳层订阅）；
        // KeepaliveTick 拦截为组织保活触发（覆盖网维护已在事件循环内完成）
        let tx = self.event_tx.clone();
        let org_tx = org_sync_tx.clone();
        let pump = self.runtime.handle().spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, P2pEvent::KeepaliveTick(_)) {
                    let _ = org_tx.send(OrgSyncRequest::KeepaliveTick);
                }
                // 无订阅者时忽略发送失败
                let _ = tx.send(event);
            }
        });
        self.p2p = Some(node);
        self.p2p_started_at = Some(system_now_ms());
        self.p2p_pump = Some(pump);
        self.org_sync_worker = Some(worker);
        self.org_sync_tx = Some(org_sync_tx);
        Ok(peer_id)
    }

    /// 停止 P2P 节点（幂等）：org-sync worker / 事件泵一并停止。
    pub fn stop_p2p(&mut self) -> Result<()> {
        self.org_sync_tx = None;
        *self.p2p_node_shared.lock().unwrap() = None;
        if let Some(worker) = self.org_sync_worker.take() {
            worker.abort();
        }
        if let Some(pump) = self.p2p_pump.take() {
            pump.abort();
        }
        if let Some(node) = self.p2p.take() {
            self.runtime.handle().block_on(node.stop());
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
}
