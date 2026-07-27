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
    AutoCleanupResult, DataUsageReport, ExportWriteResult, PurgePreview, PurgeResult,
    write_export_dump,
};
use crate::evidence::{
    EvidenceEntry, get_evidence_entry, get_evidence_head_hash, get_evidence_height,
    verify_evidence_chain,
};
use crate::org::{OrgSyncOverview, OrganizationView};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, build_delete_body, build_update_body};
use crate::schema::{
    CollectionSchemaDeclaration, CollectionSchemaRecord, declare_collection_schema,
};

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
