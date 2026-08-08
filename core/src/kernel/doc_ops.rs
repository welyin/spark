//! 文档/集合、数据治理与存证门面（`Kernel` 的本地数据 API）。
//!
//! - 文档：`doc_*` 走 `collection` 模块的本地写入路径（doc/meta/索引/存证同
//!   batch），随后经 p2p 广播 `update`/`delete`（未启动或失败不影响本地写入，
//!   对齐 TS 的非阻塞语义）；
//! - 数据治理：委托 `data_mgmt` 服务层（用量/清理/导出/purge），purge 的副本
//!   概览按 ipc/data.ts 接线；
//! - 存证：`evidence-*` 通道（ipc/db.ts）委托 `evidence` 模块。

use serde_json::{Map, Value};

use super::{Kernel, KernelError, Result};
use crate::collection::{CollectionConfig, DocumentCollection, QueryOptions, QueryResult};
use crate::data_mgmt::service::ReplicaStatus;
use crate::data_mgmt::{
    AutoCleanupResult, DataMgmtError, DataUsageReport, ExportWriteResult, PurgePreview,
    PurgeResult, write_export_dump,
};
use crate::evidence::{
    EvidenceEntry, get_evidence_entry, get_evidence_head_hash, get_evidence_height,
    verify_evidence_chain,
};
use crate::org::{OrgSyncOverview, OrganizationView, collect_org_plugin_domains};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, build_delete_body, build_update_body};
use crate::schema::{
    CollectionSchemaDeclaration, CollectionSchemaRecord, declare_collection_schema,
};
use crate::storage::StorageBackend;

/// p2p 未运行时的稳定 nodeId 回退：从持久化的 p2p 节点身份
/// （`p2p:identity:privateKey`，protobuf 编码 Ed25519 密钥对的 base64）派生
/// peerId——同设备重启稳定、跨设备唯一。固定串 `local-node` 会让多台离线
/// 设备的写入被版本向量判为同源（并发写静默丢更新）。仅在持久化身份缺失/
/// 损坏（如全新设备从未启动过 p2p）时回退 `local-node`。
pub(crate) fn persisted_sync_node_id(storage: &dyn StorageBackend) -> String {
    use base64::Engine as _;
    storage
        .get(crate::p2p::constants::P2P_IDENTITY_PRIVATE_KEY)
        .ok()
        .flatten()
        .and_then(|encoded| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .ok()?;
            let keypair = libp2p::identity::Keypair::from_protobuf_encoding(&bytes).ok()?;
            Some(libp2p::identity::PeerId::from_public_key(&keypair.public()).to_base58())
        })
        .unwrap_or_else(|| "local-node".to_string())
}

/// `data-purge-preview` 的返回（ipc/data.ts:73-86 的形状）。
#[derive(Clone, Debug)]
pub struct PurgePreviewInfo {
    /// 组织 id。
    pub org_id: String,
    /// 扫描定位的组织数据域（`doc:plugin:` 键反推；无插件文档时为 `""`）。
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

impl Kernel {
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

    /// 本地写入节点 id：p2p 运行中为 peerId；否则回退持久化 p2p 身份派生的
    /// 稳定 id（见 [`persisted_sync_node_id`]）。
    pub(crate) fn sync_node_id(&self) -> String {
        if let Some(node) = &self.p2p {
            return node.peer_id().to_string();
        }
        self.storage
            .as_ref()
            .map(|storage| persisted_sync_node_id(storage))
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
        let write = coll.put(
            self.require_storage_mut()?,
            id,
            &doc,
            &node_id,
            system_now_ms(),
        )?;
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
        let Some(write) =
            coll.delete(self.require_storage_mut()?, id, &node_id, system_now_ms())?
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
        if let Err(e) = self
            .runtime
            .handle()
            .block_on(node.broadcast(SYNC_TOPIC, body))
        {
            let _ = self
                .event_tx
                .send(P2pEvent::Warning(format!("sync broadcast failed: {e}")));
        }
    }

    // ------------------------------------------------------------------
    // 数据治理 API（委托 data_mgmt）
    // ------------------------------------------------------------------

    /// 数据用量统计（缓存优先；含磁盘信息）。
    pub fn get_usage(&mut self) -> Result<DataUsageReport> {
        let storage = self.storage.as_ref().ok_or(KernelError::StorageNotReady)?;
        let dm = self
            .data_mgmt
            .as_mut()
            .ok_or(KernelError::StorageNotReady)?;
        Ok(dm.get_usage(storage, system_now_ms())?)
    }

    /// 立即执行 L1 自动清理。
    pub fn run_cleanup_now(&mut self) -> Result<AutoCleanupResult> {
        let storage = self.storage.as_mut().ok_or(KernelError::StorageNotReady)?;
        let dm = self
            .data_mgmt
            .as_mut()
            .ok_or(KernelError::StorageNotReady)?;
        Ok(dm.run_cleanup_now(storage, system_now_ms()))
    }

    /// 全库导出（紧凑 JSON 写文件）。
    pub fn export_dump(&self, file_path: impl AsRef<std::path::Path>) -> Result<ExportWriteResult> {
        Ok(write_export_dump(
            self.require_storage()?,
            file_path,
            system_now_ms(),
        )?)
    }

    /// 解析目标组织（ipc/data.ts `resolveOrg` 的组织查找部分）：必须存在。
    ///
    /// 数据域定位：组织记录已无 `basePluginDomain` 字段，改为扫描存储的
    /// `doc:plugin:` 键，收集 `payload.orgId` 命中该组织的插件域——
    /// 0 个 → 返回 `""`（preview 展示空、affectedDocs=0，前端自然拦截 execute）；
    /// 1 个 → 用之；多个 → 取第一个（扫描键升序，结果确定），保持单 domain
    /// 语义（前端只显示一个；purge 按单 domain 执行）。
    fn resolve_org(&self, org_id: &str) -> Result<(OrganizationView, String)> {
        let view = self
            .list_orgs()?
            .into_iter()
            .find(|item| item.record.org_id == org_id)
            .ok_or_else(|| {
                KernelError::Internal("Organization not found or not a member".to_string())
            })?;
        let domain = collect_org_plugin_domains(self.require_storage()?, org_id)?
            .into_iter()
            .next()
            .unwrap_or_default();
        Ok((view, domain))
    }

    /// purge 预览（不鉴权管理员，对齐 TS；管理员标记随结果返回供壳层判断）。
    pub fn preview_purge(&self, org_id: &str, before_ts: i64) -> Result<PurgePreviewInfo> {
        let (view, domain) = self.resolve_org(org_id)?;
        // before_ts 校验先于空域短路：有域路径由 data-mgmt 校验（purge.rs
        // select_expired_metas，同口径 `before_ts <= 0`），空域路径不下到
        // data-mgmt，在此复用同一错误补齐校验，保持两条路径行为一致
        if before_ts <= 0 {
            return Err(DataMgmtError::InvalidBeforeTs.into());
        }
        // 扫描不到插件文档（domain 为 ""）→ 直接返回空影响面：前端展示空、
        // affectedDocs=0 自然拦截 execute；不下到 data-mgmt（其拒绝非插件域）
        let preview = if domain.is_empty() {
            PurgePreview {
                collections: Vec::new(),
                affected_docs: 0,
                affected_bytes: 0,
            }
        } else {
            self.data_mgmt
                .as_ref()
                .ok_or(KernelError::StorageNotReady)?
                .preview_purge(self.require_storage()?, &domain, before_ts)?
        };
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
        let dm = self
            .data_mgmt
            .as_mut()
            .ok_or(KernelError::StorageNotReady)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use base64::Engine as _;

    fn store_p2p_identity(storage: &mut MemoryStorage, keypair: &libp2p::identity::Keypair) {
        let raw = keypair.to_protobuf_encoding().unwrap();
        storage
            .put(
                crate::p2p::constants::P2P_IDENTITY_PRIVATE_KEY,
                &base64::engine::general_purpose::STANDARD.encode(raw),
            )
            .unwrap();
    }

    /// p2p 未运行时 nodeId 回退：有持久化 p2p 身份 → 派生 peerId（稳定、
    /// 非 `local-node`）；无身份 → `local-node`。
    #[test]
    fn persisted_sync_node_id_derives_from_stored_identity() {
        let mut s = MemoryStorage::new();
        assert_eq!(
            persisted_sync_node_id(&s),
            "local-node",
            "无持久化身份时回退 local-node"
        );

        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let expected =
            libp2p::identity::PeerId::from_public_key(&keypair.public()).to_base58();
        store_p2p_identity(&mut s, &keypair);

        let id = persisted_sync_node_id(&s);
        assert_eq!(id, expected, "应派生持久化身份的 peerId");
        assert_ne!(id, "local-node");
        // 稳定：重复读取一致
        assert_eq!(persisted_sync_node_id(&s), id);
    }

    /// 两台"设备"各有持久化身份 → nodeId 互不相同（版本向量唯一性，
    /// 避免离线并发写被判为同源而丢更新）。
    #[test]
    fn persisted_sync_node_id_distinct_across_devices() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        store_p2p_identity(&mut a, &libp2p::identity::Keypair::generate_ed25519());
        store_p2p_identity(&mut b, &libp2p::identity::Keypair::generate_ed25519());
        assert_ne!(persisted_sync_node_id(&a), persisted_sync_node_id(&b));
    }
}
