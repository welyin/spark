//! 插件后台运行时的宿主能力面：内核共享句柄镜像 + capability 分发。
//!
//! 插件线程**不持** `Mutex<Kernel>`——capability 所需句柄（存储镜像、事件
//! 广播、p2p 节点、签名私钥、当前身份、runtime handle）全部为内核既有
//! `Arc` 共享格的克隆，插件线程只经 [`PluginHostShared`] 访问。存储为镜像
//! 格：身份切换换库时内核更新镜像，插件线程每次调用现取（sled 克隆共享
//! 同一底层库，不会持有旧库句柄）。
//!
//! 锁序与 kernel 门面一致：先 `io_lock`（长持，落库互斥），镜像锁只在
//! 读取句柄瞬间持有，二者不构成环。

use std::collections::HashMap;
use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::collection::{
    CollectionConfig, DocumentCollection, FilterOp, QueryFilter, QueryOptions,
};
use crate::contact::ContactService;
use crate::message::{MessageError, MessageService, generate_message_id};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode, build_delete_body, build_update_body};
use crate::schema::{CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema};
use crate::storage::SledStorage;

use super::error::{PluginError, Result};
use super::runtime::PluginEvent;

/// 插件运行时的宿主共享句柄（全部由 kernel 门面的共享格克隆而来）。
///
/// `runtime` 为 kernel tokio runtime 的句柄：能力实现里需要异步投递时
/// spawn 到内核 runtime（插件线程是普通 OS 线程，spawn 安全；不得
/// `block_on`——与 kernel 线程模型口径一致）。
#[derive(Clone)]
pub(crate) struct PluginHostShared {
    /// 当前身份的存储镜像（`open_storage` 回填、`shutdown` 清空）。
    pub(crate) storage: Arc<Mutex<Option<SledStorage>>>,
    /// 存储读写互斥锁（与 kernel 门面同一把）。
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 内核事件广播（bot 回复落库后发 ChatReceived，与真人消息同口径）。
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    /// 当前身份 rootId 共享格（= kernel `current_root_id_shared`）。
    pub(crate) my_root_id: Arc<Mutex<Option<String>>>,
    /// p2p 节点句柄共享格（= kernel `p2p_node_shared`）。
    pub(crate) p2p_node: Arc<Mutex<Option<Arc<P2pNode>>>>,
    /// 解锁期签名私钥共享格（= kernel `signing_key_shared`，自设备回同步
    /// 信封自签用）。
    pub(crate) signing_key: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    /// 集合配置缓存（= kernel `collection_configs`；docs.put/delete/query 时
    /// 写入兜底声明，已持久化的集合声明优先）。
    pub(crate) collection_configs: Arc<Mutex<HashMap<(String, String), CollectionConfig>>>,
    /// 宿主查询在途表（query_id → 应答通道；`plugin_host_query` 插入，
    /// JS 侧 `query.respond` 回流取出）。
    pub(crate) pending_queries: Arc<Mutex<HashMap<u64, StdSender<Value>>>>,
    /// kernel tokio runtime 句柄（投递任务 spawn 目标）。
    pub(crate) runtime: tokio::runtime::Handle,
}

/// 单插件运行时上下文（每次启动绑定；与跨插件共享的 [`PluginHostShared`]
/// 相对——事件回流通道是每插件一条）。
#[derive(Clone)]
pub(crate) struct PluginRuntimeContext {
    pub(crate) plugin_id: String,
    pub(crate) event_tx: std::sync::mpsc::Sender<PluginEvent>,
}

impl PluginHostShared {
    /// 读当前存储镜像（ sled 克隆，共享底层库；未打开返回 StorageNotReady）。
    pub(crate) fn require_storage(&self) -> Result<SledStorage> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(PluginError::StorageNotReady)
    }

    /// host 调用入口（JS `__spark_host_call` 的 Rust 侧）：分发 capability，
    /// 结果/错误统一序列化为 JSON 字符串返回 JS（错误形如 `{"error": "..."}`，
    /// 由 JS prelude 转为异常抛出）。
    pub(crate) fn call(
        &self,
        rtx: &PluginRuntimeContext,
        capability: &str,
        payload_json: &str,
    ) -> String {
        match self.dispatch(rtx, capability, payload_json) {
            Ok(value) => value.to_string(),
            Err(error) => serde_json::json!({ "error": error.to_string() }).to_string(),
        }
    }

    fn dispatch(
        &self,
        rtx: &PluginRuntimeContext,
        capability: &str,
        payload_json: &str,
    ) -> Result<Value> {
        let plugin_id = rtx.plugin_id.as_str();
        let payload: Value = serde_json::from_str(payload_json)?;
        match capability {
            "log" => {
                let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
                eprintln!("[plugin:{plugin_id}] {message}");
                Ok(Value::Null)
            }
            "contact.ensureBot" => self.ensure_bot(plugin_id, &payload),
            "message.reply" => self.reply(plugin_id, &payload),
            // 文档能力：域恒为插件 id（插件不可读写他域数据）；同步 sled 操作
            "docs.get" => self.doc_get(plugin_id, &payload),
            "docs.put" => self.doc_put(plugin_id, &payload),
            "docs.delete" => self.doc_delete(plugin_id, &payload),
            "docs.query" => self.doc_query(plugin_id, &payload),
            "docs.defineCollection" => self.doc_define_collection(plugin_id, &payload),
            // 系统能力：长时操作异步化——启动即返，结果经事件队列回流
            // （JS Promise 由 prelude 配对 callId）
            "sys.exec.start" => self.sys_exec_start(rtx, &payload),
            "sys.fetch.start" => self.sys_fetch_start(rtx, &payload),
            // 宿主查询应答回流（plugin_host_query 的另一半）
            "query.respond" => self.query_respond(&payload),
            other => Err(PluginError::InvalidCall(format!(
                "unknown capability: {other}"
            ))),
        }
    }

    /// `contact.ensureBot` 能力：注册/刷新本插件的 bot 联系人（出现在通讯录）。
    ///
    /// bot rootId 由内核拼定为 `bot:{pluginId}:{botId}`（插件不可伪造他插件
    /// 的 bot）；`botId` 拒空串与冒号（保 rootId 三段式可解析）。
    fn ensure_bot(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let bot_id = required_str(payload, "botId")?;
        let display_name = required_str(payload, "displayName")?;
        if bot_id.is_empty() || bot_id.contains(':') {
            return Err(PluginError::InvalidCall(format!("invalid botId: {bot_id}")));
        }
        let bot_root_id = format!("bot:{plugin_id}:{bot_id}");
        crate::kernel::ensure_bot_shared(self, &bot_root_id, display_name)?;
        Ok(serde_json::json!({ "botRootId": bot_root_id }))
    }

    /// `message.reply` 能力：向插件**自己**的 bot 会话写入一条 bot 回复。
    ///
    /// bot 身份不从载荷取（防伪造）：由会话落库的权威 `peer_root_id` 推导，
    /// 并强制归属校验——目标会话必须是本插件的 bot 会话
    /// （`bot:{pluginId}:{botId}` 前缀）。
    fn reply(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = required_str(payload, "spaceKey")?;
        let conv_id = required_str(payload, "convId")?;
        let text = required_str(payload, "text")?;
        // 查询类不加 io_lock（kernel 门面同口径）；归属判定后 append 路径
        // 在 bot_reply_shared 内持锁并重新读会话，会话 peer 不可变，无竞态
        let storage = self.require_storage()?;
        let conv = MessageService::get_conversation(&storage, space, conv_id)?
            .ok_or(MessageError::ConversationNotFound)?;
        let prefix = format!("bot:{plugin_id}:");
        if !conv.peer_root_id.starts_with(&prefix) {
            return Err(PluginError::ConversationNotOwned(conv_id.to_string()));
        }
        let bot_root_id = conv.peer_root_id.clone();
        let bot_name = ContactService::get_friend(&storage, &bot_root_id)?
            .map(|friend| friend.nickname)
            .filter(|nickname| !nickname.is_empty())
            .unwrap_or_else(|| bot_root_id.clone());
        let message_id = generate_message_id(system_now_ms());
        let view = crate::kernel::bot_reply_shared(
            self,
            space,
            conv_id,
            &bot_root_id,
            &bot_name,
            &message_id,
            text,
        )?;
        Ok(serde_json::to_value(view)?)
    }
}

/// 取载荷必填字符串字段（缺失/非字符串为非法调用）。
fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::InvalidCall(format!("missing string field: {field}")))
}

// ------------------------------------------------------------------
// 文档能力（docs.*）：域缺省为插件 id；显式域经合法性约束（见
// resolve_doc_domain）。语义对齐 kernel doc_* 门面
// ------------------------------------------------------------------

/// 解析 docs 能力的目标域：缺省（null/空）为插件自身域；显式指定的域必须
/// 属于本插件的数据面——自身域、`plugin:{pluginId}` 根域（UI 桥历史数据面，
/// 存量 bot 文档沉在那里）、空间根域（`space:personal` / `space:org`）。
/// 其余域（其他插件 id / 组织域等）拒绝——插件不可读写他方数据。
fn resolve_doc_domain<'a>(plugin_id: &'a str, payload: &'a Value) -> Result<&'a str> {
    let plugin_root = format!("plugin:{plugin_id}");
    match payload.get("domain").and_then(Value::as_str) {
        None | Some("") => Ok(plugin_id),
        Some(domain) if domain == plugin_id => Ok(plugin_id),
        Some(domain) if domain == plugin_root => Ok(domain),
        Some(domain) if domain == "space:personal" || domain == "space:org" => Ok(domain),
        Some(domain) => Err(PluginError::InvalidCall(format!(
            "domain not allowed for plugin {plugin_id}: {domain}"
        ))),
    }
}

impl PluginHostShared {
    /// 集合配置载荷（camelCase，对齐壳层 CollectionConfigDto）。
    fn parse_config(payload: &Value) -> Result<CollectionConfig> {
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
    fn sync_node_id(&self) -> String {
        self.p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| "local-node".to_string())
    }

    /// 集合配置缓存写入（对齐 `Kernel::make_collection`）。
    fn remember_collection_config(&self, domain: &str, collection: &str, config: &CollectionConfig) {
        self.collection_configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((domain.to_string(), collection.to_string()), config.clone());
    }

    /// 广播同步消息：p2p 未启动直接跳过；失败降级为事件流告警。与门面
    /// `broadcast_sync_body` 的语义差异仅投递方式——插件线程不 block_on
    /// （与内核线程模型口径一致），改为 spawn fire-and-forget。
    fn spawn_broadcast_sync_body(&self, body: serde_json::Map<String, Value>) {
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

    fn doc_get(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload)?;
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

    fn doc_put(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload)?;
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

    fn doc_delete(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload)?;
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

    fn doc_query(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload)?;
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

    fn doc_define_collection(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let collection = required_str(payload, "collection")?;
        let declaration: CollectionSchemaDeclaration = serde_json::from_value(
            payload.get("schema").cloned().unwrap_or(Value::Null),
        )?;
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

    /// 查询参数载荷（camelCase，对齐壳层 QueryOptionsDto）。
    fn parse_query_options(payload: &Value) -> Result<QueryOptions> {
        let parse_op = |op: Option<&str>| -> Result<FilterOp> {
            match op.unwrap_or("eq") {
                "eq" => Ok(FilterOp::Eq),
                "startsWith" => Ok(FilterOp::StartsWith),
                "gt" => Ok(FilterOp::Gt),
                "lt" => Ok(FilterOp::Lt),
                "gte" => Ok(FilterOp::Gte),
                "lte" => Ok(FilterOp::Lte),
                other => Err(PluginError::InvalidCall(format!(
                    "filter op must be one of eq/startsWith/gt/lt/gte/lte, got {other:?}"
                ))),
            }
        };
        let filter = payload
            .get("filter")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        Ok(QueryFilter {
                            field: required_str(item, "field")?.to_string(),
                            value: item.get("value").cloned().unwrap_or(Value::Null),
                            op: parse_op(item.get("op").and_then(Value::as_str))?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(QueryOptions {
            index_name: payload
                .get("indexName")
                .and_then(Value::as_str)
                .map(str::to_string),
            index_value: payload.get("indexValue").cloned(),
            index_prefix: payload
                .get("indexPrefix")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            start_after_id: payload
                .get("startAfterId")
                .and_then(Value::as_str)
                .map(str::to_string),
            limit: payload
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            reverse: payload
                .get("reverse")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            filter,
        })
    }
}

// ------------------------------------------------------------------
// 系统能力（sys.*）：启动即返，结果经事件队列异步回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `sys.exec.start`：spawn 到内核 runtime（内部 spawn_blocking），完成
    /// 后向本插件事件队列回 `sys-exec-result`。
    fn sys_exec_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let program = required_str(payload, "program")?.to_string();
        let args: Vec<String> = payload
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let workdir = payload
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || crate::sys::exec_blocking(&program, &args, workdir.as_deref()))
                    .await;
            let payload = match result {
                Ok(Ok(r)) => serde_json::json!({
                    "callId": call_id,
                    "exitCode": r.exit_code,
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                }),
                Ok(Err(error)) => serde_json::json!({ "callId": call_id, "error": error }),
                Err(error) => serde_json::json!({
                    "callId": call_id,
                    "error": format!("exec task join failed: {error}")
                }),
            };
            // 插件线程可能已退出：丢弃结果即可（Promise 随线程销毁失去意义）
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-exec-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `sys.fetch.start`：同 exec 的异步回流模式，结果事件 `sys-fetch-result`。
    fn sys_fetch_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let url = required_str(payload, "url")?.to_string();
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let headers: Option<HashMap<String, String>> = payload
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });
        let body = payload
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let payload = match crate::sys::fetch(&url, method.as_deref(), headers.as_ref(), body.as_deref()).await {
                Ok(r) => serde_json::json!({
                    "callId": call_id,
                    "status": r.status,
                    "headers": r.headers,
                    "body": r.body,
                }),
                Err(error) => serde_json::json!({ "callId": call_id, "error": error }),
            };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-fetch-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }
}

// ------------------------------------------------------------------
// 宿主查询应答回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `query.respond`：JS 侧查询处理完成，结果送回等待中的
    /// `plugin_host_query` 调用方（在途表无记录说明已超时，静默丢弃）。
    fn query_respond(&self, payload: &Value) -> Result<Value> {
        let query_id = required_call_id(payload)?;
        let result = payload.get("result").cloned().unwrap_or(Value::Null);
        let sender = self
            .pending_queries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&query_id);
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
        Ok(Value::Null)
    }
}

/// 取载荷必填 callId/queryId（u64）。
fn required_call_id(payload: &Value) -> Result<u64> {
    payload
        .get("callId")
        .or_else(|| payload.get("queryId"))
        .and_then(Value::as_u64)
        .ok_or_else(|| PluginError::InvalidCall("missing u64 field: callId".to_string()))
}
