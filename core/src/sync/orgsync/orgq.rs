//! orgq（按需查询 / 写入受理，O3）信封 body 构造-解析 + 数据账号侧按需查询
//! 采集（分页/分批）。
//!
//! 对应 org-orgsync.md §20.5：`orgq-req`（成员 → 数据账号的按需查询/写入）与
//! `orgq-resp`（数据账号 → 成员的回执）两个方向，走 dm 直连、按 dm 默认
//! **按 from 限流**（orgq 是成员级流量，**不做全豁免**——区别于 orgsync
//! 反熵三信封，后者因背靠背多信封往返豁免，见 `p2p/direct/dm.rs`）。
//!
//! ## 线形（§20.5 + requestId 扩展）
//!
//! `requestId` 为**请求-应答关联**载体（规格未定线形，自拟）：成员发起 orgq
//! 时生成唯一 id，请求/应答信封原样携带；数据账号侧只认 requestId 匹配的
//! 应答为合法（成员侧缓存/写入回执据此路由，杜绝应答张冠李戴）。应答侧仅
//! 处理自己见过的 requestId（即 orgq-req 的接收方），否则静默丢弃。
//!
//! ```json
//! // orgq-req：查询
//! { "op":"query", "orgId":"org_...", "collection":"finance:ledger",
//!   "prefix":"2026-", "limit":500, "cursor":"<上一页末 key>", "requestId":"..." }
//! // orgq-req：写入
//! { "op":"write", "orgId":"org_...", "collection":"finance:ledger",
//!   "records":[{"key":"...","value":{...}},{"key":"...","value":null}], "requestId":"..." }
//! // orgq-resp：查询应答（records 带 meta，同 §20.4 线形；denied 仅加密/降级
//! // 时 true——非读者/插件未运行，空集 + denied:true）
//! { "orgId":"org_...", "collection":"finance:ledger", "requestId":"...",
//!   "records":[{key,value,meta}], "complete":true, "servedAt":1720000000000,
//!   "denied":false }
//! // orgq-resp：写入回执
//! { "orgId":"org_...", "collection":"finance:ledger", "requestId":"...",
//!   "accepted":2, "rejected":0, "denied":false }
//! ```
//!
//! - `limit` 缺省 500、上限 2000；`cursor` 字典序续扫（Z6 分页）；
//! - 查询应答 `records` 带 meta；写入请求 `records` **不带 meta**（pmeta 由
//!   受理方落库时生成）；
//! - `complete`：本页为末页（`cursor` 续扫已无更多）；resp 按 dm 信封体积
//!   分批（Z6），`complete` 仅末批 true；
//! - 写入回执 `denied`：数据账号侧权限/资格整批拒绝（false = 部分/全部受理）。
//!
//! ## 过滤分流
//!
//! 数据账号侧收到 orgq-req 后按集合 `confidentiality` 分流：
//! - `filtered`：逐条过插件 `canRead`/`canWrite` 钩子（在数据账号侧后台运行时
//!   执行）；**插件未运行时该集合只存不服务**（回 `denied`/降级，hello roles
//!   降标）；
//! - `encrypted`：内核按当前 acl `readers` 名单过滤（O4 填名单来源；本期查询
//!   返回明确未实现错误/占位，注释标清）。
//!
//! 本模块是**纯逻辑**（存储泛型），不触碰 p2p/签名/插件运行时——钩子执行与
//! 名单过滤的实际承载由 kernel 层完成（`inbound_dm/orgq.rs`）。
//!
//! 目录形态（Z5 650 行硬线拆分，按主题）：
//! - 本文件：orgq 信封 build/parse + 记录类型 + 数据账号侧按需查询采集/分页/分批；
//! - [`orgq_cache`]：成员侧查询结果缓存命名空间 + 淘汰；
//! - [`orgq_queue`]：成员侧离线写入队列 + 成员移除擦除；
//! - [`orgq_online`]：在线数据账号目录 + 成员侧路由决策；
//! - [`orgq_deliver`]：在途记录 / 回执 / 同步等待骨架。

use serde_json::{Value, json};

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::meta::DocMeta;

/// orgq limit 缺省值（§20.5/§20.9）。
pub const ORGQ_LIMIT_DEFAULT: usize = 500;
/// orgq limit 上限（§20.5/§20.9）。
pub const ORGQ_LIMIT_MAX: usize = 2000;

/// orgq 请求（`op` 分派）。
#[derive(Clone, Debug)]
pub enum OrgqReq {
    /// 按需查询。
    Query {
        org_id: String,
        collection: String,
        prefix: Option<String>,
        limit: usize,
        cursor: Option<String>,
        request_id: String,
    },
    /// 写入受理。
    Write {
        org_id: String,
        collection: String,
        records: Vec<OrgqWriteRecord>,
        request_id: String,
    },
}

/// orgq 写入请求中的单条记录（不带 meta——pmeta 由受理方落库时生成）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct OrgqWriteRecord {
    pub key: String,
    /// `Value::Null` = 删除（墓碑）。
    pub value: Value,
}

/// orgq 响应。
#[derive(Clone, Debug)]
pub enum OrgqResp {
    /// 查询应答。
    Query {
        org_id: String,
        collection: String,
        request_id: String,
        records: Vec<OrgqRespRecord>,
        complete: bool,
        served_at: i64,
        /// 整批拒绝（encrypted 非读者 / filtered 插件未运行降级）；true 时空集。
        denied: bool,
    },
    /// 写入回执。
    Write {
        org_id: String,
        collection: String,
        request_id: String,
        accepted: usize,
        rejected: usize,
        denied: bool,
    },
}

/// orgq 查询应答中的单条记录（带 meta，同 §20.4 线形）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OrgqRespRecord {
    pub key: String,
    pub value: Value,
    pub meta: DocMeta,
}

/// 构造 orgq-req 查询请求 body。
pub fn build_orgq_query_req(
    org_id: &str,
    collection: &str,
    prefix: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    request_id: &str,
) -> Value {
    let limit = limit.clamp(1, ORGQ_LIMIT_MAX);
    let mut body = json!({
        "op": "query",
        "orgId": org_id,
        "collection": collection,
        "limit": limit,
        "requestId": request_id,
    });
    if let Some(p) = prefix {
        body["prefix"] = json!(p);
    }
    if let Some(c) = cursor {
        body["cursor"] = json!(c);
    }
    body
}

/// 构造 orgq-req 写入请求 body（records 不带 meta）。
pub fn build_orgq_write_req(
    org_id: &str,
    collection: &str,
    records: &[OrgqWriteRecord],
    request_id: &str,
) -> Value {
    let items: Vec<Value> = records
        .iter()
        .map(|r| json!({ "key": r.key, "value": r.value }))
        .collect();
    json!({
        "op": "write",
        "orgId": org_id,
        "collection": collection,
        "records": items,
        "requestId": request_id,
    })
}

/// 解析 orgq-req body。`limit` 缺省 500、上限 2000；非法 → `None`。
pub fn parse_orgq_req(body: &Value) -> Option<OrgqReq> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let collection = body.get("collection")?.as_str()?.to_string();
    let request_id = body.get("requestId")?.as_str()?.to_string();
    match body.get("op")?.as_str()? {
        "query" => {
            let prefix = body
                .get("prefix")
                .and_then(Value::as_str)
                .map(str::to_string);
            let cursor = body
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            let limit = body
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(ORGQ_LIMIT_DEFAULT)
                .clamp(1, ORGQ_LIMIT_MAX);
            Some(OrgqReq::Query {
                org_id,
                collection,
                prefix,
                limit,
                cursor,
                request_id,
            })
        }
        "write" => {
            let records = body
                .get("records")?
                .as_array()?
                .iter()
                .map(|item| {
                    let key = item.get("key")?.as_str()?.to_string();
                    let value = item.get("value").cloned().unwrap_or(Value::Null);
                    Some(OrgqWriteRecord { key, value })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(OrgqReq::Write {
                org_id,
                collection,
                records,
                request_id,
            })
        }
        _ => None,
    }
}

/// 构造 orgq-resp 查询应答 body（records 带 meta）。
/// `denied` 缺省 false；encrypted 非读者 / filtered 插件未运行降级时为 true
/// （空集，denied:true——非读者连元数据都不给，见 §20.5）。
pub fn build_orgq_query_resp(
    org_id: &str,
    collection: &str,
    request_id: &str,
    records: &[OrgqRespRecord],
    complete: bool,
    served_at: i64,
    denied: bool,
) -> Value {
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "key": r.key,
                "value": r.value,
                "meta": serde_json::to_value(&r.meta).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "orgId": org_id,
        "collection": collection,
        "requestId": request_id,
        "records": items,
        "complete": complete,
        "servedAt": served_at,
        "denied": denied,
    })
}

/// 构造 orgq-resp 写入回执 body。
pub fn build_orgq_write_resp(
    org_id: &str,
    collection: &str,
    request_id: &str,
    accepted: usize,
    rejected: usize,
    denied: bool,
) -> Value {
    json!({
        "orgId": org_id,
        "collection": collection,
        "requestId": request_id,
        "accepted": accepted,
        "rejected": rejected,
        "denied": denied,
    })
}

/// 解析 orgq-resp body。非法 → `None`。
pub fn parse_orgq_resp(body: &Value) -> Option<OrgqResp> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let collection = body.get("collection")?.as_str()?.to_string();
    let request_id = body.get("requestId")?.as_str()?.to_string();
    // 查询应答（有 records/complete/servedAt）与写入回执（有 accepted/denied）
    // 通过请求字段有无区分（对端按请求 op 只回对应形态，字段不共存）。
    if let Some(items) = body.get("records").and_then(Value::as_array) {
        let mut records = Vec::with_capacity(items.len());
        for item in items {
            let key = item.get("key")?.as_str()?.to_string();
            let value = item.get("value").cloned().unwrap_or(Value::Null);
            let meta: DocMeta = serde_json::from_value(item.get("meta")?.clone()).ok()?;
            records.push(OrgqRespRecord { key, value, meta });
        }
        let complete = body
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let served_at = body.get("servedAt").and_then(Value::as_i64).unwrap_or(0);
        let denied = body.get("denied").and_then(Value::as_bool).unwrap_or(false);
        return Some(OrgqResp::Query {
            org_id,
            collection,
            request_id,
            records,
            complete,
            served_at,
            denied,
        });
    }
    let accepted = body.get("accepted").and_then(Value::as_u64).unwrap_or(0) as usize;
    let rejected = body.get("rejected").and_then(Value::as_u64).unwrap_or(0) as usize;
    let denied = body.get("denied").and_then(Value::as_bool).unwrap_or(false);
    Some(OrgqResp::Write {
        org_id,
        collection,
        request_id,
        accepted,
        rejected,
        denied,
    })
}

// ── 数据账号侧按需查询采集（O3：filtered 钩子放行后回真实数据）────────────

/// 采集某 org 集合数据账号侧驻留记录（`orgd:{orgId}:{name}@v{version}:` 前缀），
/// 供 orgq-req 查询在 filtered 钩子放行后组装应答。`col_full` 形如 `name@v{version}`。
/// 可选 `prefix` 过滤：按相对 key 是否以该前缀开头（`prefix` 缺省 = 全量）。
/// 每条记录带 pmeta（vv/ts），对齐 orgq-resp records 线形。
pub fn collect_orgq_records<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    prefix: Option<&str>,
) -> Vec<OrgqRespRecord> {
    let data_prefix = crate::plugindata::org_data_prefix(org_id, name, version);
    storage
        .scan(&ScanOptions::prefix(&data_prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, raw)| {
            let rel = key.strip_prefix(&data_prefix)?.to_string();
            // prefix 过滤：按相对 key 是否以指定前缀开头
            if let Some(p) = prefix {
                if !rel.starts_with(p) {
                    return None;
                }
            }
            let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let meta = crate::sync::get_personal_meta(storage, &key)
                .ok()
                .flatten()
                .unwrap_or_default();
            Some(OrgqRespRecord { key, value, meta })
        })
        .collect()
}

/// 分页采集（Z6，§20.5）：数据账号侧按 `limit`/`cursor` 做**字典序续扫**该集合
/// 驻留记录，逐条经 `allow(rel)` 过滤（filtered canRead 钩子），收集到 `limit`
/// 条为止。
///
/// - `cursor`：上一页末**相对 key**（缺省 = 从头扫），续扫从
///   `{data_prefix}{cursor}` 之后开始（与 plugindata::query 同口径）；
/// - 返回 `(records, has_more)`：`has_more` = 扫描完 limit 条后仍有**放行**
///   的记录（供调用方决定末批 complete）。`allow` 返回 false 的记录跳过，
///   不计入 limit。
pub fn collect_orgq_records_page<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    prefix: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    allow: impl Fn(&str) -> bool,
) -> (Vec<OrgqRespRecord>, bool) {
    let data_prefix = crate::plugindata::org_data_prefix(org_id, name, version);
    let match_prefix = match prefix {
        Some(p) => format!("{data_prefix}{p}"),
        None => data_prefix.clone(),
    };
    let scan_from = match cursor {
        Some(c) => format!("{data_prefix}{c}\u{0}"),
        None => match_prefix.clone(),
    };
    let options = ScanOptions {
        prefix: match_prefix,
        start: Some(scan_from),
        end: None,
        limit: Some(limit + 1), // 多取 1 判断是否还有更多
        reverse: false,
    };
    let mut records = Vec::with_capacity(limit);
    for (key, raw) in storage.scan(&options).unwrap_or_default() {
        let Some(rel) = key.strip_prefix(&data_prefix) else {
            continue;
        };
        // prefix 已由 scan 前缀覆盖；跳过 cursor 本身（start 是 cursor+'\0'，
        // 字典序上 cursor 不命中，但前缀恰相等的边界键需跳过重复）
        if let Some(c) = cursor {
            if rel == c {
                continue;
            }
        }
        if !allow(rel) {
            continue;
        }
        let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let meta = crate::sync::get_personal_meta(storage, &key)
            .ok()
            .flatten()
            .unwrap_or_default();
        records.push(OrgqRespRecord { key, value, meta });
        if records.len() >= limit {
            // 已达 limit：可能还有更多（scan 已多取 1 条放行记录 → has_more=true；
            // 否则剩余均被过滤，后续扫描已耗尽 → has_more=false）。为精确，这里
            // 保守取 true，调用方视 complete=false 让成员再发一页（空页收敛）。
            break;
        }
    }
    // has_more 精确判定：若记录数 == limit 则假设还有（成员凭 complete=false
    // 续扫，空页即收敛）；否则确无更多。
    let has_more = records.len() >= limit;
    (records, has_more)
}

// ── O4 encrypted 删除审计 ──────────────────────────────────────────────

/// encrypted 集合删除审计日志键（独立审计键族，不参与同步流量）：
/// `orgq:audit:{orgId}:{collection}:{seq:016}` = `{from,key,ts}` JSON。
/// 最简审计口径（org-orgsync §20.5 注释）：encrypted 删除是权力行为（墓碑无
/// 密文可验、非读者删除须拒绝），受理侧持久化 (from,key,ts) 供越权删除事后追查。
pub const ORGQ_AUDIT_PREFIX: &str = "orgq:audit:";

/// 追加一条 encrypted 删除审计日志条目。返回写入序号（自增，扫描取 max+1）。
/// 纯本地键（不进 orgsync/pdsync 流量）。
pub fn orgq_audit_log_delete<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
    from: &str,
    rel_key: &str,
    ts: i64,
) -> u64 {
    let prefix = format!("{ORGQ_AUDIT_PREFIX}{org_id}:{collection}:");
    let seq = storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(k, _)| k.strip_prefix(&prefix).and_then(|s| s.parse::<u64>().ok()))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
        + 1;
    let key = format!("{prefix}{seq:016}");
    let entry = serde_json::json!({ "from": from, "key": rel_key, "ts": ts });
    let _ = storage.put(&key, &entry.to_string());
    seq
}

/// 把一页 orgq 查询应答记录按**信封体积约束**切成多批（Z6，§20.5：resp 分批，
/// `complete` 仅末批 true）。`batch_bytes` 用 dm 信封体积约束
/// （`ORGSYNC_BATCH_BYTES`）。返回批序列（至少一批）；调用方按批序标记末批
/// `complete`。
pub fn split_orgq_resp_batches(
    records: Vec<OrgqRespRecord>,
    batch_bytes: usize,
) -> Vec<Vec<OrgqRespRecord>> {
    let mut batches: Vec<Vec<OrgqRespRecord>> = Vec::new();
    let mut current: Vec<OrgqRespRecord> = Vec::new();
    let mut current_bytes = 0usize;
    for rec in records {
        let bytes = serde_json::to_string(&rec.value)
            .map(|s| s.len() + rec.key.len() + 64)
            .unwrap_or(rec.key.len() + 128);
        if !current.is_empty() && current_bytes + bytes > batch_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(rec);
        current_bytes += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    if batches.is_empty() {
        // 空页：单批空集（complete 由调用方标记）
        batches.push(Vec::new());
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orgq_query_req_roundtrip() {
        let body = build_orgq_query_req(
            "org_0000000000000001",
            "finance:ledger",
            Some("2026-"),
            100,
            Some("2026-08"),
            "req-1",
        );
        let parsed = parse_orgq_req(&body).unwrap();
        match parsed {
            OrgqReq::Query {
                org_id,
                collection,
                prefix,
                limit,
                cursor,
                request_id,
            } => {
                assert_eq!(org_id, "org_0000000000000001");
                assert_eq!(collection, "finance:ledger");
                assert_eq!(prefix.as_deref(), Some("2026-"));
                assert_eq!(limit, 100);
                assert_eq!(cursor.as_deref(), Some("2026-08"));
                assert_eq!(request_id, "req-1");
            }
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn orgq_query_req_limit_default_and_cap() {
        // 缺省（请求不带 limit）→ 解析 500
        let body = json!({
            "op": "query", "orgId": "o", "collection": "c", "requestId": "r"
        });
        let parsed = parse_orgq_req(&body).unwrap();
        match parsed {
            OrgqReq::Query { limit, .. } => assert_eq!(limit, ORGQ_LIMIT_DEFAULT),
            _ => panic!("expected query"),
        }
        // 构造侧钳到上限：9999 → 2000
        let body = build_orgq_query_req("o", "c", None, 9999, None, "r");
        let parsed = parse_orgq_req(&body).unwrap();
        match parsed {
            OrgqReq::Query { limit, .. } => assert_eq!(limit, ORGQ_LIMIT_MAX),
            _ => panic!("expected query"),
        }
        // 解析侧钳上限：9999 → 2000
        let body = json!({
            "op": "query", "orgId": "o", "collection": "c",
            "limit": 9999, "requestId": "r"
        });
        let parsed = parse_orgq_req(&body).unwrap();
        match parsed {
            OrgqReq::Query { limit, .. } => assert_eq!(limit, ORGQ_LIMIT_MAX),
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn orgq_write_req_roundtrip() {
        let records = vec![
            OrgqWriteRecord {
                key: "k1".to_string(),
                value: json!({"a": 1}),
            },
            OrgqWriteRecord {
                key: "k2".to_string(),
                value: Value::Null,
            },
        ];
        let body = build_orgq_write_req("o", "c", &records, "req-2");
        let parsed = parse_orgq_req(&body).unwrap();
        match parsed {
            OrgqReq::Write {
                org_id,
                collection,
                records,
                request_id,
            } => {
                assert_eq!(org_id, "o");
                assert_eq!(collection, "c");
                assert_eq!(request_id, "req-2");
                assert_eq!(records.len(), 2);
                assert_eq!(records[0].key, "k1");
                assert_eq!(records[0].value["a"], json!(1));
                assert!(records[1].value.is_null(), "value:null = 删除墓碑");
            }
            _ => panic!("expected write"),
        }
    }

    #[test]
    fn orgq_query_resp_roundtrip() {
        let records = vec![OrgqRespRecord {
            key: "k1".to_string(),
            value: json!("v1"),
            meta: DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: 1000,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            },
        }];
        let body = build_orgq_query_resp("o", "c", "req-3", &records, true, 2000, false);
        let parsed = parse_orgq_resp(&body).unwrap();
        match parsed {
            OrgqResp::Query {
                org_id,
                collection,
                request_id,
                records,
                complete,
                served_at,
                denied,
            } => {
                assert_eq!(org_id, "o");
                assert_eq!(collection, "c");
                assert_eq!(request_id, "req-3");
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].key, "k1");
                assert_eq!(records[0].meta.vv.get("node-a"), Some(&1));
                assert!(complete);
                assert_eq!(served_at, 2000);
                assert!(!denied);
            }
            _ => panic!("expected query resp"),
        }
    }

    #[test]
    fn orgq_query_resp_denied_roundtrip() {
        // encrypted 非读者 / filtered 插件未运行降级：空集 + denied:true
        let body = build_orgq_query_resp("o", "c", "req-denied", &[], true, 2000, true);
        let parsed = parse_orgq_resp(&body).unwrap();
        match parsed {
            OrgqResp::Query {
                denied, records, ..
            } => {
                assert!(denied);
                assert!(records.is_empty(), "denied 时空集");
            }
            _ => panic!("expected query resp"),
        }
    }

    #[test]
    fn orgq_write_resp_roundtrip() {
        let body = build_orgq_write_resp("o", "c", "req-4", 2, 1, false);
        let parsed = parse_orgq_resp(&body).unwrap();
        match parsed {
            OrgqResp::Write {
                org_id,
                collection,
                request_id,
                accepted,
                rejected,
                denied,
            } => {
                assert_eq!(org_id, "o");
                assert_eq!(collection, "c");
                assert_eq!(request_id, "req-4");
                assert_eq!(accepted, 2);
                assert_eq!(rejected, 1);
                assert!(!denied);
            }
            _ => panic!("expected write resp"),
        }
    }

    #[test]
    fn orgq_resp_denied_roundtrip() {
        // denied=true：插件未运行/资格拒绝 → accepted=0 rejected=0 denied=true
        let body = build_orgq_write_resp("o", "c", "req-5", 0, 0, true);
        let parsed = parse_orgq_resp(&body).unwrap();
        match parsed {
            OrgqResp::Write { denied, .. } => assert!(denied),
            _ => panic!("expected write resp"),
        }
    }

    /// Z6 分批：一页记录按信封体积切成多批，末批外的批大小受限。
    #[test]
    fn split_orgq_resp_batches_splits_by_bytes() {
        let rec = |k: &str| OrgqRespRecord {
            key: k.to_string(),
            value: json!({"payload": "x".repeat(100)}),
            meta: Default::default(),
        };
        let records: Vec<_> = (0..10).map(|i| rec(&format!("k{i}"))).collect();
        let batches = split_orgq_resp_batches(records, 300);
        assert!(batches.len() > 1, "按体积切多批");
        // 每批（除末批）≤ 体积约束
        for (i, b) in batches.iter().enumerate() {
            if i + 1 < batches.len() {
                assert!(
                    b.iter()
                        .map(|r| r.value.to_string().len() + r.key.len())
                        .sum::<usize>()
                        <= 300,
                    "非末批须满足体积约束"
                );
            }
        }
        // 全部记录守恒
        let total: usize = batches.iter().map(|b| b.len()).sum();
        assert_eq!(total, 10);
    }
}
