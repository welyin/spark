//! kernel 门面：把内核各模块组装为壳层（Tauri）可调用的单一对象。
//!
//! - 生命周期：[`Kernel::init`]（数据目录、活动身份的 sled 打开、身份管理器就绪）
//!   / [`Kernel::shutdown`]（停 P2P、flush 存储）；
//! - 身份：见 [`identity`] 子模块（目录结构 `{data_dir}/identities/{rootId}.json`
//!   + `active-identity.json`，与 TS `RootIdentityManager` 对齐）；
//! - 文档：`doc_*` 走 `collection` 模块的本地写入路径（doc/meta/索引/存证同 batch）；
//! - 组织/数据治理：委托 `org` / `data_mgmt` 服务层，副本概览按 ipc/data.ts 接线；
//! - P2P：内部 tokio runtime 托管 [`P2pNode`]，事件经 `tokio::sync::broadcast`
//!   通道外发（[`Kernel::subscribe_p2p_events`]）。
//!
//! 时间戳一律内核内部 `SystemTime` 取（壳层不再注入 `now_ms`；各模块的 `now_ms`
//! 参数在内部转发）。
//!
//! 线程模型：全部 API 为同步方法；**不得**在 tokio runtime 线程内调用（内部以
//! `Handle::block_on` 驱动 P2P，嵌套 runtime 会 panic）——Tauri 侧请用同步
//! command 或 `spawn_blocking` 调用。

mod error;
mod host;
mod identity;
mod org_sync;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::broadcast;

pub use error::{KernelError, Result};
pub use identity::{
    DerivedDomainIdentityInfo, DomainSignatureInfo, IdentityStatus, IdentitySummary,
    InitIdentityResult, MnemonicCheckInfo, ProfileInfo, PublicIdentity, RootSignatureInfo,
};
pub use org_sync::{OrgReconcileStats, PeerOrgSyncResult};

use crate::collection::{CollectionConfig, DocumentCollection, QueryOptions, QueryResult};
use crate::data_mgmt::service::ReplicaStatus;
use crate::data_mgmt::{
    AutoCleanupResult, DataManagementService, DataUsageReport, ExportWriteResult, PurgePreview,
    PurgeResult, write_export_dump,
};
use crate::evidence::{
    EvidenceEntry, get_evidence_entry, get_evidence_head_hash, get_evidence_height,
    verify_evidence_chain,
};
use crate::org::service::{CreateOrganizationInput, CreatedOrgInvite, InviteAcceptance};
use crate::org::sync_state::{OrgSyncState, org_sync_state_key, sync_state_after_pull_synced};
use crate::org::{
    OrgInvitePayload, OrgSyncOverview, OrganizationNodeInfo, OrganizationService,
    OrganizationView, PluginDocSyncItem, apply_plugin_doc_sync_items,
    build_organization_sync_versions_default, compute_org_sync_overview, sign_node_info_claim,
};
use crate::p2p::constants::{P2P_PEER_RECORD_PREFIX, SYNC_TOPIC};
use crate::p2p::keepalive::RecoveryTrigger;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_activity::PeerActivityStore;
use crate::p2p::{
    LocalP2PNodeInfo, P2pConfig, P2pError, P2pEvent, P2pNode, PeerNodeInfo, build_delete_body,
    build_update_body, extract_peer_id,
};
use crate::schema::{CollectionSchemaDeclaration, CollectionSchemaRecord, declare_collection_schema};
use crate::storage::{ScanOptions, SledStorage, StorageBackend};

use host::{CollectionConfigs, KernelHost, SharedOrgShareAckTracker};
use org_sync::{OrgSyncContext, OrgSyncRequest};

/// 事件通道容量（慢订阅者丢旧事件，`broadcast::RecvError::Lagged` 上报）。
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// kernel 配置。
#[derive(Clone)]
pub struct KernelConfig {
    /// 应用数据目录（壳层给定，等价 Electron `app.getPath('userData')`）。
    pub data_dir: PathBuf,
    /// 应用版本（p2p `/spark/version/1.0.0` 响应）。
    pub app_version: String,
    /// p2p 节点配置覆盖；`None` 使用默认（app_version 注入）。
    pub p2p: Option<P2pConfig>,
}

/// 已解锁身份（仅内存；助记词不入内存，查看走 `reveal_mnemonic` 密码门控）。
///
/// 会话解密态（对齐 TS `UnlockedRootIdentity` 持有的 `seed`）：
/// - `seed`：BIP39 种子，域身份派生（`derive_domain_identity`）的唯一来源；
/// - `password`：会话口令，免密码资料更新（`update_profile_session`）重封
///   加密 payload 的 KDF 输入。
///
/// 两者随 `lock` 清除。
pub(crate) struct UnlockedIdentity {
    pub(crate) identity: crate::identity::Identity,
    pub(crate) seed: [u8; 64],
    pub(crate) password: String,
}

impl UnlockedIdentity {
    pub(crate) fn root_id(&self) -> String {
        self.identity.id()
    }
}

/// `data-purge-preview` 的返回（ipc/data.ts:73-86 的形状）。
#[derive(Clone, Debug)]
pub struct PurgePreviewInfo {
    /// 组织 id。
    pub org_id: String,
    /// 组织基础插件域。
    pub domain: String,
    /// 清理阈值时间戳（ms）。
    pub before_ts: i64,
    /// 影响面预览。
    pub preview: PurgePreview,
    /// K 副本概览（P2P 未启动为 `None`）。
    pub replica: Option<OrgSyncOverview>,
    /// 当前用户是否该组织管理员。
    pub is_current_user_admin: bool,
}

/// `evidence-verify` 的返回（链校验结果与高度）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EvidenceChainStatus {
    /// 链完整性（逐条验 prevHash 与重算 hash）。
    pub valid: bool,
    /// 链高（空链 0）。
    pub height: u64,
}

/// kernel 门面：壳层持有的单例。
pub struct Kernel {
    pub(crate) config: KernelConfig,
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) storage: Option<SledStorage>,
    /// 当前存储目录所属身份。
    pub(crate) storage_root_id: Option<String>,
    pub(crate) unlocked: Option<UnlockedIdentity>,
    pub(crate) data_mgmt: Option<DataManagementService>,
    /// p2p 节点（`Arc`：org-sync worker 与门面方法共享命令句柄）。
    pub(crate) p2p: Option<Arc<P2pNode>>,
    pub(crate) p2p_pump: Option<tokio::task::JoinHandle<()>>,
    /// org-sync worker（推送/保活串行队列），随 p2p 起停。
    pub(crate) org_sync_worker: Option<tokio::task::JoinHandle<()>>,
    /// org-sync 请求队列的发送端（p2p 运行期存在；host 与门面触发推送用）。
    pub(crate) org_sync_tx: Option<tokio::sync::mpsc::UnboundedSender<OrgSyncRequest>>,
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    /// p2p 宿主可见的当前身份指针（事件循环线程共享）。
    pub(crate) current_root_id_shared: Arc<Mutex<Option<String>>>,
    /// 解锁期签名私钥（org-sync worker 自签 nodeInfoClaim 用；lock 时清除）。
    pub(crate) signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    /// org-share-ack 等待器注册表（host 与 worker 共享）。
    pub(crate) org_acks: SharedOrgShareAckTracker,
    /// org-recovery 触发器（跨 tick 状态：连续失联计数 + 全局冷却）。
    pub(crate) recovery_trigger: Arc<Mutex<RecoveryTrigger>>,
    /// doc_* 调用登记的集合配置（远端应用的索引维护依据，见 host.rs）。
    pub(crate) collection_configs: CollectionConfigs,
}

impl Kernel {
    /// 初始化内核：建数据目录、迁移遗留身份、按活动身份打开 sled 存储。
    pub fn init(config: KernelConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut kernel = Kernel {
            config,
            runtime,
            storage: None,
            storage_root_id: None,
            unlocked: None,
            data_mgmt: None,
            p2p: None,
            p2p_pump: None,
            org_sync_worker: None,
            org_sync_tx: None,
            event_tx,
            current_root_id_shared: Arc::new(Mutex::new(None)),
            signing_key_shared: Arc::new(Mutex::new(None)),
            org_acks: Arc::new(Mutex::new(Default::default())),
            recovery_trigger: Arc::new(Mutex::new(RecoveryTrigger::new())),
            collection_configs: Arc::new(Mutex::new(HashMap::new())),
        };
        kernel.migrate_legacy_identity_if_needed()?;
        if let Some(root_id) = kernel.read_active_root_id()? {
            kernel.open_storage(&root_id)?;
            *kernel.current_root_id_shared.lock().unwrap() = Some(root_id);
        }
        Ok(kernel)
    }

    /// 关闭内核：停 P2P、停数据治理、flush 并释放存储（sled 文件锁随之释放）。
    /// 幂等；调用后门面进入惰性状态（storage 为 None，不可再业务调用）。
    pub fn shutdown(&mut self) -> Result<()> {
        self.stop_p2p()?;
        if let Some(dm) = &mut self.data_mgmt {
            dm.stop();
        }
        if let Some(storage) = self.storage.take() {
            storage.flush()?;
            // 句柄随 take 丢弃：p2p 已停，此为最后引用，sled 锁立即释放
        }
        self.storage_root_id = None;
        self.data_mgmt = None;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 存储对齐（TS bootstrap.ts `ensureStorageMatchesIdentity`）
    // ------------------------------------------------------------------

    /// 每身份一个存储目录名（TS `spark-leveldb-{rootId16}`；引擎换 sled 故改名）。
    fn sled_dir_name(root_id: &str) -> String {
        let prefix: String = root_id.chars().take(16).collect();
        format!("spark-sled-{prefix}")
    }

    /// 当前打开的存储目录（诊断用；未打开为 `None`）。
    pub fn storage_dir(&self) -> Option<PathBuf> {
        self.storage_root_id
            .as_ref()
            .map(|rid| self.config.data_dir.join(Self::sled_dir_name(rid)))
    }

    /// 打开指定身份的存储并启动数据治理服务（调用方负责先停 P2P）。
    fn open_storage(&mut self, root_id: &str) -> Result<()> {
        let dir = self.config.data_dir.join(Self::sled_dir_name(root_id));
        let storage = SledStorage::open(&dir)?;
        let mut dm = DataManagementService::new(Some(dir.to_string_lossy().into_owned()));
        dm.start();
        self.storage = Some(storage);
        self.storage_root_id = Some(root_id.to_string());
        self.data_mgmt = Some(dm);
        Ok(())
    }

    /// 存储对齐：身份切换时先停 P2P/治理，flush 旧库，再指向新身份的库目录。
    pub(crate) fn align_storage(&mut self, root_id: &str) -> Result<()> {
        if self.storage_root_id.as_deref() == Some(root_id) {
            return Ok(());
        }
        self.stop_p2p()?;
        if let Some(dm) = &mut self.data_mgmt {
            dm.stop();
        }
        if let Some(storage) = &self.storage {
            storage.flush()?;
        }
        self.open_storage(root_id)
    }

    pub(crate) fn require_storage(&self) -> Result<&SledStorage> {
        self.storage.as_ref().ok_or(KernelError::StorageNotReady)
    }

    pub(crate) fn require_storage_mut(&mut self) -> Result<&mut SledStorage> {
        self.storage.as_mut().ok_or(KernelError::StorageNotReady)
    }

    // ------------------------------------------------------------------
    // 文档/集合 API（collection 本地写入路径）
    // ------------------------------------------------------------------

    fn make_collection(
        &self,
        domain: &str,
        collection: &str,
        config: &CollectionConfig,
    ) -> DocumentCollection {
        self.collection_configs
            .lock()
            .unwrap()
            .insert((domain.to_string(), collection.to_string()), config.clone());
        DocumentCollection::new(domain, collection, config.clone())
    }

    /// 本地写入节点 id：p2p 运行中为 peerId，否则 `local-node`（对齐 TS）。
    fn sync_node_id(&self) -> String {
        self.p2p
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| "local-node".to_string())
    }

    /// 声明集合同步策略（幂等；一旦声明不可变更）。
    pub fn declare_collection(
        &mut self,
        domain: &str,
        collection: &str,
        declaration: CollectionSchemaDeclaration,
    ) -> Result<CollectionSchemaRecord> {
        let record = declare_collection_schema(
            self.require_storage_mut()?,
            domain,
            collection,
            &declaration,
            system_now_ms(),
        )?;
        Ok(record)
    }

    /// 读文档；不存在返回 `Ok(None)`。
    pub fn doc_get(&self, domain: &str, collection: &str, id: &str) -> Result<Option<Value>> {
        let coll = DocumentCollection::new(domain, collection, CollectionConfig::default());
        Ok(coll.get(self.require_storage()?, id)?)
    }

    /// 写文档：doc + 索引 diff + meta + 存证同 batch；随后经 p2p 广播
    /// `update`（未启动或广播失败不影响本地写入，对齐 TS 的非阻塞语义）。
    pub fn doc_put(
        &mut self,
        domain: &str,
        collection: &str,
        id: &str,
        doc: Value,
        config: CollectionConfig,
    ) -> Result<()> {
        let coll = self.make_collection(domain, collection, &config);
        let node_id = self.sync_node_id();
        let write = coll.put(self.require_storage_mut()?, id, &doc, &node_id, system_now_ms())?;
        let body = build_update_body(
            domain,
            collection,
            id,
            doc,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        self.broadcast_sync_body(body);
        Ok(())
    }

    /// 删文档：删 doc/索引 + 墓碑 meta + 存证同 batch；广播 `delete`。
    /// 返回文档是否存在过（TS `delete` 对不存在文档为空操作）。
    pub fn doc_delete(
        &mut self,
        domain: &str,
        collection: &str,
        id: &str,
        config: CollectionConfig,
    ) -> Result<bool> {
        let coll = self.make_collection(domain, collection, &config);
        let node_id = self.sync_node_id();
        let Some(write) = coll.delete(self.require_storage_mut()?, id, &node_id, system_now_ms())?
        else {
            return Ok(false);
        };
        let body = build_delete_body(
            domain,
            collection,
            id,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        self.broadcast_sync_body(body);
        Ok(true)
    }

    /// 查询集合（索引/主键分页 + 内存 filter；TS `DocumentCollection.query`）。
    pub fn doc_query(
        &self,
        domain: &str,
        collection: &str,
        config: CollectionConfig,
        options: QueryOptions,
    ) -> Result<QueryResult> {
        let coll = self.make_collection(domain, collection, &config);
        Ok(coll.query(self.require_storage()?, &options)?)
    }

    /// 广播同步消息：p2p 未启动直接跳过；失败降级为事件流告警（TS console.warn）。
    fn broadcast_sync_body(&self, body: Map<String, Value>) {
        let Some(node) = &self.p2p else {
            return;
        };
        if let Err(e) = self.runtime.handle().block_on(node.broadcast(SYNC_TOPIC, body)) {
            let _ = self
                .event_tx
                .send(P2pEvent::Warning(format!("sync broadcast failed: {e}")));
        }
    }

    // ------------------------------------------------------------------
    // 组织 API（委托 org::OrganizationService）
    // ------------------------------------------------------------------

    /// 当前用户为成员的组织视图列表（`listMine`，updatedAt 降序）。
    pub fn list_orgs(&self) -> Result<Vec<OrganizationView>> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::list_mine(self.require_storage()?, &root_id)?)
    }

    /// 创建组织（需要已解锁身份）：创建者为唯一初始 admin。
    pub fn create_org(&mut self, input: CreateOrganizationInput) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::create_organization(
            self.require_storage_mut()?,
            &input,
            &root_id,
            system_now_ms(),
        )?;
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 生成组织邀请码（仅 admin；需要 p2p 运行以携带本机节点信息，
    /// 否则报"本机 P2P 节点尚未启动"）。
    pub fn create_org_invite(&self, org_id: &str) -> Result<CreatedOrgInvite> {
        let root_id = self.require_unlocked_root_id()?;
        let (peer_id, addresses) = match &self.p2p {
            Some(node) => {
                let info = self.runtime.handle().block_on(node.local_node_info())?;
                (info.peer_id, info.addresses)
            }
            None => (None, Vec::new()),
        };
        Ok(OrganizationService::create_org_invite(
            self.require_storage()?,
            org_id,
            &root_id,
            peer_id.as_deref(),
            &addresses,
            system_now_ms(),
        )?)
    }

    /// 接受邀请码的纯逻辑部分：解码校验 + 拒绝自邀；返回邀请载荷
    /// （邀请人 rootId/peerId/addresses 供壳层连接拉取，随后以
    /// [`Kernel::check_join`] 做落库确认）。
    pub fn join_by_invite(&self, code: &str) -> Result<OrgInvitePayload> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::prepare_accept_invite(code, &root_id, system_now_ms())?)
    }

    /// `acceptOrgInvite` 的落库确认：拉取完成后本地已有成员记录才算加入成功。
    pub fn check_join(&self, org_id: &str) -> Result<InviteAcceptance> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::check_invite_accepted(
            self.require_storage()?,
            org_id,
            &root_id,
        )?)
    }

    /// 添加组织成员（仅 admin；重复添加 = 更新 nodeInfo，service.ts:216-309）。
    ///
    /// 落库后经 org-sync worker 向已知成员推送快照（尽力而为，成员离线仅
    /// 告警——对齐 service.ts `syncOrganizationToKnownMembers` 的预录模型；
    /// p2p 未启动时跳过推送，其他成员经后续反熵获得变更）。
    pub fn org_add_member(
        &mut self,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::add_member(
            self.require_storage_mut()?,
            org_id,
            member_root_id,
            node_info,
            &root_id,
            system_now_ms(),
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
                actor_root_id: root_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 移除组织成员（仅 admin；移除 admin 时组织至少保留 1 名 admin，
    /// service.ts:460-498）。TS 移除路径**不推送**（成员经 org-pull 的
    /// `removed` 状态传播剔除），本方法同样只落库。
    pub fn org_remove_member(
        &mut self,
        org_id: &str,
        member_root_id: &str,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::remove_member(
            self.require_storage_mut()?,
            org_id,
            member_root_id,
            &root_id,
            system_now_ms(),
        )?;
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 删除组织（仅 admin，service.ts:199-214）。只落库不推送（对齐 TS——
    /// 删除经 org-pull 的 `removed` 状态传播）。
    pub fn org_delete(&mut self, org_id: &str) -> Result<()> {
        let root_id = self.require_unlocked_root_id()?;
        OrganizationService::delete_organization(
            self.require_storage_mut()?,
            org_id,
            &root_id,
            system_now_ms(),
        )?;
        Ok(())
    }

    /// `acceptOrgInvite` 编排（service.ts:345-374 + org-pull-sync.ts 的受邀组织段）：
    /// 解码邀请码 → 连邀请人 → org-pull-list（捎带自签 nodeInfoClaim，供管理员回填
    /// 本机地址）→ org-pull-org 拉取受邀组织 → 快照落库（含 pluginDocs 与副本记账）
    /// → 成员确认。
    ///
    /// 与 TS 的差异：TS 的 `connectAndPull` 是一次全量反熵（协调双方全部共同组织），
    /// 本方法按加入语义只拉取受邀组织；其他共同组织的协调留给组织 keepalive 编排
    /// （阶段③后续）。
    ///
    /// 需要已解锁身份（claim 签名）与运行中的 P2P（否则报 TS 文案
    /// "P2P 网络未启动，无法通过邀请码加入"）；邀请人连接失败按 TS
    /// `connectPeer` 文案报错；拉取无响应/非成员按 TS 路径降级为
    /// [`OrganizationService::check_invite_accepted`] 的"未能加入组织"错误。
    pub fn accept_invite(&mut self, code: &str) -> Result<InviteAcceptance> {
        let root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let payload = OrganizationService::prepare_accept_invite(code, &root_id, now)?;
        let inviter = PeerNodeInfo {
            peer_id: payload.inviter.peer_id.clone(),
            addresses: payload.inviter.addresses.clone(),
        };

        if self.p2p.is_none() {
            return Err(KernelError::Message(
                "P2P 网络未启动，无法通过邀请码加入".to_string(),
            ));
        }
        let node = self.p2p.as_ref().expect("p2p checked above");
        let local = self.runtime.handle().block_on(node.local_node_info())?;

        // 自签 nodeInfoClaim（bootstrap.ts `buildSelfNodeInfoClaim`）：随首次 pull
        // 捎带，供管理员回填本机节点地址并经 gossip 扩散
        let claim = sign_node_info_claim(
            &self.unlocked.as_ref().expect("unlocked checked above").identity.signing_key,
            OrganizationNodeInfo {
                peer_id: local.peer_id.clone(),
                addresses: local.addresses.clone(),
            },
            now,
        );

        // 连接失败按 TS `connectPeer` 文案中断（service.ts 不再继续拉取）
        self.runtime
            .handle()
            .block_on(node.connect_peer(&inviter))
            .map_err(|e| {
                KernelError::Message(format!("Failed to connect peer by provided addresses: {e}"))
            })?;

        // org-pull-list：本流程只借其捎带 claim 的副作用（管理员侧回填），
        // 响应体不消费；失败不中断（对齐 TS requestDirect 的 null 降级）
        let mut list_payload = Map::new();
        list_payload.insert("requesterRootId".to_string(), Value::from(root_id.clone()));
        if let Some(peer) = &local.peer_id {
            list_payload.insert("requesterPeerId".to_string(), Value::from(peer.clone()));
        }
        list_payload.insert("nodeInfoClaim".to_string(), serde_json::to_value(&claim)?);
        let mut list_request = Map::new();
        list_request.insert("type".to_string(), Value::from("org-pull-list"));
        list_request.insert("payload".to_string(), Value::Object(list_payload));
        let _ = self.runtime.handle().block_on(
            node.org_pull_request(&inviter, &Value::Object(list_request).to_string()),
        );

        // org-pull-org：拉取受邀组织；无响应/非成员均降级为末尾的成员确认错误
        let mut org_payload = Map::new();
        org_payload.insert("requesterRootId".to_string(), Value::from(root_id.clone()));
        if let Some(peer) = &local.peer_id {
            org_payload.insert("requesterPeerId".to_string(), Value::from(peer.clone()));
        }
        org_payload.insert("orgId".to_string(), Value::from(payload.org_id.clone()));
        let mut org_request = Map::new();
        org_request.insert("type".to_string(), Value::from("org-pull-org"));
        org_request.insert("payload".to_string(), Value::Object(org_payload));
        let response = self
            .runtime
            .handle()
            .block_on(node.org_pull_request(&inviter, &Value::Object(org_request).to_string()))
            .ok()
            .flatten();

        if let Some(response) = response {
            let ok = response.get("ok").and_then(Value::as_bool) == Some(true);
            let status = response.get("status").and_then(Value::as_str);
            let organization = response.get("organization").filter(|v| !v.is_null());
            if ok && status == Some("member") && let Some(organization) = organization {
                let now = system_now_ms();
                let merged = OrganizationService::apply_incoming_snapshot(
                    self.require_storage_mut()?,
                    organization,
                    now,
                )?;
                // pluginDocs 随快照捎带（plugin-org-sync.ts `applyPluginDocSyncItems`；
                // 集合适配器取 doc_* 登记的索引配置，未登记按无索引处理——同 host.rs）
                if let Some(docs) = response.get("pluginDocs").and_then(Value::as_array) {
                    let items: Vec<PluginDocSyncItem> = docs
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect();
                    let configs = self.collection_configs.clone();
                    apply_plugin_doc_sync_items(
                        self.require_storage_mut()?,
                        &items,
                        |domain, collection| {
                            let config = configs
                                .lock()
                                .unwrap()
                                .get(&(domain.to_string(), collection.to_string()))
                                .cloned()
                                .unwrap_or_default();
                            DocumentCollection::new(domain, collection, config)
                        },
                        now,
                    )?;
                }
                // 副本记账（org-pull-sync.ts `recordPullSyncState`）
                if let Some(peer_id) = extract_peer_id(&inviter) {
                    let versions = merged
                        .sync
                        .as_ref()
                        .map(|sync| sync.versions)
                        .unwrap_or_else(|| build_organization_sync_versions_default(&merged));
                    let state = sync_state_after_pull_synced(versions, now);
                    self.require_storage_mut()?.put(
                        &org_sync_state_key(&peer_id, &merged.org_id),
                        &state.to_json(),
                    )?;
                }
            }
        }

        self.check_join(&payload.org_id)
    }

    /// 组织 K 副本统计（`getOrgSyncOverview` 纯逻辑版）。
    pub fn org_overview(&self, org_id: &str) -> Result<OrgSyncOverview> {
        let storage = self.require_storage()?;
        let record = OrganizationService::get_record(storage, org_id)?
            .ok_or(crate::org::OrgError::OrganizationNotFound)?;
        let current_root_id = self.current_root_id()?;
        let versions = record
            .sync
            .as_ref()
            .map(|sync| sync.versions)
            .or_else(|| Some(build_organization_sync_versions_default(&record)));
        let overview = compute_org_sync_overview(
            org_id,
            &record.members,
            current_root_id.as_deref(),
            versions.as_ref(),
            |peer_id| {
                storage
                    .get(&org_sync_state_key(peer_id, org_id))
                    .ok()
                    .flatten()
                    .and_then(|raw| OrgSyncState::from_json(&raw))
            },
            system_now_ms(),
        );
        Ok(overview)
    }

    // ------------------------------------------------------------------
    // 数据治理 API（委托 data_mgmt）
    // ------------------------------------------------------------------

    /// 数据用量统计（缓存优先；含磁盘信息）。
    pub fn get_usage(&mut self) -> Result<DataUsageReport> {
        let storage = self.storage.as_ref().ok_or(KernelError::StorageNotReady)?;
        let dm = self.data_mgmt.as_mut().ok_or(KernelError::StorageNotReady)?;
        Ok(dm.get_usage(storage, system_now_ms())?)
    }

    /// 立即执行 L1 自动清理。
    pub fn run_cleanup_now(&mut self) -> Result<AutoCleanupResult> {
        let storage = self.storage.as_mut().ok_or(KernelError::StorageNotReady)?;
        let dm = self.data_mgmt.as_mut().ok_or(KernelError::StorageNotReady)?;
        Ok(dm.run_cleanup_now(storage, system_now_ms()))
    }

    /// 全库导出（紧凑 JSON 写文件）。
    pub fn export_dump(&self, file_path: impl AsRef<std::path::Path>) -> Result<ExportWriteResult> {
        Ok(write_export_dump(self.require_storage()?, file_path, system_now_ms())?)
    }

    /// 解析目标组织（ipc/data.ts `resolveOrg`）：必须存在且带基础插件域。
    fn resolve_org(&self, org_id: &str) -> Result<(OrganizationView, String)> {
        let view = self
            .list_orgs()?
            .into_iter()
            .find(|item| item.record.org_id == org_id)
            .ok_or_else(|| {
                KernelError::Message("Organization not found or not a member".to_string())
            })?;
        let domain = view.record.base_plugin_domain.clone();
        if domain.is_empty() {
            return Err(KernelError::Message(format!(
                "Organization {org_id} has no base plugin domain; cannot locate its data domain"
            )));
        }
        Ok((view, domain))
    }

    /// purge 预览（不鉴权管理员，对齐 TS；管理员标记随结果返回供壳层判断）。
    pub fn preview_purge(&self, org_id: &str, before_ts: i64) -> Result<PurgePreviewInfo> {
        let (view, domain) = self.resolve_org(org_id)?;
        let preview = self
            .data_mgmt
            .as_ref()
            .ok_or(KernelError::StorageNotReady)?
            .preview_purge(self.require_storage()?, &domain, before_ts)?;
        let replica = if self.p2p.is_some() {
            Some(self.org_overview(org_id)?)
        } else {
            None
        };
        Ok(PurgePreviewInfo {
            org_id: org_id.to_string(),
            domain,
            before_ts,
            preview,
            replica,
            is_current_user_admin: view.is_current_user_admin,
        })
    }

    /// purge 执行：管理员 → 导出确认 → P2P 启动 → 副本充足 → in-flight，
    /// 校验顺序与错误文案对齐 ipc/data.ts。
    pub fn execute_purge(
        &mut self,
        org_id: &str,
        before_ts: i64,
        confirm_exported: bool,
    ) -> Result<PurgeResult> {
        let (view, domain) = self.resolve_org(org_id)?;
        let replica = if self.p2p.is_some() {
            let overview = self.org_overview(org_id)?;
            Some(ReplicaStatus {
                synced_peers: overview.synced_peers,
                replica_target: overview.replica_target,
            })
        } else {
            None
        };
        let storage = self.storage.as_mut().ok_or(KernelError::StorageNotReady)?;
        let dm = self.data_mgmt.as_mut().ok_or(KernelError::StorageNotReady)?;
        Ok(dm.execute_purge(
            storage,
            &domain,
            before_ts,
            confirm_exported,
            view.is_current_user_admin,
            replica,
            system_now_ms(),
        )?)
    }

    // ------------------------------------------------------------------
    // 存证 API（委托 evidence 模块；ipc/db.ts evidence-* 通道）
    // ------------------------------------------------------------------

    /// 存证链头 hash（`evidence-head-hash`；空链为 `Ok(None)`）。
    pub fn evidence_head_hash(&self) -> Result<Option<String>> {
        Ok(get_evidence_head_hash(self.require_storage()?)?)
    }

    /// 链校验 + 高度（`evidence-verify`）。
    pub fn evidence_verify(&self) -> Result<EvidenceChainStatus> {
        let storage = self.require_storage()?;
        Ok(EvidenceChainStatus {
            valid: verify_evidence_chain(storage)?,
            height: get_evidence_height(storage)?,
        })
    }

    /// 按 seq 取存证条目（不存在返回 `Ok(None)`）。
    pub fn evidence_entry(&self, seq: u64) -> Result<Option<EvidenceEntry>> {
        Ok(get_evidence_entry(self.require_storage()?, seq)?)
    }

    // ------------------------------------------------------------------
    // P2P API
    // ------------------------------------------------------------------

    /// 启动 P2P 节点（内部 tokio runtime 托管；幂等，重复调用返回现有 peerId）。
    /// 需要存储已打开（libp2p 身份/端口/邻居表持久化在库内）。
    ///
    /// 同时装配：事件泵（node 事件 → kernel 广播通道，`KeepaliveTick` 拦截为
    /// 组织保活触发）与 org-sync worker（推送/保活串行队列，org_sync.rs）。
    pub fn start_p2p(&mut self) -> Result<String> {
        if let Some(node) = &self.p2p {
            return Ok(node.peer_id().to_string());
        }
        let storage = self.require_storage()?.clone();
        let config = self.config.p2p.clone().unwrap_or_else(|| P2pConfig {
            app_version: self.config.app_version.clone(),
            ..Default::default()
        });
        let (org_sync_tx, org_sync_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = Box::new(KernelHost {
            storage: storage.clone(),
            current_root_id: Arc::clone(&self.current_root_id_shared),
            collection_configs: Arc::clone(&self.collection_configs),
            org_acks: Arc::clone(&self.org_acks),
            push_notify: org_sync_tx.clone(),
        });
        let mut node = self
            .runtime
            .handle()
            .block_on(P2pNode::start(config, storage.clone(), host))?;
        let peer_id = node.peer_id().to_string();
        let mut events = node.take_events();
        let node = Arc::new(node);

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
        };
        let worker = self.runtime.handle().spawn(async move {
            let mut rx = org_sync_rx;
            while let Some(request) = rx.recv().await {
                match request {
                    OrgSyncRequest::PushOrg {
                        org_id,
                        actor_root_id,
                    } => ctx.push_org_to_known_members(&org_id, &actor_root_id).await,
                    OrgSyncRequest::KeepaliveTick => ctx.maintain_org_tick().await,
                }
            }
        });

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
        self.p2p_pump = Some(pump);
        self.org_sync_worker = Some(worker);
        self.org_sync_tx = Some(org_sync_tx);
        Ok(peer_id)
    }

    /// 停止 P2P 节点（幂等）：org-sync worker / 事件泵一并停止。
    pub fn stop_p2p(&mut self) -> Result<()> {
        self.org_sync_tx = None;
        if let Some(worker) = self.org_sync_worker.take() {
            worker.abort();
        }
        if let Some(pump) = self.p2p_pump.take() {
            pump.abort();
        }
        if let Some(node) = self.p2p.take() {
            self.runtime.handle().block_on(node.stop());
        }
        Ok(())
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
        })
    }

    /// P2P 是否运行中。
    pub fn p2p_running(&self) -> bool {
        self.p2p.is_some()
    }

    /// P2P 状态快照（未启动返回 `Ok(None)`）。
    pub fn p2p_status(&self) -> Result<Option<LocalP2PNodeInfo>> {
        match &self.p2p {
            None => Ok(None),
            Some(node) => Ok(Some(self.runtime.handle().block_on(node.local_node_info())?)),
        }
    }

    /// 广播任意 pubsub 消息（ipc/p2p.ts `p2p-broadcast`）：body 原样进信封
    /// （version/evidenceHeadHash/timestamp/pubKey/signature 由节点补充）。
    /// spark-sync 的 update/delete 消息体构造用 `build_update_body` /
    /// `build_delete_body`（doc_* 写路径内部已走该组合）。p2p 未启动报
    /// `NotStarted`（TS `p2p node not started`）。
    pub fn p2p_broadcast(&self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let node = self.p2p.as_ref().ok_or(P2pError::NotStarted)?;
        self.runtime.handle().block_on(node.broadcast(topic, body))?;
        Ok(())
    }

    /// 订阅 P2P 事件流（壳层消费；慢订阅者收到 `Lagged` 表示丢事件）。
    pub fn subscribe_p2p_events(&self) -> broadcast::Receiver<P2pEvent> {
        self.event_tx.subscribe()
    }

    // ------------------------------------------------------------------
    // 组织同步编排 API（org_sync.rs；ipc/p2p.ts 对齐）
    // ------------------------------------------------------------------

    /// 向指定成员推送组织快照（org-share-sync.ts `syncOrganizationToMember`：
    /// stale 跳过 → 直连优先 → pubsub 五次重试等 ack → sync-state 记账）。
    ///
    /// p2p 未启动报 `p2p node not started`；全部重试失败报
    /// `Organization sync ack timeout: ...`（TS 同文案）。
    pub fn sync_org_to_member(
        &self,
        node_info: &OrganizationNodeInfo,
        target_root_id: &str,
        org_id: &str,
    ) -> Result<()> {
        let ctx = self.org_sync_context().ok_or(P2pError::NotStarted)?;
        let peer = PeerNodeInfo {
            peer_id: node_info.peer_id.clone(),
            addresses: node_info.addresses.clone(),
        };
        self.runtime
            .handle()
            .block_on(ctx.sync_org_to_member(&peer, target_root_id, org_id))
            .map_err(KernelError::Message)
    }

    /// `p2p-sync-peer-organizations`（ipc/p2p.ts:72-93）：从指定 peer 反熵
    /// 对账全部共同组织（不带 claim，对齐该通道的调用形状）。校验顺序与
    /// 错误文案对齐 TS：p2p 未启动 → 身份锁定 → 地址缺失。
    pub fn sync_peer_organizations(
        &self,
        target_peer: &OrganizationNodeInfo,
    ) -> Result<PeerOrgSyncResult> {
        let ctx = self.org_sync_context().ok_or_else(|| {
            KernelError::Message(
                "P2P node is not started. Start P2P before syncing organizations.".to_string(),
            )
        })?;
        self.require_unlocked_root_id()?;
        if target_peer.addresses.is_empty() {
            return Err(KernelError::Message(
                "Target peer addresses are required".to_string(),
            ));
        }
        let peer = PeerNodeInfo {
            peer_id: target_peer.peer_id.clone(),
            addresses: target_peer.addresses.clone(),
        };
        let stats = self
            .runtime
            .handle()
            .block_on(ctx.reconcile_from_peer(&peer, false))
            .map_err(KernelError::Message)?;
        Ok(stats.into())
    }

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

    /// 手动执行一次组织保活 tick（候选拨号/反熵/补副本/recovery；
    /// 周期 tick 由事件泵驱动，本方法供测试与壳层诊断注入）。
    pub fn org_keepalive_once(&self) -> Result<()> {
        let ctx = self.org_sync_context().ok_or(P2pError::NotStarted)?;
        self.runtime.handle().block_on(ctx.maintain_org_tick());
        Ok(())
    }
}

impl std::fmt::Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel")
            .field("data_dir", &self.config.data_dir)
            .field("storage_root_id", &self.storage_root_id)
            .field("unlocked", &self.unlocked.is_some())
            .field("p2p_running", &self.p2p.is_some())
            .finish_non_exhaustive()
    }
}
