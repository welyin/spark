//! pluginDocs 随组织同步（对齐 desktop/src/main/p2p/plugin-org-sync.ts）。
//!
//! 收集：扫 `doc:plugin:` 前缀键（键形 `doc:<domain=plugin:*>:<collection>:<id>`），
//! 仅取 `payload.orgId === 目标 orgId` 且未标记同步禁用的文档；meta 从本地
//! meta 键读取（须有 vv 与 ts），schema 从集合策略注册表读取。
//! 应用：逐条 `applyRemoteUpdate`（org.md §11）。
//!
//! 挂载点（网络侧组装，属 p2p 模块）：org-share `payload.pluginDocs`、
//! org-pull-org 响应 `pluginDocs`。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::{CollectionSchemaDeclaration, get_collection_schema};
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::{
    ApplyRemoteOptions, CollectionAdapter, RemoteMeta, apply_remote_update, get_meta,
};

use super::Result;

/// 插件文档键前缀（plugin-org-sync.ts:16）。
pub const PLUGIN_DOC_PREFIX: &str = "doc:plugin:";

/// 随组织同步的插件文档条目（plugin-org-sync.ts:7-14）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginDocSyncItem {
    /// 插件域（`plugin:*`）。
    pub domain: String,
    /// 集合名。
    pub collection: String,
    /// 文档 id。
    pub id: String,
    /// 文档内容。
    pub payload: Value,
    /// 同步 meta（`{vv, ts, nodeId?}`）。
    pub meta: RemoteMeta,
    /// 集合策略声明（供接收方按相同策略应用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<CollectionSchemaDeclaration>,
}

/// `parsePluginDocKey`（plugin-org-sync.ts:18-29）：
/// `^doc:(plugin:[^:]+):([^:]+):(.+)$`。
pub fn parse_plugin_doc_key(key: &str) -> Option<(String, String, String)> {
    let rest = key.strip_prefix("doc:plugin:")?;
    // domain = "plugin:" + 一段非冒号字符（`plugin:[^:]+`）
    let (domain_tail, rest) = rest.split_once(':')?;
    if domain_tail.is_empty() {
        return None;
    }
    let (collection, id) = rest.split_once(':')?;
    if collection.is_empty() || id.is_empty() {
        return None;
    }
    Some((
        format!("plugin:{domain_tail}"),
        collection.to_string(),
        id.to_string(),
    ))
}

/// `isSyncDisabled`（plugin-org-sync.ts:31-48）：`__sync === false`，或
/// `__sync.disabled === true`，或 `mode`/`strategy` ∈ {local, none, disabled}。
pub fn is_sync_disabled(payload: &Value) -> bool {
    let Some(marker) = payload.get("__sync") else {
        return false;
    };
    if marker.is_boolean() {
        return marker.as_bool() == Some(false);
    }
    let Some(sync) = marker.as_object() else {
        return false;
    };
    if sync.get("disabled").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    // JS `String(sync.mode ?? sync.strategy ?? '')`：非字符串标量按 JS 强制转换，
    // 对象/数组 → "[object Object]"/逗号拼接，均不可能等于三个禁用词，按 "" 处理
    let mode = sync
        .get("mode")
        .or_else(|| sync.get("strategy"))
        .map(js_string_coercion)
        .unwrap_or_default();
    let mode = mode.trim().to_lowercase();
    mode == "local" || mode == "none" || mode == "disabled"
}

/// JS `String(value)` 的最小子集：字符串原样、bool/null/number 按 JS 转换；
/// 对象与数组不会匹配禁用词，归一为 ""（不影响判定结果）。
fn js_string_coercion(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Number(n) => crate::evidence::js_number_to_string(n.as_f64().unwrap_or(f64::NAN)),
        _ => String::new(),
    }
}

/// `resolveOrgId`（plugin-org-sync.ts:50-56）：`payload.orgId` 为字符串时 trim，否则 `""`。
pub fn resolve_org_id(payload: &Value) -> String {
    payload
        .get("orgId")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// `collectSyncablePluginDocsByOrg`（plugin-org-sync.ts:58-124）：
/// 扫 `doc:plugin:` 前缀，收集目标组织的可同步文档。
///
/// - 键形不符 / JSON 损坏 / orgId 不匹配 / 标记同步禁用 → 跳过
/// - meta 缺失或缺 vv/ts → 跳过（`get_meta` 解析失败同样跳过）
/// - schema 从本地集合策略注册表读取（有则携带）
pub fn collect_syncable_plugin_docs<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Result<Vec<PluginDocSyncItem>> {
    let target_org_id = org_id.trim();
    if target_org_id.is_empty() {
        return Ok(Vec::new());
    }

    let rows = storage.scan(&ScanOptions::prefix(PLUGIN_DOC_PREFIX))?;
    let mut results = Vec::new();
    for (key, value) in rows {
        let Some((domain, collection, id)) = parse_plugin_doc_key(&key) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&value) else {
            continue;
        };
        if resolve_org_id(&payload) != target_org_id {
            continue;
        }
        if is_sync_disabled(&payload) {
            continue;
        }
        // meta 须有 vv 与 ts（plugin-org-sync.ts:93-96）；DocMeta 两者恒在，
        // 解析失败即视为缺失
        let Ok(Some(meta)) = get_meta(storage, &domain, &collection, &id) else {
            continue;
        };
        let schema = get_collection_schema(storage, &domain, &collection)
            .ok()
            .flatten()
            .map(|record| CollectionSchemaDeclaration {
                sync_strategy: record.sync_strategy,
                governance: record.governance,
                enable_evidence: record.enable_evidence,
            });
        results.push(PluginDocSyncItem {
            domain,
            collection,
            id,
            payload,
            meta: RemoteMeta {
                vv: meta.vv,
                ts: meta.ts,
                node_id: meta.node_id,
            },
            schema,
        });
    }
    Ok(results)
}

/// `applyPluginDocSyncItems`（plugin-org-sync.ts:126-147）：逐条
/// `applyRemoteUpdate`，返回应用条数。
///
/// 集合适配器由调用方按 (domain, collection) 构造（TS 为
/// `new DocumentCollection(db, domain, collection, {})`）。
pub fn apply_plugin_doc_sync_items<S, A>(
    storage: &mut S,
    items: &[PluginDocSyncItem],
    mut adapter_for: impl FnMut(&str, &str) -> A,
    now_ms: i64,
) -> Result<usize>
where
    S: StorageBackend,
    A: CollectionAdapter,
{
    let mut applied = 0;
    for item in items {
        let adapter = adapter_for(&item.domain, &item.collection);
        apply_remote_update(
            storage,
            &adapter,
            &item.domain,
            &item.collection,
            &item.id,
            Some(&item.payload),
            &item.meta,
            ApplyRemoteOptions {
                schema: item.schema.clone(),
                watermark: None,
                now_ms,
            },
        )
        .map_err(|e| super::OrgError::Malformed(format!("apply plugin doc: {e}")))?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::{DocMeta, meta_key};
    use serde_json::json;

    #[test]
    fn parse_key_shapes() {
        assert_eq!(
            parse_plugin_doc_key("doc:plugin:chat:messages:m1"),
            Some((
                "plugin:chat".to_string(),
                "messages".to_string(),
                "m1".to_string()
            ))
        );
        // id 可含冒号（`.+`）
        assert_eq!(
            parse_plugin_doc_key("doc:plugin:chat:messages:a:b:c"),
            Some(("plugin:chat".to_string(), "messages".to_string(), "a:b:c".to_string()))
        );
        assert_eq!(parse_plugin_doc_key("doc:plugin::messages:m1"), None);
        assert_eq!(parse_plugin_doc_key("doc:plugin:chat::m1"), None);
        assert_eq!(parse_plugin_doc_key("doc:plugin:chat:messages:"), None);
        assert_eq!(parse_plugin_doc_key("doc:core:messages:m1"), None);
        assert_eq!(parse_plugin_doc_key("plugin:chat:messages:m1"), None);
        assert_eq!(parse_plugin_doc_key(""), None);
    }

    #[test]
    fn sync_disabled_rules() {
        assert!(is_sync_disabled(&json!({"__sync": false})));
        assert!(!is_sync_disabled(&json!({"__sync": true})));
        assert!(is_sync_disabled(&json!({"__sync": {"disabled": true}})));
        assert!(!is_sync_disabled(&json!({"__sync": {"disabled": false}})));
        for mode in ["local", "none", "disabled", " LOCAL ", "None"] {
            assert!(
                is_sync_disabled(&json!({"__sync": {"mode": mode}})),
                "mode={mode}"
            );
            assert!(
                is_sync_disabled(&json!({"__sync": {"strategy": mode}})),
                "strategy={mode}"
            );
        }
        assert!(!is_sync_disabled(&json!({"__sync": {"mode": "lww"}})));
        assert!(!is_sync_disabled(&json!({"__sync": {"mode": 0}})));
        assert!(!is_sync_disabled(&json!({"__sync": "nonsense"})));
        assert!(!is_sync_disabled(&json!({})));
        // mode 优先于 strategy（JS ?? 语义）
        assert!(!is_sync_disabled(&json!({"__sync": {"mode": "lww", "strategy": "local"}})));
    }

    #[test]
    fn resolve_org_id_trims_and_defaults() {
        assert_eq!(resolve_org_id(&json!({"orgId": " org_x "})), "org_x");
        assert_eq!(resolve_org_id(&json!({"orgId": 42})), "");
        assert_eq!(resolve_org_id(&json!({})), "");
    }

    fn put_doc(storage: &mut MemoryStorage, domain: &str, collection: &str, id: &str, payload: &Value, with_meta: bool) {
        storage
            .put(
                &format!("doc:{domain}:{collection}:{id}"),
                &serde_json::to_string(payload).unwrap(),
            )
            .unwrap();
        if with_meta {
            storage
                .put(
                    &meta_key(domain, collection, id),
                    &serde_json::to_string(&DocMeta {
                        vv: [("node1".to_string(), 3)].into_iter().collect(),
                        ts: 1234,
                        node_id: Some("node1".to_string()),
                        tombstone: None,
                    })
                    .unwrap(),
                )
                .unwrap();
        }
    }

    #[test]
    fn collect_filters_by_org_and_sync_marker() {
        let mut storage = MemoryStorage::new();
        put_doc(&mut storage, "plugin:chat", "messages", "m1", &json!({"orgId": "org_x", "text": "hi"}), true);
        put_doc(&mut storage, "plugin:chat", "messages", "m2", &json!({"orgId": "org_other"}), true);
        put_doc(&mut storage, "plugin:chat", "messages", "m3", &json!({"orgId": "org_x", "__sync": false}), true);
        put_doc(&mut storage, "plugin:chat", "messages", "m4", &json!({"orgId": "org_x"}), false); // 无 meta
        storage.put("doc:plugin:chat:messages:m5", "{broken").unwrap(); // 损坏
        put_doc(&mut storage, "plugin:chat", "messages", "m6", &json!({"orgId": " org_x "}), true); // trim 命中

        let items = collect_syncable_plugin_docs(&storage, "org_x").unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["m1", "m6"]);
        assert_eq!(items[0].meta.ts, 1234);
        assert_eq!(items[0].meta.node_id.as_deref(), Some("node1"));

        // 空 orgId → 空集
        assert!(collect_syncable_plugin_docs(&storage, "  ").unwrap().is_empty());
    }

    #[test]
    fn collect_carries_schema_when_declared() {
        let mut storage = MemoryStorage::new();
        crate::schema::declare_collection_schema(
            &mut storage,
            "plugin:chat",
            "messages",
            &CollectionSchemaDeclaration::lww(),
            1000,
        )
        .unwrap();
        put_doc(&mut storage, "plugin:chat", "messages", "m1", &json!({"orgId": "org_x"}), true);
        let items = collect_syncable_plugin_docs(&storage, "org_x").unwrap();
        assert_eq!(items.len(), 1);
        let schema = items[0].schema.as_ref().unwrap();
        assert_eq!(
            schema.sync_strategy,
            Some(crate::schema::SyncStrategy::Lww)
        );
    }
}
