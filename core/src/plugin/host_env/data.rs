//! data.* 插件数据能力（从 `host_env/docs` 拆出，文件长度硬线）：集合声明、
//! 数据读写/查询/版本丢弃、blob 读写、feed-blob 拉取、orgq 路由写入。
//! 零逻辑变化。

use serde_json::Value;

use crate::collection::{FilterOp, QueryFilter, QueryOptions};
use crate::p2p::node::system_now_ms;

use crate::plugin::error::{PluginError, Result};

use super::{PluginHostShared, required_str};

impl PluginHostShared {
    pub(super) fn data_declare_collection(
        &self,
        plugin_id: &str,
        payload: &Value,
    ) -> Result<Value> {
        let mut input = Self::parse_declare_input(payload)?;
        // F10：declaredBy 防伪造——kernel 侧强制覆盖为调用方 rootId，不信任
        // 插件自报（声明记录属审计面）。
        let my_root = self
            .my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        input.declared_by = Some(my_root.clone());
        // org space：payload 中可携带 orgId（由内核按插件运行空间传入）
        let org_id = payload.get("orgId").and_then(Value::as_str);
        // B5：org 空间声明须校验调用方确为该组织成员（orgId 来源可信）——
        // 防止插件以任意 org_id 越权声明组织集合。校验失败以 InvalidCall 拒绝。
        if input.space == Some(crate::plugindata::Space::Org) {
            let Some(oid) = org_id else {
                return Err(PluginError::InvalidCall(
                    "org space declaration requires orgId".to_string(),
                ));
            };
            let my_root = self
                .my_root_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let storage = self.require_storage()?;
            let is_member = crate::org::OrganizationService::get_record(&storage, oid)
                .ok()
                .flatten()
                .is_some_and(|rec| rec.find_member(&my_root).is_some());
            if !is_member {
                return Err(PluginError::InvalidCall(format!(
                    "caller {my_root} is not a member of org {oid}"
                )));
            }
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let decl =
            crate::plugindata::declare(&mut storage, plugin_id, input, system_now_ms(), org_id)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        serde_json::to_value(&decl).map_err(Into::into)
    }

    /// O3 写路径是否应走 orgq 路由（F1）：本机**非数据账号**写 org data-accounts
    /// 集合 → 不得直接落 `orgd:` 孤儿副本，须经 orgq 离线入队 / 在线经桥转发。
    /// 插件线程不能 `block_on`，故 QuickJS 通路采用「离线入队」路径：数据账号
    /// 上线后经统一同步链路冲刷（与 Kernel `data_org_write_route::Enqueued`
    /// 同收敛点）。本机数据账号 / all-members 集合 → 本地落库。
    pub(super) fn orgq_route_write(&self, decl: &crate::plugindata::CollectionDeclaration) -> bool {
        if decl.space != Some(crate::plugindata::Space::Org)
            || decl.accounts != crate::plugindata::Accounts::DataAccounts
        {
            return false;
        }
        // 非驻留（非数据账号）→ 走 orgq；驻留 → 本地
        !self.org_local_resident(decl).unwrap_or(false)
    }

    /// F1：org data-accounts 且本机非数据账号 → orgq 离线入队（不落 orgd 副本）。
    /// 入队用相对 key，value 为 null 即删除（与 Kernel 侧 data_org_enqueue 同口径）。
    pub(super) fn orgq_enqueue_write(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        value: &Value,
    ) {
        let Some(oid) = decl.org_id.as_deref() else {
            return;
        };
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let relative = key
            .strip_prefix(&crate::plugindata::org_data_prefix(
                oid,
                &decl.name,
                &decl.version,
            ))
            .unwrap_or(key);
        if let Ok(storage) = self.require_storage().map(|s| s.clone()) {
            let _ = crate::sync::orgsync::orgq_queue_put(
                &mut storage.clone(),
                oid,
                &col_full,
                relative,
                value,
            );
        }
    }

    pub(super) fn data_save(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let value = payload.get("value").cloned().unwrap_or(Value::Null);
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // F1：org data-accounts 非数据账号 → 走 orgq 离线入队（不落孤儿副本）
        if self.orgq_route_write(&decl) {
            self.orgq_enqueue_write(&decl, key, &value);
            return Ok(Value::Null);
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = storage;
        crate::plugindata::save(&mut storage, &decl, key, &value.to_string())
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    pub(super) fn data_delete(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // F1：org data-accounts 非数据账号 → 删除走 orgq 离线入队（value:null 墓碑）
        if self.orgq_route_write(&decl) {
            self.orgq_enqueue_write(&decl, key, &Value::Null);
            return Ok(Value::Null);
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = storage;
        crate::plugindata::del(&mut storage, &decl, key)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    pub(super) fn data_get(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // O3 读路径透明路由（QuickJS 通路）：org data-accounts 集合对本机
        // 非数据账号 → 走成员侧缓存（数据账号离线回缓存；在线 orgq 由宿主
        // 接线）。本机数据账号 / all-members 集合直接读本地。
        if decl.space == Some(crate::plugindata::Space::Org) && !self.org_local_resident(&decl)? {
            if let Some(v) = self.orgq_cached_get(&storage, &decl, key)? {
                return Ok(v);
            }
            // 无缓存 → UnavailableOffline 语义（回 Null，UI 标注不可得）
            return Ok(Value::Null);
        }
        let raw = crate::plugindata::get(&storage, &decl, key)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(match raw {
            // C7 后 orgd 值恒为明文（encrypted 轴已退役）
            Some(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
            None => Value::Null,
        })
    }

    pub(super) fn data_query(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        let prefix = payload.get("prefix").and_then(Value::as_str);
        let limit = payload
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let cursor = payload.get("cursor").and_then(Value::as_str);
        // O3 读路径透明路由（QuickJS 通路）：同 data_get 的 org 缓存路由。
        let page = if decl.space == Some(crate::plugindata::Space::Org)
            && !self.org_local_resident(&decl)?
        {
            self.orgq_cached_query(&storage, &decl, prefix, limit, cursor)?
        } else {
            crate::plugindata::query(&storage, &decl, prefix, limit, cursor)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?
        };
        let mut value = serde_json::json!({
            "items": page.items.iter().map(|(key, raw)| {
                serde_json::json!({
                    "key": key,
                    "value": serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone())),
                })
            }).collect::<Vec<_>>(),
        });
        if let Some(next) = page.next_cursor {
            value["nextCursor"] = Value::String(next);
        }
        Ok(value)
    }

    pub(super) fn data_drop_version(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        required_str(payload, "version")?; // drop 必须显式指定代际（不容许误清最新）
        let (decl, mut storage) = self.resolve_data_declaration(plugin_id, payload)?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        crate::plugindata::drop_version(&mut storage, &decl)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    /// blob 保存：base64 入、内容哈希出（引用对象由插件自行写入记录：
    /// `{ "$blob": hash, name, size, mime }`）。
    pub(super) fn data_save_blob(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
        let data_b64 = required_str(payload, "data")?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| PluginError::InvalidCall(format!("invalid base64: {e}")))?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let info = crate::plugindata::blob::save_blob(&mut storage, &bytes)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        serde_json::to_value(&info).map_err(Into::into)
    }

    /// blob 读取：命中 → `{status:"ready", data(base64)}`；未命中 → 置 want
    /// 标记（lazy 拉取意图，pdsync hello 调和时向在线自设备拉取）并回
    /// `{status:"pending"}`——调用方稍后重读（前端建议轮询/重试）。
    ///
    /// **B3 feed-blob 请求方发起链路**：未命中时若该 hash 有 feed 来源登记
    /// （`feed::blob_source` → fromRootId），向来源 rootId 发首块 `feed-blob-req`
    /// （offset 0，经 `blob::throttle_request` 节流）；无来源登记维持现状
    /// （pdsync 自设备拉取）。
    pub(super) fn data_read_blob(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
        let hash = required_str(payload, "hash")?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        if let Some(data) = crate::plugindata::blob::read_blob(&storage, hash)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?
        {
            return Ok(serde_json::json!({ "status": "ready", "data": data }));
        }
        // B3：查 feed 来源登记 → 有则出站首块 feed-blob-req（经节流）
        let now = system_now_ms();
        let from_source = crate::kernel::blob_source(&storage, hash, now)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        if let Some(from_root_id) = from_source {
            self.request_feed_blob(&mut storage, hash, &from_root_id);
        }
        crate::plugindata::blob::mark_want(&mut storage, hash)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(serde_json::json!({ "status": "pending" }))
    }

    /// B3：向 feed 来源 rootId 出站首块 `feed-blob-req`（offset 0，跨联系人
    /// 分块传输通道）。经 `blob::throttle_request` 逐 hash 节流（服务方同口径），
    /// 命中节流跳过；无寻址对端/未解锁等静默跳过（由 pdsync want 兜底）。
    /// spawn 到内核 runtime 投递，失败静默（请求方由 `feed-blob-resp` 续拉/
    /// 下轮调和重试）。
    pub(super) fn request_feed_blob(
        &self,
        storage: &mut crate::kernel::KernelStorage,
        hash: &str,
        from_root_id: &str,
    ) {
        use crate::kernel::dm_envelope::{KIND_FEED_BLOB_REQ, build_envelope};
        // 逐 hash 节流（§19.6）：距上次请求不足 BLOB_REQ_THROTTLE_MS 跳过
        let now = system_now_ms();
        if !crate::plugindata::blob::throttle_request(storage, hash, now).unwrap_or(false) {
            return;
        }
        // 解析来源 rootId 的对端（friend 记录择优；无可寻址端点跳过）
        let Some(peer) = crate::kernel::resolve_feed_recipient_peer_shared(storage, from_root_id)
            .ok()
            .flatten()
        else {
            return;
        };
        // 本机 rootId + 签名私钥
        let (Some(my_root_id), Some(signing_key)) = (
            self.my_root_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            self.signing_key
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        ) else {
            return;
        };
        let Some(node) = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let envelope = build_envelope(
            KIND_FEED_BLOB_REQ,
            &my_root_id,
            from_root_id,
            now,
            serde_json::json!({ "hash": hash, "offset": 0 }),
            &signing_key,
        );
        self.runtime.spawn(async move {
            let _ = node.dm_direct(&peer, envelope).await;
        });
    }

    /// 查询参数载荷（camelCase，对齐壳层 QueryOptionsDto）。
    pub(super) fn parse_query_options(payload: &Value) -> Result<QueryOptions> {
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
