//! O3 读路径透明路由（QuickJS 通路）：成员侧 orgq 缓存读取辅助。
//!
//! 数据账号离线时，普通成员对 org data-accounts 集合的读经成员侧缓存
//! （`orgq:cache:{orgId}:{collection}:`，可淘汰、不计副本）返回；无缓存回
//! UnavailableOffline。在线 orgq-req 的异步投递由宿主层接线，本模块只做
//! 纯逻辑缓存读取（存储泛型，可单测）。
//!
//! 从 `host_env.rs` 拆出的子模块（650 行硬线），共享父模块的
//! [`PluginHostShared`] 与错误类型。

use serde_json::Value;

use super::error::Result;
use super::host_env::PluginHostShared;

impl PluginHostShared {
    /// O3 读路径驻留判定（QuickJS 通路）：本机对某 org 集合是否本地驻留——
    /// 数据账号（data-accounts 天然驻留）或 all-members 集合（全员驻留）→
    /// 本地直读；普通成员对 data-accounts 恒非驻留（走 orgq 缓存路由）。
    pub(crate) fn org_local_resident(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
    ) -> Result<bool> {
        if decl.accounts == crate::plugindata::Accounts::AllMembers {
            return Ok(true);
        }
        let storage = self.require_storage()?;
        let my_root = self
            .my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let Some(oid) = decl.org_id.as_deref() else {
            return Ok(true);
        };
        let Ok(Some(record)) = crate::org::OrganizationService::get_record(&storage, oid) else {
            return Ok(false);
        };
        Ok(crate::org::roles::is_data_node(&record, &my_root))
    }

    /// O3 读路径（QuickJS 通路）：非数据账号成员读 org data-accounts 集合时
    /// 查成员侧缓存（`orgq:cache:`）。命中返回缓存值；未命中返回 `None`。
    pub(crate) fn orgq_cached_get<S: crate::storage::StorageBackend>(
        &self,
        storage: &S,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
    ) -> Result<Option<Value>> {
        let oid = decl.org_id.as_deref().unwrap_or("");
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let relative = key
            .strip_prefix(&crate::plugindata::org_data_prefix(
                oid,
                &decl.name,
                &decl.version,
            ))
            .unwrap_or(key);
        let cache_key = crate::sync::orgsync::orgq_cache_key(oid, &col_full, relative);
        let raw = storage
            .get(&cache_key)
            .map_err(|e| super::error::PluginError::InvalidCall(e.to_string()))?;
        Ok(raw.map(|r| {
            // 缓存值 = OrgqRespRecord JSON，取其中 value 字段（C7 后恒为明文）
            serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&r)
                .map(|rec| rec.value)
                .unwrap_or_else(|_| serde_json::from_str(&r).unwrap_or(Value::Null))
        }))
    }

    /// O3 读路径（QuickJS 通路）：成员侧缓存前缀扫描，返回 QueryPage。
    pub(crate) fn orgq_cached_query<S: crate::storage::StorageBackend>(
        &self,
        storage: &S,
        decl: &crate::plugindata::CollectionDeclaration,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<crate::plugindata::QueryPage> {
        let oid = decl.org_id.as_deref().unwrap_or("");
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let cache_prefix = crate::sync::orgsync::orgq_cache_prefix(oid, &col_full);
        let match_prefix = match prefix {
            Some(p) => format!("{cache_prefix}{p}"),
            None => cache_prefix.clone(),
        };
        let scan_from = match cursor {
            Some(c) => format!("{cache_prefix}{c}\u{0}"),
            None => match_prefix.clone(),
        };
        let limit = limit
            .unwrap_or(crate::sync::orgsync::ORGQ_LIMIT_DEFAULT)
            .clamp(1, crate::sync::orgsync::ORGQ_LIMIT_MAX);
        let options = crate::storage::ScanOptions {
            prefix: match_prefix,
            start: Some(scan_from),
            end: None,
            limit: Some(limit),
            reverse: false,
        };
        let mut page = crate::plugindata::QueryPage::default();
        let scanned = storage
            .scan(&options)
            .map_err(|e| super::error::PluginError::InvalidCall(e.to_string()))?;
        for (key, raw) in scanned {
            let relative = key[cache_prefix.len()..].to_string();
            let value = serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&raw)
                .map(|rec| rec.value.to_string())
                .unwrap_or_else(|_| raw.clone());
            page.items.push((relative.clone(), value));
            if page.items.len() >= limit {
                page.next_cursor = Some(relative);
                break;
            }
        }
        Ok(page)
    }
}

impl PluginHostShared {
    /// 解析声明轴载荷（缺省轴取模块缺省值）。
    /// B5：解析 space/accounts/confidentiality（org scope 声明轴）。
    /// 从 `host_env.rs` 移入（Z5 650 行硬线：O3 新增 org 轴解析独立成文件）。
    pub(crate) fn parse_declare_input(payload: &Value) -> Result<crate::plugindata::DeclareInput> {
        use crate::plugindata::{
            Accounts, Confidentiality, DeclareInput, Devices, MergeRule, Scope, Sensitivity, Space,
        };
        let name = required_str(payload, "name")?.to_string();
        let version = payload
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string);
        let parse_axis =
            |field: &str| -> Option<&str> { payload.get(field).and_then(Value::as_str) };
        let scope = match parse_axis("scope") {
            None => None,
            Some("sync") => Some(Scope::Sync),
            Some("local") => Some(Scope::Local),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "scope must be 'sync' or 'local', got {other:?}"
                )));
            }
        };
        let space = match parse_axis("space") {
            None => None,
            Some("personal") => Some(Space::Personal),
            Some("org") => Some(Space::Org),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "space must be 'personal' or 'org', got {other:?}"
                )));
            }
        };
        let accounts = match parse_axis("accounts") {
            None => None,
            Some("all-members") => Some(Accounts::AllMembers),
            Some("data-accounts") => Some(Accounts::DataAccounts),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "accounts must be 'all-members' or 'data-accounts', got {other:?}"
                )));
            }
        };
        let confidentiality = match parse_axis("confidentiality") {
            None => None,
            Some("filtered") => Some(Confidentiality::Filtered),
            // C7：encrypted 轴已退役——显式传入即报错（不再静默忽略，防插件
            // 误以为集合级加密仍生效）
            Some("encrypted") => {
                return Err(super::error::PluginError::InvalidCall(
                    "confidentiality 'encrypted' is retired (C7): org collections are plaintext-filtered only".to_string(),
                ));
            }
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "confidentiality must be 'filtered', got {other:?}"
                )));
            }
        };
        let devices = match parse_axis("devices") {
            None => None,
            Some("all") => Some(Devices::All),
            Some("pc-backup") => Some(Devices::PcBackup),
            Some("pc-only") => Some(Devices::PcOnly),
            Some("mobile-only") => Some(Devices::MobileOnly),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "devices must be one of all/pc-backup/pc-only/mobile-only, got {other:?}"
                )));
            }
        };
        let sensitivity = match parse_axis("sensitivity") {
            None => None,
            Some("normal") => Some(Sensitivity::Normal),
            Some("sensitive") => Some(Sensitivity::Sensitive),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "sensitivity must be 'normal' or 'sensitive', got {other:?}"
                )));
            }
        };
        let merge = match parse_axis("merge") {
            None => None,
            Some("lww-record") => Some(MergeRule::LwwRecord),
            Some("append-only") => Some(MergeRule::AppendOnly),
            Some("whole") => Some(MergeRule::Whole),
            Some(other) => {
                return Err(super::error::PluginError::InvalidCall(format!(
                    "merge must be one of lww-record/append-only/whole, got {other:?}"
                )));
            }
        };
        // readPolicy（read-gate §2，org scope 专有）：结构按内核 ReadPolicy
        // 线形反序列化（缺省键省略 = members 现状）；取值合法性（org 空间
        // 限定 + credential 必填面）由 plugindata::declare 录入校验兜底。
        let read_policy = match payload.get("readPolicy") {
            None | Some(Value::Null) => None,
            Some(v) => serde_json::from_value::<crate::plugindata::ReadPolicy>(v.clone())
                .map(Some)
                .map_err(|e| {
                    super::error::PluginError::InvalidCall(format!("invalid readPolicy: {e}"))
                })?,
        };
        Ok(DeclareInput {
            name,
            version,
            scope,
            space,
            accounts,
            confidentiality,
            sensitivity,
            devices,
            merge,
            declared_by: payload
                .get("declaredBy")
                .and_then(Value::as_str)
                .map(str::to_string),
            read_policy,
        })
    }

    /// `data.onReadFilter`：记录集合注册了读过滤钩子（prelude 侧已存 readFilters）。
    /// 集合名取自声明（`data.declareCollection` 的 name，不含 @v 代际）。
    /// 从 `host_env.rs` 移入（Z5 650 行硬线）。
    pub(crate) fn data_on_read_filter(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let collection = required_str(payload, "collection")?.to_string();
        if !collection.starts_with(&format!("{plugin_id}:")) {
            return Err(super::error::PluginError::InvalidCall(format!(
                "collection {collection:?} does not belong to plugin {plugin_id}"
            )));
        }
        self.remember_filter_cap(&collection, "read");
        Ok(Value::Null)
    }

    /// `data.onWriteFilter`：记录集合注册了写过滤钩子。
    pub(crate) fn data_on_write_filter(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let collection = required_str(payload, "collection")?.to_string();
        if !collection.starts_with(&format!("{plugin_id}:")) {
            return Err(super::error::PluginError::InvalidCall(format!(
                "collection {collection:?} does not belong to plugin {plugin_id}"
            )));
        }
        self.remember_filter_cap(&collection, "write");
        Ok(Value::Null)
    }

    /// 合并过滤种类到 filter_caps（read/write/read-write）。
    pub(crate) fn remember_filter_cap(&self, collection: &str, kind: &str) {
        let mut caps = self.filter_caps.lock().unwrap_or_else(|e| e.into_inner());
        let cur = caps.get(collection).cloned().unwrap_or_default();
        let merged = if cur.contains(kind) {
            cur
        } else {
            format!("{cur}{kind}")
        };
        caps.insert(collection.to_string(), merged);
    }
}

/// 取载荷必填字符串字段（缺失/非字符串为非法调用）。
fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload.get(field).and_then(Value::as_str).ok_or_else(|| {
        super::error::PluginError::InvalidCall(format!("missing string field: {field}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{MemoryStorage, StorageBackend};

    fn decl(org_id: &str) -> crate::plugindata::CollectionDeclaration {
        crate::plugindata::CollectionDeclaration {
            name: "ai-chat:finance".to_string(),
            version: "1.0.0".to_string(),
            scope: crate::plugindata::Scope::Sync,
            space: Some(crate::plugindata::Space::Org),
            accounts: crate::plugindata::Accounts::DataAccounts,
            devices: crate::plugindata::Devices::All,
            confidentiality: crate::plugindata::Confidentiality::Filtered,
            sensitivity: crate::plugindata::Sensitivity::default(),
            merge: crate::plugindata::MergeRule::LwwRecord,
            declared_at: 0,
            declared_by: None,
            ts: None,
            org_id: Some(org_id.to_string()),
            read_policy: None,
        }
    }

    fn bare_host() -> PluginHostShared {
        PluginHostShared {
            storage: std::sync::Arc::new(std::sync::Mutex::new(None)),
            io_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
            event_tx: tokio::sync::broadcast::channel(16).0,
            my_root_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
            p2p_node: std::sync::Arc::new(std::sync::Mutex::new(None)),
            signing_key: std::sync::Arc::new(std::sync::Mutex::new(None)),
            seed_shared: std::sync::Arc::new(std::sync::Mutex::new(None)),
            collection_configs: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            pending_queries: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            filter_caps: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            feed_limiter: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            app_msg_limiter: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// readPolicy 声明入参解析（read-gate §2）：线形键 readPolicy → DeclareInput
    /// .read_policy；缺省/显式 null → None（members 缺省，向后兼容）；结构非法
    /// → InvalidCall 早失败。
    #[test]
    fn parse_declare_input_parses_read_policy() {
        // 缺省 → None（members 现状）
        let input = PluginHostShared::parse_declare_input(&serde_json::json!({
            "name": "ai-chat:c"
        }))
        .unwrap();
        assert!(input.read_policy.is_none(), "缺省 readPolicy → None");
        // 显式 null → None
        let input = PluginHostShared::parse_declare_input(&serde_json::json!({
            "name": "ai-chat:c", "readPolicy": null
        }))
        .unwrap();
        assert!(input.read_policy.is_none());
        // credential 门禁线形透传（kind/credTypes/verifierDomain/policyRef）
        let input = PluginHostShared::parse_declare_input(&serde_json::json!({
            "name": "ai-chat:c",
            "space": "org",
            "readPolicy": {
                "kind": "credential",
                "credTypes": ["member", "resident"],
                "verifierDomain": "org_abababababababababababababababababababababababababababababababab",
                "policyRef": null
            }
        }))
        .unwrap();
        let policy = input.read_policy.expect("readPolicy 解析入 input");
        assert_eq!(policy.kind, crate::plugindata::ReadPolicyKind::Credential);
        assert_eq!(policy.cred_types, vec!["member", "resident"]);
        assert_eq!(
            policy.verifier_domain,
            "org_abababababababababababababababababababababababababababababababab"
        );
        assert_eq!(policy.policy_ref, None);
        // 结构非法（缺 kind）→ InvalidCall
        assert!(
            PluginHostShared::parse_declare_input(&serde_json::json!({
                "name": "ai-chat:c",
                "readPolicy": { "credTypes": ["member"] }
            }))
            .is_err(),
            "readPolicy 结构非法早失败"
        );
    }

    /// O3 读路径透明路由（QuickJS 通路）：`orgq_cached_get` 从成员侧缓存
    /// `orgq:cache:{orgId}:{collection}:` 读取——命中返回缓存值，未命中返回
    /// `None`（UnavailableOffline 语义）。
    #[tokio::test(flavor = "multi_thread")]
    async fn orgq_cached_get_reads_member_cache() {
        let host = bare_host();
        let mut s = MemoryStorage::new();
        let org_id = "org_0000000000000001";
        let col = "ai-chat:finance@v1.0.0";
        let decl = decl(org_id);
        // 未命中 → None
        assert!(host.orgq_cached_get(&s, &decl, "k1").unwrap().is_none());
        // 命中 → 返回缓存值
        let rec = crate::sync::orgsync::OrgqRespRecord {
            key: format!("orgd:{org_id}:{col}:k1"),
            value: serde_json::json!({"amt": 5}),
            meta: Default::default(),
        };
        s.put(
            &crate::sync::orgsync::orgq_cache_key(org_id, col, "k1"),
            &serde_json::to_string(&rec).unwrap(),
        )
        .unwrap();
        let got = host.orgq_cached_get(&s, &decl, "k1").unwrap().unwrap();
        assert_eq!(got["amt"], serde_json::json!(5), "命中返回缓存值");
    }

    /// O3 读路径透明路由（QuickJS 通路）：`orgq_cached_query` 前缀扫描成员侧
    /// 缓存并返回分页（相对键 → 缓存值）。
    #[tokio::test(flavor = "multi_thread")]
    async fn orgq_cached_query_scans_member_cache() {
        let host = bare_host();
        let mut s = MemoryStorage::new();
        let org_id = "org_0000000000000001";
        let col = "ai-chat:finance@v1.0.0";
        let decl = decl(org_id);
        let rec = |rel: &str, v: i64| crate::sync::orgsync::OrgqRespRecord {
            key: format!("orgd:{org_id}:{col}:{rel}"),
            value: serde_json::json!({"v": v}),
            meta: Default::default(),
        };
        s.put(
            &crate::sync::orgsync::orgq_cache_key(org_id, col, "k1"),
            &serde_json::to_string(&rec("k1", 1)).unwrap(),
        )
        .unwrap();
        s.put(
            &crate::sync::orgsync::orgq_cache_key(org_id, col, "k2"),
            &serde_json::to_string(&rec("k2", 2)).unwrap(),
        )
        .unwrap();
        let page = host
            .orgq_cached_query(&s, &decl, None, Some(10), None)
            .unwrap();
        assert_eq!(page.items.len(), 2, "缓存前缀扫描返回两条");
        assert_eq!(page.items[0].0, "k1");
    }
}
