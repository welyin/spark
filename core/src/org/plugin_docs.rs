//! `doc:plugin:` 键域工具（对齐 desktop/src/main/p2p/plugin-org-sync.ts 的
//! 键形/禁用判定口径）。
//!
//! 现役职责：键形解析（[`parse_plugin_doc_key`]）、同步禁用判定
//! （[`is_sync_disabled`]）、orgId 解析（[`resolve_org_id`]）、旧通道向
//! orgsync 声明通道的迁移（[`migrate_plugin_docs`]`doc:plugin:` → org scope
//! 集合声明 + `orgd:` 数据键）、purge 数据域定位（[`collect_org_plugin_domains`]）。
//!
//! （legacy org-share/org-pull 平面的 pluginDocs 收集/应用挂载点——org-share
//! `payload.pluginDocs` 与 org-pull-org 响应——已随该平面退役删除；插件文档
//! 经迁移后的 org 声明集合走 orgsync 反熵。）

use serde_json::Value;

use crate::plugindata::{Accounts, DeclareInput, Scope, Space, declare, org_data_prefix};
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::versioned::VersionedStorage;

use super::Result;

/// 插件文档键前缀（plugin-org-sync.ts:16）。
pub const PLUGIN_DOC_PREFIX: &str = "doc:plugin:";

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

/// `doc:plugin:` 旧通道 → declareCollection 声明 + `orgd:` 键域（O2 工作项 5）。
///
/// 按 plugin-data-api §7 映射：
/// - 每个 `doc:plugin:{plugin}:{collection}:{id}`（`payload.orgId == org_id`
///   且未标记同步禁用）声明一个 **org scope** 集合 `{plugin}:{collection}@v1`
///   （space=org；`accounts` 由调用方指定——缺省 data-accounts+filtered，需全员
///   驻留时传 `Accounts::AllMembers`；其余全缺省：devices=all / merge=lww-record）；
///   并把 payload 迁入 `orgd:{orgId}:{name}@v1:{id}` 数据键域（orgsync 受管，
///   经 VersionedStorage 写入即自动版本化/删除日志）。
/// - [`is_sync_disabled`]（`__sync === false` / mode ∈ {local, none, disabled}
///   → scope:local）的文档**不迁移**（保持本地）。
/// - **幂等**：集合已声明（策略兼容）与 `orgd:` 键已存在均跳过；重复调用无副作用。
///
/// 返回本次新迁入的 `orgd:` 记录条数。
pub fn migrate_plugin_docs<S: StorageBackend>(
    storage: &mut VersionedStorage<S>,
    org_id: &str,
    accounts: Accounts,
    declared_by: &str,
    now_ms: i64,
) -> Result<usize> {
    let target_org_id = org_id.trim();
    if target_org_id.is_empty() {
        return Ok(0);
    }
    let rows = storage.scan(&ScanOptions::prefix(PLUGIN_DOC_PREFIX))?;
    let mut migrated = 0usize;
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
        // scope:local（同步禁用）不迁移——保持旧通道本地文档语义。
        if is_sync_disabled(&payload) {
            continue;
        }
        // 插件 id = `plugin:{tail}` 的 tail；集合名 = `{plugin}:{collection}`。
        let Some(plugin_id) = domain.strip_prefix("plugin:") else {
            continue;
        };
        if plugin_id.is_empty() || collection.is_empty() {
            continue;
        }
        let name = format!("{plugin_id}:{collection}");
        let version = "1";
        // 声明 org scope 集合（幂等：策略兼容返回既有）。F7：遇非法名/策略
        // 冲突逐条跳过 + warn，一颗耗子屎不堵后续迁移（不再整体中断）。
        if let Err(e) = declare(
            storage,
            plugin_id,
            DeclareInput {
                name: name.clone(),
                version: Some(version.to_string()),
                scope: Some(Scope::Sync),
                space: Some(Space::Org),
                accounts: Some(accounts),
                devices: None,
                confidentiality: None,
                sensitivity: None,
                merge: None,
                declared_by: Some(declared_by.to_string()),
                read_policy: None,
            },
            now_ms,
            Some(target_org_id),
        ) {
            log::warn!(
                "[plugin-docs] migrate skip doc {} (declare failed): {e}",
                key
            );
            continue;
        }
        // 迁入 orgd: 数据键（幂等：已存在跳过）。值 = payload 原样 JSON。
        let data_key = format!("{}{id}", org_data_prefix(target_org_id, &name, version));
        if storage.get(&data_key)?.is_some() {
            continue;
        }
        storage.put(&data_key, &value)?;
        migrated += 1;
    }
    Ok(migrated)
}

/// 扫描定位组织的插件域：收集 `doc:plugin:` 键中 `payload.orgId == org_id`
/// 出现过的全部插件域（扫描键升序去重，结果确定性）。
///
/// 用途：purge 的数据域定位——组织记录已无 `basePluginDomain` 字段，数据域
/// 只能从存储的插件文档反推。调用方按数量分派：0 个 → 无数据域（返回空串，
/// preview affectedDocs=0 自然拦截 execute）；1 个 → 用之；多个 → 取第一个，
/// 保持单 domain 语义（purge 按单域执行）。
pub fn collect_org_plugin_domains<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Result<Vec<String>> {
    let target_org_id = org_id.trim();
    if target_org_id.is_empty() {
        return Ok(Vec::new());
    }

    let rows = storage.scan(&ScanOptions::prefix(PLUGIN_DOC_PREFIX))?;
    let mut domains: Vec<String> = Vec::new();
    for (key, value) in rows {
        let Some((domain, _, _)) = parse_plugin_doc_key(&key) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&value) else {
            continue;
        };
        if resolve_org_id(&payload) != target_org_id {
            continue;
        }
        if !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    Ok(domains)
}
