//! docs/data 能力（从 `host_env` 拆出，文件长度硬线）：集合配置解析、
//! docs.* 文档读写、data.* 插件数据（含 orgq 路由）、blob 读写、feed-blob
//! 拉取。零逻辑变化。

use serde_json::Value;

use crate::collection::{CollectionConfig, DocumentCollection};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, build_delete_body, build_update_body};
use crate::schema::{CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema};

use crate::plugin::error::{PluginError, Result};

use super::{PluginHostShared, required_str};

// ------------------------------------------------------------------
// 文档能力（docs.*）：域缺省为插件 id；显式域经合法性约束（见
// resolve_doc_domain）。语义对齐 kernel doc_* 门面
// ------------------------------------------------------------------

/// docs 能力的访问别：空间根域按只读收窄（见 resolve_doc_domain）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocAccess {
    Read,
    Write,
}

/// 空间根域（`space:personal` / `space:org`）放开只读的遗留集合白名单：
/// 旧 UI 桥的历史缺陷把 bot 文档沉在空间根域（ai-chat botDataSpaces 的
/// 兜底读只涉及这些集合），仅放行读；空间根域的写一律拒绝——共享空间
/// 数据面不属任一插件，放开写即可伪造他方配置（如 ai_chat_bots 的
/// cliPath 指向恶意二进制）。
const SPACE_DOMAIN_READABLE_COLLECTIONS: [&str; 1] = ["ai_chat_bots"];

/// 解析 docs 能力的目标域：缺省（null/空）为插件自身域；显式指定的域必须
/// 属于本插件的数据面——自身域、`plugin:{pluginId}` 根域（UI 桥历史数据面，
/// 存量 bot 文档沉在那里）可读写；空间根域（`space:personal` / `space:org`）
/// 仅限 [`SPACE_DOMAIN_READABLE_COLLECTIONS`] 遗留集合的只读。其余域
/// （其他插件 id / 组织域等）拒绝——插件不可读写他方数据。
fn resolve_doc_domain<'a>(
    plugin_id: &'a str,
    payload: &'a Value,
    access: DocAccess,
) -> Result<&'a str> {
    let plugin_root = format!("plugin:{plugin_id}");
    match payload.get("domain").and_then(Value::as_str) {
        None | Some("") => Ok(plugin_id),
        Some(domain) if domain == plugin_id => Ok(plugin_id),
        Some(domain) if domain == plugin_root => Ok(domain),
        Some(domain) if domain == "space:personal" || domain == "space:org" => {
            let collection = payload
                .get("collection")
                .and_then(Value::as_str)
                .unwrap_or("");
            if access == DocAccess::Read && SPACE_DOMAIN_READABLE_COLLECTIONS.contains(&collection)
            {
                Ok(domain)
            } else {
                Err(PluginError::InvalidCall(format!(
                    "domain not allowed for plugin {plugin_id}: {domain}"
                )))
            }
        }
        Some(domain) => Err(PluginError::InvalidCall(format!(
            "domain not allowed for plugin {plugin_id}: {domain}"
        ))),
    }
}

impl PluginHostShared {
    /// 集合配置载荷（camelCase，对齐壳层 CollectionConfigDto）。
    pub(super) fn parse_config(payload: &Value) -> Result<CollectionConfig> {
        let sync_strategy = match payload.get("syncStrategy").and_then(Value::as_str) {
            None => None,
            Some("append-only") => Some(SyncStrategy::AppendOnly),
            Some("lww") => Some(SyncStrategy::Lww),
            Some(other) => {
                return Err(PluginError::InvalidCall(format!(
                    "syncStrategy must be 'append-only' or 'lww', got {other:?}"
                )));
            }
        };
        let indexed_fields = payload
            .get("indexedFields")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(CollectionConfig {
            indexed_fields,
            enable_evidence: payload.get("enableEvidence").and_then(Value::as_bool),
            sync_strategy,
            governance: payload.get("governance").and_then(Value::as_bool),
        })
    }

    /// 本地写入节点 id：p2p 运行中为 peerId，否则 `local-node`（对齐内核门面）。
    pub(crate) fn sync_node_id(&self) -> String {
        self.p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| "local-node".to_string())
    }

    /// 集合配置缓存写入（对齐 `Kernel::make_collection`）。
    pub(super) fn remember_collection_config(
        &self,
        domain: &str,
        collection: &str,
        config: &CollectionConfig,
    ) {
        self.collection_configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((domain.to_string(), collection.to_string()), config.clone());
    }

    /// 广播同步消息：p2p 未启动直接跳过；失败降级为事件流告警。与门面
    /// `broadcast_sync_body` 的语义差异仅投递方式——插件线程不 block_on
    /// （与内核线程模型口径一致），改为 spawn fire-and-forget。
    pub(super) fn spawn_broadcast_sync_body(&self, body: serde_json::Map<String, Value>) {
        let node = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(node) = node else { return };
        let event_tx = self.event_tx.clone();
        self.runtime.spawn(async move {
            if let Err(error) = node.broadcast(SYNC_TOPIC, body).await {
                let _ = event_tx.send(P2pEvent::Warning(format!("sync broadcast failed: {error}")));
            }
        });
    }

    pub(super) fn doc_get(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Read)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let storage = self.require_storage()?;
        let coll = DocumentCollection::new(domain, collection, CollectionConfig::default());
        let doc = coll.get(&storage, id)?;
        Ok(match doc {
            Some(value) => value,
            None => Value::Null,
        })
    }

    pub(super) fn doc_put(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Write)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let doc = payload.get("doc").cloned().unwrap_or(Value::Null);
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let coll = DocumentCollection::new(domain, collection, config);
        let node_id = self.sync_node_id();
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let write = coll.put(&mut storage, id, &doc, &node_id, system_now_ms())?;
        let body = build_update_body(
            domain,
            collection,
            id,
            doc,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        drop(_io);
        self.spawn_broadcast_sync_body(body);
        Ok(Value::Null)
    }

    pub(super) fn doc_delete(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Write)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let coll = DocumentCollection::new(domain, collection, config);
        let node_id = self.sync_node_id();
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        // 与门面一致：删除不存在文档为空操作
        let Some(write) = coll.delete(&mut storage, id, &node_id, system_now_ms())? else {
            return Ok(Value::Null);
        };
        let body = build_delete_body(
            domain,
            collection,
            id,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        drop(_io);
        self.spawn_broadcast_sync_body(body);
        Ok(Value::Null)
    }

    pub(super) fn doc_query(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Read)?;
        let collection = required_str(payload, "collection")?;
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let options = Self::parse_query_options(payload.get("options").unwrap_or(&Value::Null))?;
        let coll = DocumentCollection::new(domain, collection, config);
        let storage = self.require_storage()?;
        let result = coll.query(&storage, &options)?;
        // 对齐前端 QueryResult 形状：{items:[{id,data}], nextCursor?}
        let mut value = serde_json::json!({
            "items": result.items.iter().map(|item| serde_json::json!({
                "id": item.id, "data": item.data
            })).collect::<Vec<_>>(),
        });
        if let Some(cursor) = result.next_cursor {
            value["nextCursor"] = Value::String(cursor);
        }
        Ok(value)
    }

    pub(super) fn doc_define_collection(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let collection = required_str(payload, "collection")?;
        let declaration: CollectionSchemaDeclaration =
            serde_json::from_value(payload.get("schema").cloned().unwrap_or(Value::Null))?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        declare_collection_schema(
            &mut storage,
            plugin_id,
            collection,
            &declaration,
            system_now_ms(),
        )?;
        Ok(Value::Null)
    }

    // ------------------------------------------------------------------
    // P6 声明式数据 API（data.*）：personal scope。策略随声明走，读写零
    // 同步参数；存储镜像是版本化句柄，写库即同步（pdsync 记账全自动）。
    // ------------------------------------------------------------------

    /// 解析 data.* 载荷的目标声明（name + 可选 version → 最新代际）。
    pub(super) fn resolve_data_declaration(
        &self,
        plugin_id: &str,
        payload: &Value,
    ) -> Result<(
        crate::plugindata::CollectionDeclaration,
        crate::kernel::KernelStorage,
    )> {
        let name = required_str(payload, "name")?;
        // 插件只能触达自己前缀的集合（与声明校验同口径）
        if !name.starts_with(&format!("{plugin_id}:")) {
            return Err(PluginError::InvalidCall(format!(
                "collection {name:?} does not belong to plugin {plugin_id}"
            )));
        }
        let version = payload.get("version").and_then(Value::as_str);
        let storage = self.require_storage()?;
        // B5：org 空间数据读写路由到 orgd: 键——payload 携带 orgId 时走
        // org 声明域，否则走 personal 声明域。
        let decl = match payload.get("orgId").and_then(Value::as_str) {
            Some(org_id) => crate::plugindata::resolve_org(&storage, org_id, name, version)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?,
            None => crate::plugindata::resolve(&storage, name, version)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?,
        };
        Ok((decl, storage))
    }
}
