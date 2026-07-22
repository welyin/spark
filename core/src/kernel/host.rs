//! kernel 的 P2pHost 实现：把 p2p 事件循环的业务回调接到内核存储与身份状态上。
//!
//! 已接线的回调：`current_root_id`、`evidence_head_hash`、`apply_remote_update`
//! （sync 模块远端应用 + purge 水位线拦截）、`recovery_view`（org 模块恢复视图）、
//! org-share 接收应答（`apply_incoming_org_share`：快照合并 → 落库 → pluginDocs
//! → ack）、org-pull-list/org 响应（`handle_org_pull_*`，org::pull 纯逻辑）、
//! org-share-ack 唤醒（`on_org_share_ack` → 推送编排的等待器注册表）。
//!
//! 纯逻辑全在 org 模块（snapshot/pull/plugin_docs），本层只做编排与错误映射。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::collection::{CollectionConfig, DocumentCollection};
use crate::data_mgmt::watermark::StoragePurgeWatermark;
use crate::evidence::get_evidence_head_hash;
use crate::org::recovery::RecoveryViewItem;
use crate::org::{
    OrganizationService, PluginDocSyncItem, apply_plugin_doc_sync_items,
    handle_pull_list_request, handle_pull_org_request, validate_incoming_share_payload,
};
use crate::p2p::host::{OrgShareAck, P2pHost};
use crate::p2p::node::system_now_ms;
use crate::schema::CollectionSchemaDeclaration;
use crate::storage::SledStorage;
use crate::sync::apply::{ApplyRemoteOptions, apply_remote_update};
use crate::sync::meta::RemoteMeta;

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
}

impl KernelHost {
    /// 按登记配置构造集合适配器（pluginDocs 应用与远端应用共用）。
    fn make_collection(&self, domain: &str, collection: &str) -> DocumentCollection {
        let config = self
            .collection_configs
            .lock()
            .unwrap()
            .get(&(domain.to_string(), collection.to_string()))
            .cloned()
            .unwrap_or_default();
        DocumentCollection::new(domain, collection, config)
    }
}

impl P2pHost for KernelHost {
    fn current_root_id(&mut self) -> Option<String> {
        self.current_root_id.lock().unwrap().clone()
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
        let payload_opt = if payload.is_null() { None } else { Some(payload) };
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
        let current = self.current_root_id.lock().unwrap().clone();
        let Ok((target_root_id, organization, sync_id, plugin_docs)) =
            validate_incoming_share_payload(&payload, current.as_deref())
        else {
            // TS：invalid payload / target mismatch / 非成员 → console.warn 后 accepted:false
            return Ok(None);
        };
        let now = system_now_ms();
        let merged = OrganizationService::apply_incoming_snapshot(
            &mut self.storage,
            &organization,
            now,
        )
        .map_err(|e| e.to_string())?;
        // pluginDocs 随快照捎带（plugin-org-sync.ts `applyPluginDocSyncItems`）
        if !plugin_docs.is_empty() {
            let items: Vec<PluginDocSyncItem> = plugin_docs
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            let configs = Arc::clone(&self.collection_configs);
            apply_plugin_doc_sync_items(&mut self.storage, &items, |domain, collection| {
                let config = configs
                    .lock()
                    .unwrap()
                    .get(&(domain.to_string(), collection.to_string()))
                    .cloned()
                    .unwrap_or_default();
                DocumentCollection::new(domain, collection, config)
            }, now)
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
        let current = self.current_root_id.lock().unwrap().clone();
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
                let _ = self.push_notify.send(super::org_sync::OrgSyncRequest::PushOrg {
                    org_id,
                    actor_root_id: actor.clone(),
                });
            }
        }
        Ok(response)
    }

    /// org-pull-org 响应（org-pull-sync.ts:200-241）。
    fn handle_org_pull_org(
        &mut self,
        payload: Value,
        remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        handle_pull_org_request(&self.storage, &payload, remote_peer_id.as_deref())
            .map_err(|e| e.to_string())
    }

    fn recovery_view(&mut self) -> Vec<RecoveryViewItem> {
        let Some(root_id) = self.current_root_id.lock().unwrap().clone() else {
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
        self.org_acks.lock().unwrap().mark_ack(sync_id);
    }
}
