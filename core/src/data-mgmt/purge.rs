//! 手动清理执行器 L2 级（purge.ts；core/spec/data-mgmt.md §4）。
//!
//! 语义：删除指定插件域（可选限定集合）中 `meta.ts < beforeTs` 的全部本地副本
//! （doc + idx + meta，含同时代 tombstone），并把每个受影响集合的 purge 水位线
//! 抬升到 `beforeTs`——后续远端重推同时代数据会被 `apply_remote_update` 拒绝，
//! 本地清理不会因 K 副本同步而回灌。
//!
//! 硬性边界（防御性强制，purge.ts:16-22 注释）：
//! - 只接受 `plugin:` 域——系统域（策略/水位线/审计日志）、存证链、组织与 p2p
//!   状态永远不在本路径清理范围；
//! - 无 meta 的文档无法判定年代，保守跳过不删；
//! - 存证链是全局单链，删除中间环节会破坏整链验证，本路径从不触碰。
//!
//! 选中→batch 非原子的竞态（purge.ts:13-16 注释，可自愈）：期间若选中 id 恰好
//! 收到 `ts >= beforeTs` 的远端新写入，该新值会被一并删除；水位线不拦截它，
//! 靠后续反熵从其他副本补回。

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::storage::{BatchOperation, ScanOptions, StorageBackend};

use super::watermark::raise_purge_watermark;
use super::{DataMgmtError, Result};

/// 审计日志 key 前缀（purge.ts:40）。
const PURGE_LOG_PREFIX: &str = "doc:system:purge-log:";

/// 手动清理入参（purge.ts:24-29）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurgeOptions {
    /// 目标域（必须 `plugin:` 开头且长度 > 7）。
    pub domain: String,
    /// 清理该时间戳（严格小于）之前的本地副本。
    pub before_ts: i64,
    /// 可选：只清理该集合；缺省清理域内全部集合
    /// （坑 #8：仅模块层能力，service/IPC 层恒为 `None`）。
    pub collection: Option<String>,
}

/// 手动清理结果（purge.ts:31-38）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PurgeResult {
    /// 目标域。
    pub domain: String,
    /// 清理阈值时间戳。
    #[serde(rename = "beforeTs")]
    pub before_ts: i64,
    /// 受影响集合（按选中条目的首次出现顺序）。
    pub collections: Vec<String>,
    /// 删除文档数（以 meta 条数计，每文档恰好一条）。
    #[serde(rename = "removedDocs")]
    pub removed_docs: u64,
    /// 释放字节数（doc/meta/idx 三类 key+value 合计）。
    #[serde(rename = "freedBytes")]
    pub freed_bytes: u64,
    /// 清理执行时间（ms）。
    #[serde(rename = "purgedAt")]
    pub purged_at: i64,
}

/// 预览结果（purge.ts:151-160）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PurgePreview {
    /// 受影响集合。
    pub collections: Vec<String>,
    /// 受影响文档数（以 meta 条数计）。
    #[serde(rename = "affectedDocs")]
    pub affected_docs: u64,
    /// 受影响字节数（doc/meta/idx 三类合计）。
    #[serde(rename = "affectedBytes")]
    pub affected_bytes: u64,
}

/// 审计日志记录（purge.ts:184-196；JSON 字段序对齐 TS 对象字面量）。
#[derive(Serialize)]
struct PurgeAuditLog<'a> {
    domain: &'a str,
    collection: Option<&'a str>,
    #[serde(rename = "beforeTs")]
    before_ts: i64,
    collections: &'a [String],
    #[serde(rename = "removedDocs")]
    removed_docs: u64,
    #[serde(rename = "freedBytes")]
    freed_bytes: u64,
    #[serde(rename = "purgedAt")]
    purged_at: i64,
}

/// 选中的待清理 meta 条目。
#[derive(Clone, Debug)]
struct SelectedMeta {
    collection: String,
    id: String,
    key: String,
    /// meta 行自身 key+value 的 UTF-8 字节数。
    bytes: u64,
}

/// `selectExpiredMetas`（purge.ts:50-92）：选中 `ts < beforeTs` 的 meta 条目，
/// 并校验目标域合法性（校验顺序固定：先 domain 后 beforeTs）。
fn select_expired_metas<S: StorageBackend + ?Sized>(
    storage: &S,
    options: &PurgeOptions,
) -> Result<Vec<SelectedMeta>> {
    if !options.domain.starts_with("plugin:") || options.domain.len() <= "plugin:".len() {
        return Err(DataMgmtError::NonPluginDomain(options.domain.clone()));
    }
    if options.before_ts <= 0 {
        return Err(DataMgmtError::InvalidBeforeTs);
    }

    let meta_prefix = match &options.collection {
        Some(collection) => format!("meta:{}:{collection}:", options.domain),
        None => format!("meta:{}:", options.domain),
    };
    let rows = storage.scan(&ScanOptions::prefix(&meta_prefix))?;

    // 注意：即使指定了 collection，键解析仍只去掉 `meta:{domain}:` 后按第一个 `:` 分
    // （purge.ts:65——TS 在两种扫描前缀下都用同一段解析逻辑）
    let strip_prefix = format!("meta:{}:", options.domain);
    let mut selected = Vec::new();
    for (key, value) in rows {
        // 键剩余部分为 {collection}:{id}；collection 名不含冒号（schema 约束），
        // id 取第一个冒号之后的全部内容，对含冒号的 id 同样精确
        let Some(remainder) = key.strip_prefix(&strip_prefix) else {
            continue;
        };
        let Some(separator) = remainder.find(':') else {
            continue;
        };
        if separator == 0 || separator == remainder.len() - 1 {
            continue;
        }
        let collection = &remainder[..separator];
        let id = &remainder[separator + 1..];

        // JSON.parse 失败或 ts 非 number → 跳过（保守不删）；
        // 选中条件 ts < beforeTs（严格 <），ts >= beforeTs 跳过
        let ts = serde_json::from_str::<Value>(&value)
            .ok()
            .and_then(|parsed| parsed.get("ts").and_then(Value::as_f64));
        let Some(ts) = ts else { continue };
        if ts >= options.before_ts as f64 {
            continue;
        }

        selected.push(SelectedMeta {
            collection: collection.to_string(),
            id: id.to_string(),
            bytes: (key.len() + value.len()) as u64,
            key,
        });
    }
    Ok(selected)
}

/// `buildPurgePlan`（purge.ts:95-148）：汇总选中条目的 doc/idx 体量与删除操作（不执行）。
///
/// 按 collection 分组（保持首次出现顺序，对齐 TS `Map` 迭代序），逐集合累加
/// 删除 op 与 freedBytes（均为 key+value 的 UTF-8 字节数）。
fn build_purge_plan<S: StorageBackend + ?Sized>(
    storage: &S,
    domain: &str,
    selected: &[SelectedMeta],
) -> Result<(Vec<BatchOperation>, u64)> {
    let mut ops = Vec::new();
    let mut freed_bytes = 0u64;

    let mut order: Vec<&str> = Vec::new();
    let mut by_collection: BTreeMap<&str, Vec<&SelectedMeta>> = BTreeMap::new();
    for item in selected {
        if !by_collection.contains_key(item.collection.as_str()) {
            order.push(item.collection.as_str());
        }
        by_collection
            .entry(item.collection.as_str())
            .or_default()
            .push(item);
    }

    for collection in order {
        let items = &by_collection[collection];

        // doc：存在才删，计入释放体量（无 meta 的 doc 不在选中集内，不会误删）
        let doc_prefix = format!("doc:{domain}:{collection}:");
        let selected_ids: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
        for (key, value) in storage.scan(&ScanOptions::prefix(&doc_prefix))? {
            let id = &key[doc_prefix.len()..];
            if !selected_ids.contains(id) {
                continue;
            }
            freed_bytes += (key.len() + value.len()) as u64;
            ops.push(BatchOperation::delete(key));
        }

        // meta：选中集全删（含同时代 tombstone，水位线会拦截同时代重推）
        for item in items {
            ops.push(BatchOperation::delete(item.key.clone()));
            freed_bytes += item.bytes;
        }

        // idx：键为 idx:{domain}:{collection}:{indexName}:{encValue}:{id}，
        // 只能按尾部 ":{id}" 匹配——坑 #10 如实复刻（purge.ts:130-133 注释）：
        // 若系统未来允许 id 内含冒号，"a:b" 的索引会被 "b" 的清理误匹配；
        // 当前各环节产生的 id 均不含冒号
        let idx_prefix = format!("idx:{domain}:{collection}:");
        for (key, value) in storage.scan(&ScanOptions::prefix(&idx_prefix))? {
            for item in items {
                if key.ends_with(&format!(":{}", item.id)) {
                    freed_bytes += (key.len() + value.len()) as u64;
                    ops.push(BatchOperation::delete(key));
                    break;
                }
            }
        }
    }

    Ok((ops, freed_bytes))
}

/// 选中条目的集合去重（首次出现顺序，对齐 TS `[...new Set(...)]`）。
fn unique_collections(selected: &[SelectedMeta]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut collections = Vec::new();
    for item in selected {
        if seen.insert(item.collection.as_str()) {
            collections.push(item.collection.clone());
        }
    }
    collections
}

/// `previewPurgeDomainDocs`（purge.ts:151-160）：预览清理影响面，不删除任何数据。
pub fn preview_purge_domain_docs<S: StorageBackend + ?Sized>(
    storage: &S,
    options: &PurgeOptions,
) -> Result<PurgePreview> {
    let selected = select_expired_metas(storage, options)?;
    let (_ops, freed_bytes) = build_purge_plan(storage, &options.domain, &selected)?;
    // affectedDocs 以 meta（每文档恰好一条）计数；affectedBytes 含 doc/meta/idx 三类
    Ok(PurgePreview {
        collections: unique_collections(&selected),
        affected_docs: selected.len() as u64,
        affected_bytes: freed_bytes,
    })
}

/// `purgeDomainDocs`（purge.ts:166-207）：执行手动清理。
///
/// 流程：删除选中时代的 doc/idx/meta → 抬升各集合 purge 水位线 → 追加审计日志。
/// 坑 #4 如实复刻：选中为空时直接返回，**不抬水位线、不写审计日志**。
pub fn purge_domain_docs<S: StorageBackend>(
    storage: &mut S,
    options: &PurgeOptions,
    now_ms: i64,
) -> Result<PurgeResult> {
    let selected = select_expired_metas(storage, options)?;
    let purged_at = now_ms;
    let collections = unique_collections(&selected);

    if selected.is_empty() {
        return Ok(PurgeResult {
            domain: options.domain.clone(),
            before_ts: options.before_ts,
            collections,
            removed_docs: 0,
            freed_bytes: 0,
            purged_at,
        });
    }

    let (ops, freed_bytes) = build_purge_plan(storage, &options.domain, &selected)?;
    storage.batch(ops)?;

    // 水位线先于返回抬升：此后同时代远端重推一律被拒绝，清理不会被同步回灌
    for collection in &collections {
        let removed_in_collection = selected
            .iter()
            .filter(|item| &item.collection == collection)
            .count() as u64;
        raise_purge_watermark(
            storage,
            &options.domain,
            collection,
            options.before_ts,
            removed_in_collection,
            now_ms,
        )?;
    }

    // 坑 #5 如实复刻：审计日志 key 以毫秒时间戳结尾，同毫秒内两次 purge 会后写覆盖先写
    let log = PurgeAuditLog {
        domain: &options.domain,
        collection: options.collection.as_deref(),
        before_ts: options.before_ts,
        collections: &collections,
        removed_docs: selected.len() as u64,
        freed_bytes,
        purged_at,
    };
    storage.put(
        &format!("{PURGE_LOG_PREFIX}{purged_at}"),
        &serde_json::to_string(&log)?,
    )?;

    Ok(PurgeResult {
        domain: options.domain.clone(),
        before_ts: options.before_ts,
        collections,
        removed_docs: selected.len() as u64,
        freed_bytes,
        purged_at,
    })
}

#[cfg(test)]
mod tests;
