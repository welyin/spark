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
//!
//! 代码组织：本文件为 [`Kernel`] 结构、生命周期（init/shutdown）与存储对齐；
//! 文档/数据治理/存证门面在 `doc_ops`，组织门面与同步编排包装在 `org_ops`，
//! 副本概览在 `org_overview`，P2P/节点名片/组织地址门面在 `p2p_ops`，身份 API
//! 在 `identity/`，组织同步编排（async worker）在 `org_sync/`，消息门面在
//! `message_ops`（出站投递机器在 `dm_delivery`），通讯录门面在 `contact_ops`
//! （标签/分组树在 `contact_group_ops`），dm 信封构造/校验在
//! `dm_envelope`，dm 入站编排在 `inbound_dm`（host.rs 的 `handle_dm` 接线）。

mod contact_group_ops;
mod contact_ops;
mod contact_request_ops;
mod dm_delivery;
mod doc_ops;
pub mod dm_envelope;
mod error;
mod host;
mod identity;
mod inbound_dm;
mod message_ops;
mod org_ops;
mod org_overview;
mod org_sync;
mod p2p_ops;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

pub use contact_ops::SendFriendRequestInput;
pub use doc_ops::{EvidenceChainStatus, PurgePreviewInfo};
pub use error::{KernelError, Result};
pub use identity::{
    DerivedDomainIdentityInfo, DomainSignatureInfo, IdentityStatus, IdentitySummary,
    InitIdentityResult, MnemonicCheckInfo, ProfileInfo, PublicIdentity, RootSignatureInfo,
};
pub use inbound_dm::{AutoAccept, InboundDmError, InboundDmResult, handle_inbound_dm};
pub use message_ops::{
    AppMessageView, ChatMessageView, ConversationView, app_conversation_id,
    direct_conversation_id, sanitize_link_preview,
};
pub use org_sync::{OrgReconcileStats, PeerOrgSyncResult};
pub use p2p_ops::NodeCardImport;

use crate::data_mgmt::DataManagementService;
use crate::p2p::keepalive::RecoveryTrigger;
use crate::p2p::{P2pConfig, P2pEvent, P2pNode};
use crate::storage::SledStorage;

use host::{CollectionConfigs, SharedOrgShareAckTracker};
use org_sync::OrgSyncRequest;

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
    /// p2p 启动时间（组织网络状态防抖的"无连接时长"下限基准）。
    pub(crate) p2p_started_at: Option<i64>,
    /// 登录链路自动启动 p2p 的失败原因（`p2p_status` 暴露给壳层；成功/停止时清除）。
    pub(crate) p2p_start_error: Option<String>,
    pub(crate) p2p_pump: Option<tokio::task::JoinHandle<()>>,
    /// org-sync worker（推送/保活串行队列），随 p2p 起停。
    pub(crate) org_sync_worker: Option<tokio::task::JoinHandle<()>>,
    /// org-sync 请求队列的发送端（p2p 运行期存在；host 与门面触发推送用）。
    pub(crate) org_sync_tx: Option<tokio::sync::mpsc::UnboundedSender<OrgSyncRequest>>,
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    /// p2p 宿主可见的当前身份指针（事件循环线程共享）。
    pub(crate) current_root_id_shared: Arc<Mutex<Option<String>>>,
    /// p2p 宿主可见的当前身份昵称（dm 入站应答/回发用；随解锁/资料更新
    /// 刷新，lock 清空——避免事件循环线程逐条 dm 读身份文件）。
    pub(crate) nickname_shared: Arc<Mutex<String>>,
    /// p2p 宿主可见的当前身份头像（data URL，空串=无头像；口径与
    /// `nickname_shared` 相同，随解锁/资料更新刷新、lock 清空）。
    pub(crate) avatar_shared: Arc<Mutex<String>>,
    /// p2p 节点句柄共享格（host 回发 auto_accept 用；start 后回填、stop 清空）。
    pub(crate) p2p_node_shared: Arc<Mutex<Option<Arc<P2pNode>>>>,
    /// 解锁期签名私钥（org-sync worker 自签 nodeInfoClaim 用；lock 时清除）。
    pub(crate) signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    /// org-share-ack 等待器注册表（host 与 worker 共享）。
    pub(crate) org_acks: SharedOrgShareAckTracker,
    /// org-recovery 触发器（跨 tick 状态：连续失联计数 + 全局冷却）。
    pub(crate) recovery_trigger: Arc<Mutex<RecoveryTrigger>>,
    /// 组织地址记录发布状态（orgAddress → 最近发布时间；org.md §16）。
    pub(crate) org_address_publish: Arc<Mutex<HashMap<String, i64>>>,
    /// doc_* 调用登记的集合配置（远端应用的索引维护依据，见 host.rs）。
    pub(crate) collection_configs: CollectionConfigs,
    /// 存储读写互斥：p2p 事件循环（host `handle_dm`）与 Tauri 命令线程的
    /// read-modify-write 串行化。锁顺序：Tauri `Mutex<Kernel>` → `io_lock`
    /// （host 只拿 `io_lock`，不会死锁）；查询类方法可不加。
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 应用消息限流器（内存态，p2p-messages.md §20.5；进程重启清零）。
    pub(crate) app_msg_limiter: crate::message::AppMessageRateLimiter,
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
            p2p_started_at: None,
            p2p_start_error: None,
            p2p_pump: None,
            org_sync_worker: None,
            org_sync_tx: None,
            event_tx,
            current_root_id_shared: Arc::new(Mutex::new(None)),
            nickname_shared: Arc::new(Mutex::new(String::new())),
            avatar_shared: Arc::new(Mutex::new(String::new())),
            p2p_node_shared: Arc::new(Mutex::new(None)),
            signing_key_shared: Arc::new(Mutex::new(None)),
            org_acks: Arc::new(Mutex::new(Default::default())),
            recovery_trigger: Arc::new(Mutex::new(RecoveryTrigger::new())),
            org_address_publish: Arc::new(Mutex::new(HashMap::new())),
            collection_configs: Arc::new(Mutex::new(HashMap::new())),
            io_lock: Arc::new(Mutex::new(())),
            app_msg_limiter: crate::message::AppMessageRateLimiter::default(),
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

    /// 测试专用：克隆共享存储句柄（sled 内部为 Arc，克隆不重复占用锁）。
    /// 仅供壳层测试断言底层 KV，正常代码路径请走公开 API。
    #[doc(hidden)]
    pub fn __test_storage(&self) -> Option<SledStorage> {
        self.storage.clone()
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
