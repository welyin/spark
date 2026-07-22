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
        let Some(remainder) = key.strip_prefix(&strip_prefix) else { continue };
        let Some(separator) = remainder.find(':') else { continue };
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
        by_collection.entry(item.collection.as_str()).or_default().push(item);
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
        let removed_in_collection =
            selected.iter().filter(|item| &item.collection == collection).count() as u64;
        raise_purge_watermark(storage, &options.domain, collection, options.before_ts, removed_in_collection, now_ms)?;
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
    storage.put(&format!("{PURGE_LOG_PREFIX}{purged_at}"), &serde_json::to_string(&log)?)?;

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
mod tests {
    use super::*;
    use crate::data_mgmt::watermark::{get_purge_watermark, is_purged_by_watermark};
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_800_000_000_000;

    fn opts(domain: &str, before_ts: i64) -> PurgeOptions {
        PurgeOptions { domain: domain.to_string(), before_ts, collection: None }
    }

    fn meta_json(ts: i64) -> String {
        format!("{{\"vv\":{{\"n1\":1}},\"ts\":{ts}}}")
    }

    /// 标准 fixture：col1 下 id1（ts=100，旧）与 id2（ts=300，新），含 doc/meta/idx 三件套。
    fn standard_fixture() -> MemoryStorage {
        let mut s = MemoryStorage::new();
        s.put("doc:plugin:app:col1:id1", "{\"v\":1}").unwrap();
        s.put("meta:plugin:app:col1:id1", &meta_json(100)).unwrap();
        s.put("idx:plugin:app:col1:byX:enc1:id1", "").unwrap();
        s.put("doc:plugin:app:col1:id2", "{\"v\":2}").unwrap();
        s.put("meta:plugin:app:col1:id2", &meta_json(300)).unwrap();
        s.put("idx:plugin:app:col1:byX:enc2:id2", "").unwrap();
        s
    }

    #[test]
    fn select_validates_domain_before_before_ts() {
        let s = MemoryStorage::new();
        // 非 plugin 域
        let err = purge_domain_docs(&mut s.clone(), &opts("chat", 100), NOW).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Refused to purge non-plugin domain \"chat\": only plugin domains can be purged"
        );
        // 恰好 "plugin:"（长度 7）也拒绝
        let err = purge_domain_docs(&mut s.clone(), &opts("plugin:", 100), NOW).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Refused to purge non-plugin domain \"plugin:\": only plugin domains can be purged"
        );
        // domain 校验优先于 beforeTs 校验
        let err = purge_domain_docs(&mut s.clone(), &opts("chat", 0), NOW).unwrap_err();
        assert!(matches!(err, DataMgmtError::NonPluginDomain(_)));
        // beforeTs <= 0
        let err = purge_domain_docs(&mut s.clone(), &opts("plugin:app", 0), NOW).unwrap_err();
        assert_eq!(err.to_string(), "beforeTs must be a positive timestamp");
        let err = purge_domain_docs(&mut s.clone(), &opts("plugin:app", -1), NOW).unwrap_err();
        assert!(matches!(err, DataMgmtError::InvalidBeforeTs));
    }

    #[test]
    fn select_ts_strict_less_than_and_parse_failures() {
        let mut s = MemoryStorage::new();
        s.put("meta:plugin:app:c:old", &meta_json(199)).unwrap(); // 选中
        s.put("meta:plugin:app:c:equal", &meta_json(200)).unwrap(); // ts == beforeTs 不选（严格 <）
        s.put("meta:plugin:app:c:new", &meta_json(201)).unwrap(); // 不选
        s.put("meta:plugin:app:c:broken", "not json").unwrap(); // 损坏 → 保守跳过
        s.put("meta:plugin:app:c:strts", "{\"ts\":\"100\"}").unwrap(); // ts 非 number → 跳过
        s.put("meta:plugin:app:c:nots", "{\"vv\":{}}").unwrap(); // ts 缺失 → 跳过
        // tombstone meta 同样按 ts 判定（同时代 tombstone 一并清理）
        s.put("meta:plugin:app:c:tomb", "{\"ts\":50,\"tombstone\":true}").unwrap();

        let selected = select_expired_metas(&s, &opts("plugin:app", 200)).unwrap();
        let ids: Vec<&str> = selected.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["old", "tomb"]); // 扫描按键升序
    }

    #[test]
    fn select_key_parsing_edge_cases() {
        let mut s = MemoryStorage::new();
        // 空 collection（separator == 0）→ 跳过
        s.put("meta:plugin:app::id", &meta_json(1)).unwrap();
        // 空 id（separator 在末尾）→ 跳过
        s.put("meta:plugin:app:col:", &meta_json(1)).unwrap();
        // 无冒号 → 跳过
        s.put("meta:plugin:app:colonly", &meta_json(1)).unwrap();
        // id 含冒号：第一个冒号后全部内容为 id（精确）
        s.put("meta:plugin:app:col:a:b", &meta_json(1)).unwrap();

        let selected = select_expired_metas(&s, &opts("plugin:app", 100)).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].collection, "col");
        assert_eq!(selected[0].id, "a:b");
    }

    #[test]
    fn select_with_collection_option_scans_only_that_collection() {
        let mut s = MemoryStorage::new();
        s.put("meta:plugin:app:c1:a", &meta_json(1)).unwrap();
        s.put("meta:plugin:app:c2:b", &meta_json(1)).unwrap();
        let options = PurgeOptions {
            domain: "plugin:app".to_string(),
            before_ts: 100,
            collection: Some("c1".to_string()),
        };
        let selected = select_expired_metas(&s, &options).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].collection, "c1");
    }

    #[test]
    fn purge_full_flow_deletes_three_key_kinds_and_writes_watermark_and_audit() {
        let mut s = standard_fixture();
        let result = purge_domain_docs(&mut s, &opts("plugin:app", 200), NOW).unwrap();

        assert_eq!(result.domain, "plugin:app");
        assert_eq!(result.before_ts, 200);
        assert_eq!(result.collections, ["col1"]);
        assert_eq!(result.removed_docs, 1);
        assert_eq!(result.purged_at, NOW);
        // freedBytes = doc/meta/idx 三件套 key+value 合计
        let expected_freed = ("doc:plugin:app:col1:id1".len() + "{\"v\":1}".len()
            + "meta:plugin:app:col1:id1".len()
            + meta_json(100).len()
            + "idx:plugin:app:col1:byX:enc1:id1".len()) as u64;
        assert_eq!(result.freed_bytes, expected_freed);

        // id1 三件套全删；id2（ts=300 >= 200）完整保留
        assert!(s.get("doc:plugin:app:col1:id1").unwrap().is_none());
        assert!(s.get("meta:plugin:app:col1:id1").unwrap().is_none());
        assert!(s.get("idx:plugin:app:col1:byX:enc1:id1").unwrap().is_none());
        assert!(s.get("doc:plugin:app:col1:id2").unwrap().is_some());
        assert!(s.get("meta:plugin:app:col1:id2").unwrap().is_some());
        assert!(s.get("idx:plugin:app:col1:byX:enc2:id2").unwrap().is_some());

        // 水位线抬升到 beforeTs，removedDocs=1
        let w = get_purge_watermark(&s, "plugin:app", "col1").unwrap().unwrap();
        assert_eq!((w.purged_before, w.removed_docs, w.purged_at), (200, 1, NOW));
        // 同时代远端重推被拦截，边界 ts == 200 不拦
        assert!(is_purged_by_watermark(&s, "plugin:app", "col1", 199).unwrap());
        assert!(!is_purged_by_watermark(&s, "plugin:app", "col1", 200).unwrap());

        // 审计日志：key = doc:system:purge-log:{purgedAt}，value 逐字节对齐 TS
        let log = s.get(&format!("doc:system:purge-log:{NOW}")).unwrap().unwrap();
        let expected_log = format!(
            "{{\"domain\":\"plugin:app\",\"collection\":null,\"beforeTs\":200,\"collections\":[\"col1\"],\"removedDocs\":1,\"freedBytes\":{expected_freed},\"purgedAt\":{NOW}}}"
        );
        assert_eq!(log, expected_log);
    }

    #[test]
    fn purge_multi_collection_watermark_per_collection() {
        let mut s = MemoryStorage::new();
        // col1：两条旧；col2：一条旧
        s.put("meta:plugin:app:col1:a", &meta_json(10)).unwrap();
        s.put("doc:plugin:app:col1:a", "1").unwrap();
        s.put("meta:plugin:app:col1:b", &meta_json(20)).unwrap();
        s.put("meta:plugin:app:col2:c", &meta_json(30)).unwrap();
        s.put("idx:plugin:app:col2:byY:v:c", "").unwrap();

        let result = purge_domain_docs(&mut s, &opts("plugin:app", 100), NOW).unwrap();
        assert_eq!(result.collections, ["col1", "col2"]); // 首次出现顺序（键升序）
        assert_eq!(result.removed_docs, 3);

        let w1 = get_purge_watermark(&s, "plugin:app", "col1").unwrap().unwrap();
        let w2 = get_purge_watermark(&s, "plugin:app", "col2").unwrap().unwrap();
        assert_eq!((w1.purged_before, w1.removed_docs), (100, 2));
        assert_eq!((w2.purged_before, w2.removed_docs), (100, 1));
    }

    #[test]
    fn empty_purge_leaves_no_trace() {
        let mut s = standard_fixture();
        let before = s.len();
        // beforeTs=50：无 meta 早于 50
        let result = purge_domain_docs(&mut s, &opts("plugin:app", 50), NOW).unwrap();
        assert_eq!(result.removed_docs, 0);
        assert_eq!(result.freed_bytes, 0);
        assert!(result.collections.is_empty());
        assert_eq!(result.purged_at, NOW);
        // 坑 #4：不抬水位线、不写审计日志、库内容零变化
        assert_eq!(s.len(), before);
        assert!(get_purge_watermark(&s, "plugin:app", "col1").unwrap().is_none());
        assert!(s.get(&format!("doc:system:purge-log:{NOW}")).unwrap().is_none());
    }

    #[test]
    fn preview_matches_execute_without_writes() {
        let mut s = standard_fixture();
        let preview = preview_purge_domain_docs(&s, &opts("plugin:app", 200)).unwrap();
        assert_eq!(preview.collections, ["col1"]);
        assert_eq!(preview.affected_docs, 1);
        let before = s.len();

        let result = purge_domain_docs(&mut s, &opts("plugin:app", 200), NOW).unwrap();
        assert_eq!(preview.affected_bytes, result.freed_bytes);
        assert_eq!(preview.affected_docs, result.removed_docs);
        // preview 本身不写（上方 execute 前的 len 与 fixture 相同）
        assert_eq!(before, 6);
    }

    #[test]
    fn idx_suffix_match_pit10_replicated() {
        // 坑 #10 如实复刻并固定行为：id "a:b" 的索引行尾部以 ":b" 结尾，
        // 清理 id "b" 时被误匹配删除；doc 按精确 id 匹配不受影响。
        let mut s = MemoryStorage::new();
        s.put("meta:plugin:app:c:b", &meta_json(10)).unwrap(); // 选中 id "b"
        s.put("doc:plugin:app:c:b", "v").unwrap();
        s.put("meta:plugin:app:c:a:b", &meta_json(9999)).unwrap(); // id "a:b" 不在选中集（ts 新）
        s.put("doc:plugin:app:c:a:b", "v2").unwrap();
        s.put("idx:plugin:app:c:byX:v:a:b", "").unwrap(); // id "a:b" 的索引行

        let result = purge_domain_docs(&mut s, &opts("plugin:app", 100), NOW).unwrap();
        assert_eq!(result.removed_docs, 1);
        // id "b" 的 doc/meta 删除
        assert!(s.get("doc:plugin:app:c:b").unwrap().is_none());
        assert!(s.get("meta:plugin:app:c:b").unwrap().is_none());
        // 缺陷：id "a:b" 的索引行被 ":b" 尾部匹配误删
        assert!(s.get("idx:plugin:app:c:byX:v:a:b").unwrap().is_none());
        // 但 id "a:b" 的 doc/meta 按精确匹配保留
        assert!(s.get("doc:plugin:app:c:a:b").unwrap().is_some());
        assert!(s.get("meta:plugin:app:c:a:b").unwrap().is_some());
    }

    #[test]
    fn purge_with_collection_option_writes_collection_in_audit() {
        let mut s = MemoryStorage::new();
        s.put("meta:plugin:app:c1:a", &meta_json(10)).unwrap();
        s.put("meta:plugin:app:c2:b", &meta_json(10)).unwrap();
        let options = PurgeOptions {
            domain: "plugin:app".to_string(),
            before_ts: 100,
            collection: Some("c1".to_string()),
        };
        let result = purge_domain_docs(&mut s, &options, NOW).unwrap();
        assert_eq!(result.collections, ["c1"]);
        assert!(s.get("meta:plugin:app:c2:b").unwrap().is_some());
        let log = s.get(&format!("doc:system:purge-log:{NOW}")).unwrap().unwrap();
        assert!(log.contains("\"collection\":\"c1\""));
        assert!(get_purge_watermark(&s, "plugin:app", "c2").unwrap().is_none());
    }
}
