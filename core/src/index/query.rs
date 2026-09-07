//! indexer 查询信封（wiki/protocol/community/affair-metadata.md §8）：
//! 轻客户端向启用角色的节点发 `affair-meta-query`，节点回
//! `affair-meta-result`——走 `/spark/affairmeta/1.0.0` request-response
//! （p2p 层），帧级校验在这里；业务分发（确定性匹配 + 健康信号内嵌）
//! 为纯函数，本地直查（kernel 门面）与 p2p 应答共用同一路径。
//!
//! 收录编排（§4 验证分层 + §5 暂存区裁决）的入口 `ingest_announcement`
//! 也在本模块：gossip 入站（host 回调）与内核自发布共用。

use serde_json::{Map, Value};

use crate::storage::StorageBackend;

use super::IndexError;
use super::announce::{MetaAnnounce, parse_announce};
use super::health::{HealthSignals, try_health, verify_against_log};
use super::local_index::{entry_from_announce, list_entries, upsert_entry};
use super::log::load_log;
use super::match_::{MATCH_LIMIT_MAX, MatchHit, SearchQuery, search};
use super::staging::{StageOutcome, stage_announcement};

/// 请求帧类型（§8）。
pub const QUERY_TYPE: &str = "affair-meta-query";
/// 响应帧类型（§8）。
pub const RESULT_TYPE: &str = "affair-meta-result";
/// queryId 长度上限（不透明字符串，1–64）。
const QUERY_ID_MAX: usize = 64;
/// 匹配文本长度上限（UTF-8 字节）。
const QUERY_TEXT_MAX: usize = 256;
/// 查询帧体积上限（§8）。
pub const QUERY_MAX_BYTES: usize = 4 * 1024;
/// 缺省结果条数（§8）。
pub const QUERY_LIMIT_DEFAULT: usize = 20;

/// 解析后的查询（当前仅 search 一种 kind；kind 字段为扩展位）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffairMetaQuery {
    pub query_id: String,
    pub search: SearchQuery,
}

/// 构造请求帧文本（固定键序 type → queryId → payload）。
pub fn build_query(query_id: &str, search: &SearchQuery) -> String {
    let mut payload = Map::new();
    payload.insert("kind".to_string(), Value::String("search".to_string()));
    payload.insert("query".to_string(), Value::String(search.text.clone()));
    payload.insert(
        "limit".to_string(),
        Value::Number(search.limit.min(u32::MAX as usize).into()),
    );
    if let Some(region) = &search.region {
        payload.insert("region".to_string(), Value::String(region.clone()));
    }
    if !search.tags.is_empty() {
        payload.insert(
            "tags".to_string(),
            serde_json::to_value(&search.tags).expect("tags serialize"),
        );
    }
    // 可选过滤/容错参数（仅启用时携带，旧端忽略未知键）
    if search.fuzzy {
        payload.insert("fuzzy".to_string(), Value::Bool(true));
    }
    if let Some(after) = search.updated_after {
        payload.insert("updatedAfter".to_string(), Value::Number(after.into()));
    }
    if let Some(before) = search.updated_before {
        payload.insert("updatedBefore".to_string(), Value::Number(before.into()));
    }
    if search.verified_only {
        payload.insert("verifiedOnly".to_string(), Value::Bool(true));
    }
    if let Some(max) = search.max_objections {
        payload.insert("maxObjections".to_string(), Value::Number(max.into()));
    }
    if let Some(since) = search.active_since_ms {
        payload.insert("activeSinceMs".to_string(), Value::Number(since.into()));
    }
    if let Some(min) = search.min_recent_active {
        payload.insert("minRecentActive".to_string(), Value::Number(min.into()));
    }
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(QUERY_TYPE.to_string()));
    map.insert("queryId".to_string(), Value::String(query_id.to_string()));
    map.insert("payload".to_string(), Value::Object(payload));
    serde_json::to_string(&Value::Object(map)).expect("query frame serializable")
}

/// 解析请求帧：帧级校验（type/queryId/体积），payload 的 search 语义校验
/// 随后由 [`parse_search_payload`] 完成。任一不符返回 None（静默丢弃口径
/// 由调用方决定；p2p 应答侧回 bad-query 错误帧）。
pub fn parse_query(text: &str) -> Option<AffairMetaQuery> {
    if text.len() > QUERY_MAX_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(Value::as_str) != Some(QUERY_TYPE) {
        return None;
    }
    let query_id = value.get("queryId").and_then(Value::as_str)?;
    if query_id.is_empty() || query_id.len() > QUERY_ID_MAX {
        return None;
    }
    let search = parse_search(value.get("payload")?)?;
    Some(AffairMetaQuery {
        query_id: query_id.to_string(),
        search,
    })
}

/// search payload 语义校验：query ≤256B；limit 缺省/非正 → 缺省值（match
/// 层再截断到上限）；tags ≤16 个、每个 ≤32 UTF-16；可选过滤/容错参数
/// （fuzzy/updatedAfter/updatedBefore/verifiedOnly/maxObjections/
/// activeSinceMs/minRecentActive）缺失 = 不启用、类型不符整帧拒绝。
/// p2p 应答侧（host 回调拿到的是裸 payload）与请求帧解析共用此入口。
pub fn parse_search(payload: &Value) -> Option<SearchQuery> {
    let obj = payload.as_object()?;
    if obj.get("kind").and_then(Value::as_str) != Some("search") {
        return None;
    }
    let text = obj.get("query").and_then(Value::as_str)?;
    if text.len() > QUERY_TEXT_MAX {
        return None;
    }
    let limit = obj
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= u32::MAX as u64)
        .map(|n| n as usize)
        .unwrap_or(QUERY_LIMIT_DEFAULT);
    let region = obj
        .get("region")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let mut tags = Vec::new();
    if let Some(raw) = obj.get("tags") {
        let arr = raw.as_array()?;
        if arr.len() > 16 {
            return None;
        }
        for tag in arr {
            let tag = tag.as_str()?;
            if tag.encode_utf16().count() > 32 {
                return None;
            }
            tags.push(tag.to_string());
        }
    }
    // 可选过滤/容错参数（缺失 = 不启用；类型不符整帧拒绝，与 tags 同口径）
    let fuzzy = match obj.get("fuzzy") {
        Some(v) => v.as_bool()?,
        None => false,
    };
    let updated_after = match obj.get("updatedAfter") {
        Some(v) => Some(v.as_i64()?),
        None => None,
    };
    let updated_before = match obj.get("updatedBefore") {
        Some(v) => Some(v.as_i64()?),
        None => None,
    };
    let verified_only = match obj.get("verifiedOnly") {
        Some(v) => v.as_bool()?,
        None => false,
    };
    let max_objections = match obj.get("maxObjections") {
        Some(v) => Some(v.as_u64()?),
        None => None,
    };
    let active_since_ms = match obj.get("activeSinceMs") {
        Some(v) => Some(v.as_i64()?),
        None => None,
    };
    let min_recent_active = match obj.get("minRecentActive") {
        Some(v) => Some(v.as_u64()?),
        None => None,
    };
    Some(SearchQuery {
        text: text.to_string(),
        limit,
        region,
        tags,
        fuzzy,
        updated_after,
        updated_before,
        verified_only,
        max_objections,
        active_since_ms,
        min_recent_active,
    })
}

/// 构造响应帧（payload 为结果或错误对象；键序 type → queryId → payload）。
///
/// 体积上限（§7「响应帧体积 ≤ 4 KB」）：带 `results` 数组的 payload 超限时
/// 从结果**尾部**按序截断条数并将 `complete` 置 false，直至帧 ≤ 4 KB——
/// 排序确定（score 降序、affairId 升序），截断同样确定：同输入同输出，
/// 任何节点同样截断。错误对象等小 payload 不触发截断。
pub fn build_result(query_id: &str, payload: &Value) -> String {
    let mut payload = payload.clone();
    while payload
        .get("results")
        .and_then(Value::as_array)
        .is_some_and(|r| !r.is_empty())
        && frame_text(query_id, &payload).len() > QUERY_MAX_BYTES
    {
        if let Some(results) = payload.get_mut("results").and_then(Value::as_array_mut) {
            results.pop();
        }
        payload["complete"] = Value::Bool(false);
    }
    frame_text(query_id, &payload)
}

/// 帧序列化（固定键序 type → queryId → payload）。
fn frame_text(query_id: &str, payload: &Value) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(RESULT_TYPE.to_string()));
    map.insert("queryId".to_string(), Value::String(query_id.to_string()));
    map.insert("payload".to_string(), payload.clone());
    serde_json::to_string(&Value::Object(map)).expect("result frame serializable")
}

/// 构造错误响应帧（payload = {"error": reason}；reason 为稳定字符串）。
pub fn build_error(query_id: &str, reason: &str) -> String {
    build_result(query_id, &serde_json::json!({ "error": reason }))
}

/// 响应帧解析（请求侧）：返回 payload；type/queryId 不符返回 None。
pub fn parse_result(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("type").and_then(Value::as_str) != Some(RESULT_TYPE) {
        return None;
    }
    value.get("payload").cloned()
}

/// 命中条目 → 结果对象（健康信号有本地日志则内嵌，否则缺席）。
fn hit_to_value<S: StorageBackend>(
    storage: &S,
    hit: &MatchHit,
    now_ms: i64,
) -> Result<Value, IndexError> {
    let entry = &hit.entry;
    let health = try_health(storage, &entry.affair_id, now_ms)?;
    let mut obj = Map::new();
    obj.insert(
        "affairId".to_string(),
        Value::String(entry.affair_id.clone()),
    );
    obj.insert("title".to_string(), Value::String(entry.title.clone()));
    obj.insert("summary".to_string(), Value::String(entry.summary.clone()));
    obj.insert(
        "tags".to_string(),
        serde_json::to_value(&entry.tags).expect("tags serialize"),
    );
    if let Some(region) = &entry.region {
        obj.insert("region".to_string(), Value::String(region.clone()));
    }
    obj.insert("metaSeq".to_string(), Value::Number(entry.meta_seq.into()));
    obj.insert(
        "basisOpHash".to_string(),
        Value::String(entry.basis_op_hash.clone()),
    );
    obj.insert("verified".to_string(), Value::Bool(entry.verified));
    obj.insert(
        "updatedAt".to_string(),
        Value::Number(entry.updated_at.into()),
    );
    obj.insert("score".to_string(), Value::Number(hit.score.into()));
    if let Some(health) = health {
        obj.insert(
            "health".to_string(),
            serde_json::to_value(&health).expect("health serialize"),
        );
    }
    Ok(Value::Object(obj))
}

/// 健康信号过滤判定：所有设置的健康过滤须全部通过；健康信号缺席（无本地
/// 日志副本）一律不通过——携带健康过滤的查询只返回可复算的条目（确定性：
/// 同一存储状态下推导结果一致）。
fn health_filter_pass(health: Option<&HealthSignals>, query: &SearchQuery) -> bool {
    if !query.has_health_filter() {
        return true;
    }
    let Some(health) = health else {
        return false;
    };
    if let Some(max) = query.max_objections {
        if health.objection_count > max {
            return false;
        }
    }
    if let Some(since) = query.active_since_ms {
        if health.last_activity_ms.map_or(true, |t| t < since) {
            return false;
        }
    }
    if let Some(min) = query.min_recent_active {
        // 趋势桶 0 = 最近完整活跃窗口（health.rs 口径）
        let recent = health.active_participants_trend.first().copied().unwrap_or(0);
        if recent < min {
            return false;
        }
    }
    true
}

/// 确定性搜索分发（本地索引 → 匹配 → 逐条内嵌健康信号）。同一存储状态 +
/// 同一查询 → 逐字节一致（索引扫描键序 = affairId 序，匹配/健康皆纯函数）。
/// 携带健康信号过滤时：先把匹配上限放宽到 MATCH_LIMIT_MAX 取候选，套用
/// 健康过滤后再按查询 limit 截断——保证过滤不吞掉本应排进前 limit 的条目。
pub fn run_search<S: StorageBackend>(
    storage: &S,
    search_query: &SearchQuery,
    now_ms: i64,
) -> Result<Value, IndexError> {
    let entries = list_entries(storage)?;
    let hits = if search_query.has_health_filter() {
        let mut widened = search_query.clone();
        widened.limit = MATCH_LIMIT_MAX;
        let mut hits = search(&entries, &widened);
        let mut filtered = Vec::with_capacity(hits.len());
        for hit in hits.drain(..) {
            let health = try_health(storage, &hit.entry.affair_id, now_ms)?;
            if health_filter_pass(health.as_ref(), search_query) {
                filtered.push(hit);
            }
        }
        filtered.truncate(search_query.limit.min(MATCH_LIMIT_MAX));
        filtered
    } else {
        search(&entries, search_query)
    };
    let mut results = Vec::with_capacity(hits.len());
    for hit in &hits {
        results.push(hit_to_value(storage, hit, now_ms)?);
    }
    Ok(serde_json::json!({
        "results": Value::Array(results),
        "complete": true,
    }))
}

/// 公告收录结果（gossip 入站 → host 回调的上报语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    /// 新公告入暂存区并建索引条目。
    Inserted,
    /// 按裁决替换暂存区与索引条目。
    Replaced,
    /// 裁决保留既有条目，本条丢弃。
    Kept,
    /// 与本地日志复算矛盾，丢弃（调用方告警，§4）。
    ConflictDropped,
}

/// 公告收录编排（affair-metadata §4 + §5）：完整线形校验 → 修订链复算
/// （有日志副本）→ 暂存区裁决 → 更新本地索引。gossip 入站与内核自发布
/// 共用此入口，保证两路径簿记一致。
pub fn ingest_announcement<S: StorageBackend>(
    storage: &mut S,
    payload: &Value,
    now_ms: i64,
) -> Result<IngestOutcome, IndexError> {
    let announce: MetaAnnounce =
        parse_announce(payload).map_err(|reason| IndexError::BadAnnounce(reason))?;
    let log = load_log(storage, &announce.affair_id)?;
    let verified = match verify_against_log(log.as_ref(), &announce, now_ms) {
        crate::affair::MetaVerify::Verified => true,
        crate::affair::MetaVerify::Conflict => return Ok(IngestOutcome::ConflictDropped),
        crate::affair::MetaVerify::Unverified => false,
    };
    let outcome = stage_announcement(storage, &announce, verified, now_ms)?;
    if outcome == StageOutcome::Kept {
        return Ok(IngestOutcome::Kept);
    }
    upsert_entry(storage, &entry_from_announce(&announce, verified))?;
    Ok(match outcome {
        StageOutcome::Inserted => IngestOutcome::Inserted,
        _ => IngestOutcome::Replaced,
    })
}

/// 从本地日志最新生效代际构造待发布公告（发布侧共用，见 kernel/index_ops）。
pub fn announce_from_local_log(log: &super::log::LoadedLog, now_ms: i64) -> MetaAnnounce {
    let moderator = log.genesis.initiator.identity.clone();
    let objections = super::log::objection_counts(&log.ops);
    let revisions: Vec<crate::affair::MetaReviseEntry> = log
        .ops
        .iter()
        .filter(|op| op.parsed.op_type == crate::affair::OpType::MetaRevise)
        .filter_map(|op| {
            let payload = crate::affair::parse_meta_revise_payload(&op.parsed.payload).ok()?;
            if !matches!(
                payload.mechanism,
                crate::affair::Mechanism::DelayedVeto { .. }
            ) {
                return None;
            }
            Some(crate::affair::MetaReviseEntry {
                op_hash: op.op_hash.clone(),
                actor_identity: op.parsed.actor.identity.clone(),
                payload,
                anchored_ms: op.anchored_ms.unwrap_or(i64::MIN),
                objection_count: objections.get(&op.op_hash).copied().unwrap_or(0),
            })
        })
        .collect();
    let genesis_meta = crate::affair::AffairMeta {
        title: log.genesis.title.clone(),
        summary: log.genesis.summary.clone(),
        tags: log.genesis.tags.clone(),
    };
    let generations = crate::affair::derive_meta_generations(
        &log.affair_id,
        &genesis_meta,
        &moderator,
        &revisions,
        now_ms,
    );
    let latest = generations
        .last()
        .expect("generations 恒非空：创世为第 0 代");
    super::announce::announce_from_generation(&log.affair_id, latest, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// 无日志副本的公告（unverified 收录）→ 索引 + 查询回路。
    #[test]
    fn loopback_query_over_unverified_entry() {
        let mut s = MemoryStorage::new();
        let id = "a".repeat(64);
        let payload = serde_json::json!({
            "metaV": 1,
            "affairId": id,
            "title": "第二届业委会选举",
            "summary": "候选人提名",
            "tags": ["region:110105", "hoa"],
            "metaSeq": 0,
            "basisOpHash": id,
            "updatedAt": 1720000000000i64,
        });
        let outcome = ingest_announcement(&mut s, &payload, 1720000100000).unwrap();
        assert_eq!(outcome, IngestOutcome::Inserted);

        // 同一查询两次分发，结果逐字节一致（确定性）。
        let text = build_query(
            "q1",
            &SearchQuery {
                text: "业委会".to_string(),
                limit: 20,
                region: Some("110105".to_string()),
                ..SearchQuery::plain("", 0)
            },
        );
        let parsed = parse_query(&text).expect("frame parses");
        let a = run_search(&s, &parsed.search, 1720000200000).unwrap();
        let b = run_search(&s, &parsed.search, 1720000200000).unwrap();
        assert_eq!(a, b);
        assert_eq!(a["results"].as_array().unwrap().len(), 1);
        let hit = &a["results"][0];
        assert_eq!(hit["title"], "第二届业委会选举");
        assert!(!hit["verified"].as_bool().unwrap());
        assert!(hit.get("health").is_none(), "无日志副本 → health 缺席");

        // 响应帧回路：build → parse 不丢 payload。
        let response = build_result(&parsed.query_id, &a);
        let round = parse_result(&response).unwrap();
        assert_eq!(round, a);
    }

    /// 可选过滤/容错参数的线形往返：build → parse 逐字段一致；未启用时
    /// 键不携带（旧端忽略未知键的另一面是新端缺省不启用）。
    #[test]
    fn search_filters_roundtrip_over_wire() {
        let query = SearchQuery {
            text: "业委会".to_string(),
            limit: 7,
            fuzzy: true,
            updated_after: Some(100),
            updated_before: Some(200),
            verified_only: true,
            max_objections: Some(3),
            active_since_ms: Some(50),
            min_recent_active: Some(2),
            ..SearchQuery::plain("", 0)
        };
        let text = build_query("qf", &query);
        let parsed = parse_query(&text).expect("frame parses");
        assert_eq!(parsed.search, query);

        // 缺省口径：plain 查询的线形不携带任何过滤键
        let plain = build_query("qp", &SearchQuery::plain("x", 5));
        let value: Value = serde_json::from_str(&plain).unwrap();
        let payload = value["payload"].as_object().unwrap();
        for key in [
            "fuzzy",
            "updatedAfter",
            "updatedBefore",
            "verifiedOnly",
            "maxObjections",
            "activeSinceMs",
            "minRecentActive",
        ] {
            assert!(!payload.contains_key(key), "{key} 不应携带");
        }
        assert_eq!(parse_query(&plain).unwrap().search, SearchQuery::plain("x", 5));
    }

    /// 过滤参数类型不符 → 整帧拒绝（与 tags 校验同口径）。
    #[test]
    fn parse_search_rejects_bad_filter_types() {
        for bad in [
            serde_json::json!({"kind": "search", "query": "x", "fuzzy": 1}),
            serde_json::json!({"kind": "search", "query": "x", "updatedAfter": "now"}),
            serde_json::json!({"kind": "search", "query": "x", "verifiedOnly": "yes"}),
            serde_json::json!({"kind": "search", "query": "x", "maxObjections": -1}),
            serde_json::json!({"kind": "search", "query": "x", "minRecentActive": 1.5}),
        ] {
            assert!(parse_search(&bad).is_none(), "{bad} 应被拒绝");
        }
    }

    /// 健康信号过滤的确定性口径：无本地日志副本（health 缺席）的条目在
    /// 携带健康过滤时一律被排除；同一查询去掉过滤即返回。
    #[test]
    fn health_filter_excludes_entries_without_signals() {
        let mut s = MemoryStorage::new();
        let id = "c".repeat(64);
        let payload = serde_json::json!({
            "metaV": 1,
            "affairId": id,
            "title": "业委会选举",
            "summary": "",
            "tags": [],
            "metaSeq": 0,
            "basisOpHash": id,
            "updatedAt": 100i64,
        });
        ingest_announcement(&mut s, &payload, 1000).unwrap();

        let filtered = SearchQuery {
            text: "业委会".to_string(),
            max_objections: Some(0),
            ..SearchQuery::plain("", 10)
        };
        assert!(filtered.has_health_filter());
        let out = run_search(&s, &filtered, 2000).unwrap();
        assert_eq!(
            out["results"].as_array().unwrap().len(),
            0,
            "无健康信号 → 健康过滤排除"
        );

        let unfiltered = SearchQuery::plain("业委会", 10);
        let out = run_search(&s, &unfiltered, 2000).unwrap();
        assert_eq!(out["results"].as_array().unwrap().len(), 1);
    }

    /// health_filter_pass 纯函数口径：各过滤独立判定、缺席信号不通过。
    #[test]
    fn health_filter_pass_semantics() {
        let health = HealthSignals {
            active_participants_trend: vec![2, 0, 0, 0, 0, 0],
            adoption: crate::index::health::AdoptionConcentration {
                total_adoptions: 1,
                top_identity: None,
                top_share_permille: 0,
            },
            objection_count: 1,
            appealed_count: 1,
            last_activity_ms: Some(500),
            fork: crate::index::health::ForkLineage {
                head_count: 1,
                max_depth_ops: 2,
                heads: vec!["h".to_string()],
            },
        };
        // 无过滤 → 恒通过（含缺席）
        assert!(health_filter_pass(None, &SearchQuery::plain("x", 1)));
        assert!(health_filter_pass(Some(&health), &SearchQuery::plain("x", 1)));
        // 有过滤 + 缺席 → 不通过
        let q = SearchQuery {
            max_objections: Some(1),
            ..SearchQuery::plain("x", 1)
        };
        assert!(!health_filter_pass(None, &q));
        assert!(health_filter_pass(Some(&health), &q), "异议 1 ≤ 上限 1");
        let q = SearchQuery {
            max_objections: Some(0),
            ..SearchQuery::plain("x", 1)
        };
        assert!(!health_filter_pass(Some(&health), &q), "异议 1 > 上限 0");
        // 最近活动时间下界（含）
        let q = SearchQuery {
            active_since_ms: Some(500),
            ..SearchQuery::plain("x", 1)
        };
        assert!(health_filter_pass(Some(&health), &q));
        let q = SearchQuery {
            active_since_ms: Some(501),
            ..SearchQuery::plain("x", 1)
        };
        assert!(!health_filter_pass(Some(&health), &q));
        // 最近活跃窗口人数下限（含；last_activity 缺席同判不通过）
        let q = SearchQuery {
            min_recent_active: Some(2),
            ..SearchQuery::plain("x", 1)
        };
        assert!(health_filter_pass(Some(&health), &q));
        let q = SearchQuery {
            min_recent_active: Some(3),
            ..SearchQuery::plain("x", 1)
        };
        assert!(!health_filter_pass(Some(&health), &q));
        let mut silent = health.clone();
        silent.last_activity_ms = None;
        let q = SearchQuery {
            active_since_ms: Some(1),
            ..SearchQuery::plain("x", 1)
        };
        assert!(!health_filter_pass(Some(&silent), &q), "无锚定活动 → 不通过");
    }

    /// 环回 + 裁决：次新公告不覆盖，新代际公告替换，索引随暂存区更新。
    #[test]
    fn loopback_index_follows_arbitration() {
        let mut s = MemoryStorage::new();
        let id = "b".repeat(64);
        let mk = |meta_seq: u64, updated_at: i64| {
            serde_json::json!({
                "metaV": 1,
                "affairId": id,
                "title": format!("标题 v{meta_seq}"),
                "summary": "",
                "tags": [],
                "metaSeq": meta_seq,
                "basisOpHash": id,
                "updatedAt": updated_at,
            })
        };
        ingest_announcement(&mut s, &mk(2, 500), 1000).unwrap();
        // 旧代际（metaSeq 小）到达：不覆盖
        assert_eq!(
            ingest_announcement(&mut s, &mk(1, 900), 1100).unwrap(),
            IngestOutcome::Kept
        );
        // 新代际到达：替换，索引随之更新
        assert_eq!(
            ingest_announcement(&mut s, &mk(3, 600), 1200).unwrap(),
            IngestOutcome::Replaced
        );
        let text = build_query(
            "q2",
            &SearchQuery {
                text: "标题".to_string(),
                ..SearchQuery::plain("", 0)
            },
        );
        let parsed = parse_query(&text).unwrap();
        let out = run_search(&s, &parsed.search, 1300).unwrap();
        let hits = out["results"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["metaSeq"], 3);
        assert_eq!(hits[0]["title"], "标题 v3");
    }

    #[test]
    fn parse_query_rejects_bad_frames() {
        assert!(parse_query("not json").is_none());
        assert!(
            parse_query(
                &serde_json::json!({
                    "type": "affair-meta-query", "queryId": "", "payload": {}
                })
                .to_string()
            )
            .is_none()
        );
        assert!(
            parse_query(
                &serde_json::json!({
                    "type": "affair-meta-query", "queryId": "q", "payload": { "kind": "other" }
                })
                .to_string()
            )
            .is_none()
        );
    }

    #[test]
    fn result_error_frame_roundtrips() {
        let text = build_error("q9", "indexer-disabled");
        let payload = parse_result(&text).unwrap();
        assert_eq!(payload["error"], "indexer-disabled");
    }

    /// 响应帧 4KB 上限（§7）：超限从结果尾部截断，complete 置 false，
    /// 截断确定（同输入同输出）。
    #[test]
    fn build_result_truncates_over_cap_deterministically() {
        let hit = |i: usize| {
            serde_json::json!({
                "affairId": format!("{:064x}", i),
                "title": "第二届业委会选举候选人提名与投票安排",
                "summary": "简介".repeat(200),
                "tags": ["region:110105", "hoa"],
                "metaSeq": 0,
                "basisOpHash": "ab".repeat(32),
                "verified": false,
                "updatedAt": 1720000000000i64,
                "score": 3,
            })
        };
        let payload = serde_json::json!({
            "results": (0..50).map(hit).collect::<Vec<_>>(),
            "complete": true,
        });
        let text = build_result("q-cap", &payload);
        assert!(text.len() <= QUERY_MAX_BYTES, "frame within 4KB cap");
        let parsed = parse_result(&text).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert!(!results.is_empty() && results.len() < 50, "tail-truncated");
        assert_eq!(parsed["complete"], false, "truncation marks incomplete");
        // 截断从尾部：保留的恰是前 results.len() 条
        assert_eq!(results[0]["affairId"], format!("{:064x}", 0));
        assert_eq!(
            results.last().unwrap()["affairId"],
            format!("{:064x}", results.len() - 1)
        );
        // 确定性：同输入同输出
        assert_eq!(build_result("q-cap", &payload), text);

        // 未超限的 payload 原样保留（complete 不被改写）
        let small = serde_json::json!({ "results": [hit(0)], "complete": true });
        let small_text = build_result("q-small", &small);
        assert_eq!(parse_result(&small_text).unwrap(), small);
    }
}
