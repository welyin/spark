//! data.online* 能力（org-followups-batch3 §3）：QuickJS 后台运行时的
//! **在线** orgq 查询/写入——本机非驻留（普通成员读 data-accounts 集合）时，
//! 经 orgq-req 在线投递数据账号并等待应答，超时/失败回退缓存语义/离线入队
//! （与 Tauri 通路口径一致）。
//!
//! 既有同步 ops（`data.*`：缓存路由/离线入队）行为不变——online ops 是纯增量。
//!
//! 异步配对模式照搬 `sys.exec.start`（host_env/sys.rs）：host op 经
//! `runtime.spawn` + `spawn_blocking` 执行（插件 OS 线程零 block_on），完成
//! 经 `PluginEvent::Dispatch{kind: "data-online-result"}` 回流，prelude 按
//! callId 配对兑现 Promise。
//!
//! 投递编排复用内核下沉后的共享格自由函数（`kernel/data_orgq.rs` 的
//! `OrgqOnlineCtx` + `orgq_online_query`/`orgq_online_write`）——两侧资源
//! 形态不同、规则一份（F3 `OrgkeyDeliverCtx` 模式）。

use serde_json::Value;

use crate::kernel::data_orgq::orgq_online_target;
use crate::kernel::data_orgq::{OrgqOnlineCtx, orgq_online_query, orgq_online_write};
use crate::plugin::error::{PluginError, Result};
use crate::plugin::runtime::PluginEvent;

use super::{PluginHostShared, PluginRuntimeContext, required_str};

/// 在线投递应答事件 kind（prelude `settleAsync` 按 callId 配对）。
const ONLINE_RESULT_KIND: &str = "data-online-result";

impl PluginHostShared {
    /// 装配在线投递共享上下文（资源全部来自共享格；未解锁/未启动 p2p →
    /// None，调用方回退缓存/入队语义）。holder_domain 由调用方按插件域
    /// 补设（read-gate §3 查询侧 readAuth 的 holder 身份匹配口径）。
    fn online_ctx(&self) -> Option<OrgqOnlineCtx> {
        Some(OrgqOnlineCtx {
            storage: self.require_storage().ok()?,
            my_root_id: self
                .my_root_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()?,
            signing_key: self
                .signing_key
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()?,
            node: self
                .p2p_node
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()?,
            runtime: self.runtime.clone(),
            seed: self
                .seed_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            holder_domain: None,
        })
    }

    /// 在线目标数据账号（共享规则 `orgq_online_target`）：在线 peer 集经
    /// 节点命令通道取（spawn_blocking 上下文内 `Handle::block_on` 驱动，
    /// 与内核同步编排同口径——插件线程本身零 block_on）。
    fn online_target(ctx: &OrgqOnlineCtx, org_id: &str, col_full: &str) -> Option<String> {
        let online_peers = ctx
            .runtime
            .block_on(ctx.node.local_node_info())
            .ok()
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();
        orgq_online_target(
            &ctx.storage,
            &online_peers,
            org_id,
            col_full,
            &ctx.my_root_id,
        )
    }

    /// `data.onlineGet`：在线拉取单条（prefix=key 精确查询 → 读缓存）。
    pub(super) fn data_online_get(
        &self,
        rtx: &PluginRuntimeContext,
        payload: &Value,
    ) -> Result<Value> {
        let call_id = payload.get("callId").and_then(Value::as_u64).unwrap_or(0);
        let key = required_str(payload, "key")?.to_string();
        let (decl, _storage) = self.resolve_data_declaration(rtx.plugin_id.as_str(), payload)?;
        let host = self.clone();
        let event_tx = rtx.event_tx.clone();
        let domain = format!("plugin:{}", rtx.plugin_id);
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                host.online_get_work(&decl, &key, &domain)
            })
            .await;
            let mut out = result.unwrap_or_else(|e| serde_json::json!({ "value": Value::Null, "error": format!("onlineGet task join failed: {e}") }));
            out["callId"] = Value::from(call_id);
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: ONLINE_RESULT_KIND.to_string(),
                payload: out,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `data.onlineQuery`：在线前缀分页查询 → 读缓存页。
    pub(super) fn data_online_query(
        &self,
        rtx: &PluginRuntimeContext,
        payload: &Value,
    ) -> Result<Value> {
        let call_id = payload.get("callId").and_then(Value::as_u64).unwrap_or(0);
        let prefix = payload
            .get("prefix")
            .and_then(Value::as_str)
            .map(str::to_string);
        let limit = payload
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let cursor = payload
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let (decl, _storage) = self.resolve_data_declaration(rtx.plugin_id.as_str(), payload)?;
        let host = self.clone();
        let event_tx = rtx.event_tx.clone();
        let domain = format!("plugin:{}", rtx.plugin_id);
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                host.online_query_work(&decl, prefix.as_deref(), limit, cursor.as_deref(), &domain)
            })
            .await;
            let mut out = result.unwrap_or_else(|e| {
                serde_json::json!({ "items": [], "nextCursor": Value::Null, "error": format!("onlineQuery task join failed: {e}") })
            });
            out["callId"] = Value::from(call_id);
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: ONLINE_RESULT_KIND.to_string(),
                payload: out,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `data.onlineSave` / `data.onlineDelete`：在线写入受理（三态应答：
    /// `accepted` / `denied` / `queued`——超时/失败回退离线入队，与 Tauri
    /// 通路「不丢写」口径一致）。
    pub(super) fn data_online_save(
        &self,
        rtx: &PluginRuntimeContext,
        payload: &Value,
    ) -> Result<Value> {
        self.online_write(rtx, payload, false)
    }

    /// `data.onlineDelete`（见 onlineSave）。
    pub(super) fn data_online_delete(
        &self,
        rtx: &PluginRuntimeContext,
        payload: &Value,
    ) -> Result<Value> {
        self.online_write(rtx, payload, true)
    }

    fn online_write(
        &self,
        rtx: &PluginRuntimeContext,
        payload: &Value,
        is_delete: bool,
    ) -> Result<Value> {
        let call_id = payload.get("callId").and_then(Value::as_u64).unwrap_or(0);
        let key = required_str(payload, "key")?.to_string();
        let value = if is_delete {
            Value::Null
        } else {
            payload.get("value").cloned().unwrap_or(Value::Null)
        };
        let (decl, _storage) = self.resolve_data_declaration(rtx.plugin_id.as_str(), payload)?;
        let host = self.clone();
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || host.online_write_work(&decl, &key, &value))
                    .await;
            let mut out = result.unwrap_or_else(
                |e| serde_json::json!({ "error": format!("online write task join failed: {e}") }),
            );
            out["callId"] = Value::from(call_id);
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: ONLINE_RESULT_KIND.to_string(),
                payload: out,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    // ── spawn_blocking 内执行的工作体（同步上下文，可 Handle::block_on） ──

    /// onlineGet 工作体：本机驻留 → 本地读（委托 data_get 同规则）；非驻留 →
    /// 有在线数据账号投递查询后读缓存；超时/全离线 → 回退成员侧缓存。
    /// `domain` = 调用方插件域（credential 门禁集合的 readAuth holder 匹配口径）。
    fn online_get_work(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        domain: &str,
    ) -> Value {
        if decl.space != Some(crate::plugindata::Space::Org)
            || self.org_local_resident(decl).unwrap_or(true)
        {
            let raw = self.require_storage().and_then(|s| {
                crate::plugindata::get(&s, decl, key)
                    .map_err(|e| PluginError::InvalidCall(e.to_string()))
            });
            let value = raw
                .ok()
                .flatten()
                .map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text)));
            return serde_json::json!({ "value": value });
        }
        let oid = decl.org_id.clone().unwrap_or_default();
        let col_full = format!("{}@v{}", decl.name, decl.version);
        if let Some(mut ctx) = self.online_ctx()
            && let Some(target) = Self::online_target(&ctx, &oid, &col_full)
        {
            // 在线投递（prefix=key 精确拉取）——应答到达即落缓存；超时继续读旧缓存
            ctx.holder_domain = Some(domain.to_string());
            let _ = orgq_online_query(&ctx, &oid, decl, &target, Some(key), Some(1), None);
            if let Some(v) = self.orgq_cached_get(&ctx.storage, decl, key).ok().flatten() {
                return serde_json::json!({ "value": v });
            }
        }
        // 回退缓存语义（无缓存 → null = UnavailableOffline）
        let cached = self
            .require_storage()
            .ok()
            .and_then(|s| self.orgq_cached_get(&s, decl, key).ok().flatten());
        serde_json::json!({ "value": cached })
    }

    /// onlineQuery 工作体（同 onlineGet 的路由，返回分页）。
    fn online_query_work(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
        domain: &str,
    ) -> Value {
        if decl.space != Some(crate::plugindata::Space::Org)
            || self.org_local_resident(decl).unwrap_or(true)
        {
            let page = self.require_storage().and_then(|s| {
                crate::plugindata::query(&s, decl, prefix, limit, cursor)
                    .map_err(|e| PluginError::InvalidCall(e.to_string()))
            });
            return match page {
                Ok(p) => serde_json::json!({ "items": p.items, "nextCursor": p.next_cursor }),
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };
        }
        let oid = decl.org_id.clone().unwrap_or_default();
        let col_full = format!("{}@v{}", decl.name, decl.version);
        if let Some(mut ctx) = self.online_ctx() {
            if let Some(target) = Self::online_target(&ctx, &oid, &col_full) {
                ctx.holder_domain = Some(domain.to_string());
                let _ = orgq_online_query(&ctx, &oid, decl, &target, prefix, limit, cursor);
            }
            if let Ok(page) = self.orgq_cached_query(&ctx.storage, decl, prefix, limit, cursor) {
                return serde_json::json!({ "items": page.items, "nextCursor": page.next_cursor });
            }
        }
        let page = self
            .require_storage()
            .ok()
            .and_then(|s| self.orgq_cached_query(&s, decl, prefix, limit, cursor).ok());
        match page {
            Some(p) => serde_json::json!({ "items": p.items, "nextCursor": p.next_cursor }),
            None => serde_json::json!({ "items": [], "nextCursor": Value::Null }),
        }
    }

    /// onlineSave/onlineDelete 工作体（三态：accepted / denied / queued）。
    fn online_write_work(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        value: &Value,
    ) -> Value {
        // 本机驻留 → 本地写（委托 data_save/data_delete 的规则集；C7 后 orgd
        // 值恒为明文，此处直接落库）
        if decl.space != Some(crate::plugindata::Space::Org)
            || self.org_local_resident(decl).unwrap_or(true)
        {
            let result = self.require_storage().and_then(|mut s| {
                let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
                if value.is_null() {
                    crate::plugindata::del(&mut s, decl, key)
                } else {
                    crate::plugindata::save(&mut s, decl, key, &value.to_string())
                }
                .map_err(|e| PluginError::InvalidCall(e.to_string()))
            });
            return match result {
                Ok(()) => serde_json::json!({ "accepted": true }),
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };
        }
        let oid = decl.org_id.clone().unwrap_or_default();
        let col_full = format!("{}@v{}", decl.name, decl.version);
        if let Some(ctx) = self.online_ctx()
            && let Some(target) = Self::online_target(&ctx, &oid, &col_full)
        {
            match orgq_online_write(&ctx, &oid, decl, &target, key, value) {
                Ok(Some(true)) => return serde_json::json!({ "accepted": true }),
                Ok(Some(false)) => return serde_json::json!({ "denied": true }),
                _ => {} // 超时/失败 → 落入队
            }
        }
        // 全部数据账号离线 / 超时 → 离线入队（不丢写）
        self.orgq_enqueue_write(decl, key, value);
        serde_json::json!({ "queued": true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
    use crate::storage::StorageBackend;
    use std::sync::{Arc, Mutex};

    /// 测试宿主：tempdir sled（版本化句柄镜像）+ 每用例一条事件通道
    /// （返回 (host, 事件接收端)）。
    fn test_host() -> (
        PluginHostShared,
        std::sync::mpsc::Receiver<PluginEvent>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let sled = crate::storage::SledStorage::open(dir.path().join("db")).unwrap();
        let storage = crate::sync::versioned::VersionedStorage::new(
            crate::storage::Backend::Sled(sled),
            crate::sync::versioned::shared_node_id("node-test"),
        );
        let host = PluginHostShared {
            storage: Arc::new(Mutex::new(Some(storage))),
            io_lock: Arc::new(Mutex::new(())),
            event_tx: tokio::sync::broadcast::channel(16).0,
            my_root_id: Arc::new(Mutex::new(Some("me".repeat(32)))),
            p2p_node: Arc::new(Mutex::new(None)), // 无节点 → 无在线目标 → 回退语义
            signing_key: Arc::new(Mutex::new(None)),
            seed_shared: Arc::new(Mutex::new(None)),
            collection_configs: Arc::new(Mutex::new(Default::default())),
            pending_queries: Arc::new(Mutex::new(Default::default())),
            filter_caps: Arc::new(Mutex::new(Default::default())),
            feed_limiter: Arc::new(Mutex::new(Default::default())),
            app_msg_limiter: Arc::new(Mutex::new(Default::default())),
            runtime: tokio::runtime::Handle::current(),
        };
        let (_tx, rx) = std::sync::mpsc::channel();
        (host, rx, dir)
    }

    fn member(root_id: &str, role: OrganizationRole) -> OrganizationMember {
        OrganizationMember {
            root_id: root_id.to_string(),
            role,
            joined_at: 1000,
            added_by: "creator".to_string(),
            ..Default::default()
        }
    }

    /// 预制：org 记录（me=普通成员，da=管理员数据账号）+ 集合声明
    ///（org scope data-accounts）。返回 orgId。
    fn seed_org_decl(host: &PluginHostShared) -> String {
        let org_id = "org_0000000000000001".to_string();
        let record = OrganizationRecord {
            org_id: org_id.clone(),
            name: "t".to_string(),
            created_at: 1000,
            created_by: "creator".to_string(),
            updated_at: 1000,
            members: vec![
                member(&"me".repeat(32), OrganizationRole::Member),
                member(&"da".repeat(32), OrganizationRole::Admin),
            ],
            data_accounts: vec!["da".repeat(32)],
            ..Default::default()
        };
        let mut s = host.require_storage().unwrap();
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        crate::plugindata::declare(
            &mut s,
            "testplug",
            crate::plugindata::DeclareInput {
                name: "testplug:col".to_string(),
                version: Some("1".to_string()),
                space: Some(crate::plugindata::Space::Org),
                accounts: Some(crate::plugindata::Accounts::DataAccounts),
                scope: Some(crate::plugindata::Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(&org_id),
        )
        .unwrap();
        org_id
    }

    fn rtx(tx: std::sync::mpsc::Sender<PluginEvent>) -> PluginRuntimeContext {
        PluginRuntimeContext {
            plugin_id: "testplug".to_string(),
            event_tx: tx,
            permissions: vec!["storage:read".to_string(), "storage:write".to_string()],
        }
    }

    fn recv_result(rx: &std::sync::mpsc::Receiver<PluginEvent>) -> Value {
        let PluginEvent::Dispatch { kind, payload } = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("online result event")
        else {
            panic!("应回流 Dispatch 事件");
        };
        assert_eq!(kind, "data-online-result");
        payload
    }

    /// onlineSave：无在线数据账号（无节点）→ 回退离线入队（queued:true），
    /// 队列有条目（不丢写）。
    #[tokio::test(flavor = "multi_thread")]
    async fn online_save_falls_back_to_queue_when_offline() {
        let (host, rx, _dir) = test_host();
        let (tx, _) = std::sync::mpsc::channel::<PluginEvent>();
        let org_id = seed_org_decl(&host);
        let r = rtx(tx);
        // 注意：rtx 的 event_tx 须指向 rx 所在通道——重建配对
        let (tx2, rx2) = std::sync::mpsc::channel::<PluginEvent>();
        let r = PluginRuntimeContext { event_tx: tx2, ..r };
        host.data_online_save(
            &r,
            &serde_json::json!({"callId": 1, "name": "testplug:col", "key": "k1", "value": {"v": 1}, "orgId": org_id}),
        )
        .unwrap();
        let payload = recv_result(&rx2);
        assert_eq!(
            payload["queued"],
            serde_json::json!(true),
            "全离线 → 入队确认"
        );
        assert!(
            crate::sync::orgsync::orgq_queue_has_data(&host.require_storage().unwrap(), &org_id),
            "离线队列有条目（不丢写）"
        );
        drop(rx);
    }

    /// onlineGet：非驻留 + 无在线目标 → 回退成员侧缓存（缓存有值返回缓存值）。
    #[tokio::test(flavor = "multi_thread")]
    async fn online_get_falls_back_to_member_cache() {
        let (host, _rx, _dir) = test_host();
        let org_id = seed_org_decl(&host);
        // 预制成员侧缓存
        let rec = crate::sync::orgsync::OrgqRespRecord {
            key: format!("orgd:{org_id}:testplug:col@v1:k1"),
            value: serde_json::json!({"v": 42}),
            meta: Default::default(),
        };
        let mut s = host.require_storage().unwrap();
        s.put(
            &crate::sync::orgsync::orgq_cache_key(&org_id, "testplug:col@v1", "k1"),
            &serde_json::to_string(&rec).unwrap(),
        )
        .unwrap();
        drop(s);
        let (tx2, rx2) = std::sync::mpsc::channel::<PluginEvent>();
        host.data_online_get(
            &rtx(tx2),
            &serde_json::json!({"callId": 7, "name": "testplug:col", "key": "k1", "orgId": org_id}),
        )
        .unwrap();
        let payload = recv_result(&rx2);
        assert_eq!(
            payload["value"]["v"],
            serde_json::json!(42),
            "回退缓存返回缓存值"
        );
    }

    /// onlineGet：本机驻留（personal 集合）→ 本地直读。
    #[tokio::test(flavor = "multi_thread")]
    async fn online_get_local_resident_reads_local() {
        let (host, _rx, _dir) = test_host();
        // personal scope 集合（本地驻留）
        let mut s = host.require_storage().unwrap();
        crate::plugindata::declare(
            &mut s,
            "testplug",
            crate::plugindata::DeclareInput {
                name: "testplug:local".to_string(),
                version: Some("1".to_string()),
                ..Default::default()
            },
            1000,
            None,
        )
        .unwrap();
        let local_decl = crate::plugindata::resolve(&s, "testplug:local", Some("1")).unwrap();
        crate::plugindata::save(&mut s, &local_decl, "k9", "{\"n\":9}").unwrap();
        drop(s);
        let (tx2, rx2) = std::sync::mpsc::channel::<PluginEvent>();
        host.data_online_get(
            &rtx(tx2),
            &serde_json::json!({"callId": 9, "name": "testplug:local", "key": "k9"}),
        )
        .unwrap();
        let payload = recv_result(&rx2);
        assert_eq!(
            payload["value"]["n"],
            serde_json::json!(9),
            "驻留集合本地直读"
        );
    }
}
