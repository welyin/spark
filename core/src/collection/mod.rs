//! 集合（collection）本地读写路径：逐行对齐 `desktop/src/main/db/collection.ts`
//! 的 `DocumentCollection`。
//!
//! - 文档键 `doc:{domain}:{collection}:{id}`，值为 `JSON.stringify(doc)`（紧凑、
//!   键序 = 对象插入序，依赖 serde_json `preserve_order`）；
//! - 二级索引键 `idx:{domain}:{collection}:{indexName}:{encodeURIComponent(value)}:{id}`，
//!   值为空串；写/删时按 `indexedFields` 做索引 diff；
//! - 写路径同时生成 meta（`sync::meta::generate_updated_meta`，nodeId 由调用方给：
//!   p2p 运行中为 peerId，否则 `local-node`）与链式存证（策略 `enableEvidence` 时）；
//! - 查询复刻 TS 的分页（`startAfterId + \x00`）、索引精确/前缀扫描、内存 filter
//!   与 `nextCursor` 语义。
//!
//! 与 TS 的有意差异（继承内核 storage 层口径，见 `storage/mod.rs` 头注）：
//! 查询的扫描上界用 `U+10FFFF` 而非 TS 的 `\xFF`——后者会漏掉首字节 > 0xC3 的
//! 非 ASCII id（如中文 id 的索引/文档键）。键格式本身与 TS 逐字节一致。
//!
//! 另两处说明：
//! - meta 时间戳与存证时间戳共用同一个 `now_ms`（TS 是相邻两次 `Date.now()`，
//!   最多差 1ms，无语义影响）；
//! - filter 的 gt/lt 等字符串比较用 Rust 字典序（UTF-8 字节序），与 JS 的 UTF-16
//!   码元序仅在「代理对字符 vs BMP 高位字符」混排时不同（极端边缘，不影响 ASCII）。

use serde_json::Value;

use crate::evidence::{
    EvidenceOp, NewEvidenceEntry, build_next_evidence_entry, evidence_batch_operations,
    js_number_to_string,
};
use crate::schema::{
    CollectionSchemaDeclaration, ResolvedCollectionPolicy, SyncStrategy, encode_uri_component,
    resolve_collection_policy,
};
use crate::storage::{BatchOperation, ScanOptions, StorageBackend};
use crate::sync::apply::CollectionAdapter;
use crate::sync::meta::{DocMeta, generate_updated_meta, meta_key};

/// 集合配置（TS `CollectionConfig`）。
///
/// `sync_strategy`/`governance`/`enable_evidence` 仅作策略兜底声明，
/// 已持久化的集合声明优先（见 `schema` 模块）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionConfig {
    /// 需要建立二级索引的字段（支持点号嵌套）。
    pub indexed_fields: Vec<String>,
    /// 存证开关兜底声明。
    pub enable_evidence: Option<bool>,
    /// 同步策略兜底声明。
    pub sync_strategy: Option<SyncStrategy>,
    /// 治理标记兜底声明。
    pub governance: Option<bool>,
}

/// 条件查询中的单个条件（TS `CollectionQueryFilter`）。
#[derive(Clone, Debug, PartialEq)]
pub struct QueryFilter {
    /// 文档字段（支持点号嵌套）。
    pub field: String,
    /// 比较值（string/number/boolean；按 JS `String(value)` 归一后比较）。
    pub value: Value,
    /// 比较操作（默认 eq）。
    pub op: FilterOp,
}

/// filter 比较操作（TS `op`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterOp {
    /// 等于（默认）。
    #[default]
    Eq,
    /// 前缀匹配。
    StartsWith,
    /// 大于（字符串字典序）。
    Gt,
    /// 小于。
    Lt,
    /// 大于等于。
    Gte,
    /// 小于等于。
    Lte,
}

/// 集合查询参数（TS `CollectionQueryOptions`）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryOptions {
    /// 二级索引名（即 indexedFields 中的字段名）。
    pub index_name: Option<String>,
    /// 索引值（string/number/boolean）。
    pub index_value: Option<Value>,
    /// `true` 时按索引值前缀匹配（缺省精确匹配）。
    pub index_prefix: bool,
    /// 分页游标（上一页 `next_cursor`）。
    pub start_after_id: Option<String>,
    /// 每页条数（默认 50）。
    pub limit: Option<usize>,
    /// 逆序。
    pub reverse: bool,
    /// 内存 filter（在扫描结果回读后应用）。
    pub filter: Vec<QueryFilter>,
}

/// 查询结果项。
#[derive(Clone, Debug, PartialEq)]
pub struct DocItem {
    /// 文档 id。
    pub id: String,
    /// 文档内容。
    pub data: Value,
}

/// 查询结果（TS `CollectionQueryResult`）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    /// 命中项。
    pub items: Vec<DocItem>,
    /// 下一页游标（仅当本页条数 == limit 时给出）。
    pub next_cursor: Option<String>,
}

/// 一次本地写/删的返回：广播同步消息所需的 meta 与策略声明副本。
#[derive(Clone, Debug, PartialEq)]
pub struct LocalWrite {
    /// 新版 meta（put：`{vv, ts, nodeId}`；delete 广播用非墓碑 meta）。
    pub meta: DocMeta,
    /// 策略声明副本（随同步消息携带，供远端兜底；不持久化）。
    pub schema: CollectionSchemaDeclaration,
}

/// 集合模块错误（消息文本与 TS 抛出的 `Error.message` 逐字一致）。
#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    /// append-only 集合拒绝覆盖已存在文档。
    #[error("Collection \"{collection}\" is append-only: document \"{id}\" already exists and cannot be overwritten")]
    AppendOnlyOverwrite {
        /// 集合名。
        collection: String,
        /// 文档 id。
        id: String,
    },

    /// append-only 集合禁止删除。
    #[error("Collection \"{0}\" is append-only: documents cannot be deleted")]
    AppendOnlyDelete(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// schema 模块错误。
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),

    /// evidence 模块错误。
    #[error(transparent)]
    Evidence(#[from] crate::evidence::EvidenceError),

    /// sync 模块错误。
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 集合模块 Result 别名。
pub type Result<T> = std::result::Result<T, CollectionError>;

/// JS `String(value)` 的 JSON 值版（索引值归一用）：
/// bool → `true/false`；number → JS 数字串（复用 evidence 的 `js_number_to_string`）；
/// string → 原样；array → 元素 `String()` 以 `,` 连接（null → 空串）；
/// object → `[object Object]`；null → `null`。
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number_to_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// TS `resolveFieldValue`：点号路径解析嵌套字段。
///
/// 对象按属性访问；数组额外支持数字下标（JS `in` 运算符对数组同样生效）。
/// 不复制 JS 原型链语义（`'length' in []` 之类，现实中不会被配成索引字段）。
fn resolve_field_value<'a>(doc: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = doc;
    for part in field.split('.') {
        match current {
            Value::Object(map) => {
                current = map.get(part)?;
            }
            Value::Array(items) => {
                let index: usize = part.parse().ok()?;
                current = items.get(index)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// 单集合文档抽象（TS `DocumentCollection`）：无状态，方法显式接收存储。
#[derive(Clone, Debug)]
pub struct DocumentCollection {
    domain: String,
    collection: String,
    indexed_fields: Vec<String>,
    policy_hint: Option<CollectionSchemaDeclaration>,
}

impl DocumentCollection {
    /// 构造集合句柄（TS 构造函数：`policyHint` 仅当显式给出策略字段时存在，
    /// 且其 `syncStrategy` 缺省按 `lww` 兜底）。
    pub fn new(
        domain: impl Into<String>,
        collection: impl Into<String>,
        config: CollectionConfig,
    ) -> Self {
        let policy_hint = if config.sync_strategy.is_some()
            || config.governance.is_some()
            || config.enable_evidence.is_some()
        {
            Some(CollectionSchemaDeclaration {
                sync_strategy: config.sync_strategy.or(Some(SyncStrategy::Lww)),
                governance: config.governance.unwrap_or(false),
                enable_evidence: config.enable_evidence.unwrap_or(false),
            })
        } else {
            None
        };
        Self {
            domain: domain.into(),
            collection: collection.into(),
            indexed_fields: config.indexed_fields,
            policy_hint,
        }
    }

    /// 数据域。
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// 集合名。
    pub fn collection(&self) -> &str {
        &self.collection
    }

    fn key_prefix(&self) -> String {
        format!("doc:{}:{}:", self.domain, self.collection)
    }

    fn index_prefix_base(&self) -> String {
        format!("idx:{}:{}:", self.domain, self.collection)
    }

    /// 主键文档键 `doc:{domain}:{collection}:{id}`。
    pub fn doc_key(&self, id: &str) -> String {
        format!("{}{id}", self.key_prefix())
    }

    /// 二级索引键 `idx:{domain}:{collection}:{indexName}:{encodeURIComponent(value)}:{id}`。
    pub fn index_key(&self, index_name: &str, index_value: &str, id: &str) -> String {
        format!(
            "{}{index_name}:{}:{id}",
            self.index_prefix_base(),
            encode_uri_component(index_value)
        )
    }

    fn index_prefix(&self, index_name: &str) -> String {
        format!("{}{index_name}:", self.index_prefix_base())
    }

    /// TS `buildIndexMap`：按 `indexed_fields` 顺序提取 `field → String(value)`；
    /// 缺失/null 字段跳过。
    fn build_index_map_ordered(&self, doc: Option<&Value>) -> Vec<(String, String)> {
        let mut map = Vec::new();
        let Some(doc) = doc else {
            return map;
        };
        for field in &self.indexed_fields {
            let Some(value) = resolve_field_value(doc, field) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            map.push((field.clone(), js_string(value)));
        }
        map
    }

    /// 解析集合当前生效的同步策略（持久化声明优先，其次构造配置，最后默认）。
    pub fn resolve_policy<S: StorageBackend>(
        &self,
        storage: &S,
    ) -> std::result::Result<ResolvedCollectionPolicy, crate::schema::SchemaError> {
        resolve_collection_policy(storage, &self.domain, &self.collection, self.policy_hint.as_ref())
    }

    /// 策略声明副本（随同步消息携带；不持久化）。
    fn policy_declaration(policy: &ResolvedCollectionPolicy) -> CollectionSchemaDeclaration {
        CollectionSchemaDeclaration {
            sync_strategy: Some(policy.sync_strategy),
            governance: policy.governance,
            enable_evidence: policy.enable_evidence,
        }
    }

    /// 读取文档；不存在返回 `Ok(None)`。
    pub fn get<S: StorageBackend>(&self, storage: &S, id: &str) -> Result<Option<Value>> {
        let Some(raw) = storage.get(&self.doc_key(id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// 写入/替换文档：维护索引 diff、生成 meta（`{vv, ts, nodeId}`）、按需追加存证，
    /// 全部经一个原子 batch 提交。append-only 集合拒绝覆盖已存在文档。
    ///
    /// 返回广播所需的 meta 与策略声明副本（p2p 推送由调用方执行，对齐 TS
    /// `put` 末尾的非阻塞 broadcast）。
    pub fn put<S: StorageBackend>(
        &self,
        storage: &mut S,
        id: &str,
        doc: &Value,
        node_id: &str,
        now_ms: i64,
    ) -> Result<LocalWrite> {
        let policy = self.resolve_policy(storage)?;
        let existing = self.get(storage, id)?;
        if policy.sync_strategy == SyncStrategy::AppendOnly && existing.is_some() {
            return Err(CollectionError::AppendOnlyOverwrite {
                collection: self.collection.clone(),
                id: id.to_string(),
            });
        }
        let old_index_map = self.build_index_map_ordered(existing.as_ref());
        let new_index_map = self.build_index_map_ordered(Some(doc));
        let mut ops = vec![BatchOperation::put(
            self.doc_key(id),
            serde_json::to_string(doc)?,
        )];

        // 删除旧索引中已变化的项
        for (field, old_value) in &old_index_map {
            if !new_index_map.iter().any(|(f, v)| f == field && v == old_value) {
                ops.push(BatchOperation::delete(self.index_key(field, old_value, id)));
            }
        }
        // 新增索引项
        for (field, new_value) in &new_index_map {
            if !old_index_map.iter().any(|(f, v)| f == field && v == new_value) {
                ops.push(BatchOperation::put(self.index_key(field, new_value, id), ""));
            }
        }

        // 新版 meta 与 doc 同 batch 提交
        let meta = generate_updated_meta(storage, node_id, &self.domain, &self.collection, id, now_ms)?;
        ops.push(BatchOperation::put(
            meta_key(&self.domain, &self.collection, id),
            serde_json::to_string(&meta)?,
        ));

        if policy.enable_evidence {
            let meta_value = serde_json::to_value(&meta)?;
            let entry = build_next_evidence_entry(
                storage,
                NewEvidenceEntry::from_parts(
                    &self.domain,
                    &self.collection,
                    id,
                    EvidenceOp::Put,
                    Some(doc),
                    Some(&meta_value),
                    now_ms,
                    node_id,
                ),
            )?;
            ops.extend(evidence_batch_operations(&entry)?);
        }

        storage.batch(ops)?;
        Ok(LocalWrite {
            meta,
            schema: Self::policy_declaration(&policy),
        })
    }

    /// 删除文档并清理索引；写墓碑 meta（`{vv, ts, tombstone: true}`）与存证。
    /// append-only 集合禁止删除；文档不存在时为空操作（返回 `Ok(None)`）。
    pub fn delete<S: StorageBackend>(
        &self,
        storage: &mut S,
        id: &str,
        node_id: &str,
        now_ms: i64,
    ) -> Result<Option<LocalWrite>> {
        let policy = self.resolve_policy(storage)?;
        if policy.sync_strategy == SyncStrategy::AppendOnly {
            return Err(CollectionError::AppendOnlyDelete(self.collection.clone()));
        }
        let Some(existing) = self.get(storage, id)? else {
            return Ok(None);
        };
        let mut ops = vec![BatchOperation::delete(self.doc_key(id))];
        for (field, value) in self.build_index_map_ordered(Some(&existing)) {
            ops.push(BatchOperation::delete(self.index_key(&field, &value, id)));
        }

        // 墓碑 meta：注意不带 nodeId（对齐 TS `{vv, ts, tombstone: true}`）
        let meta = generate_updated_meta(storage, node_id, &self.domain, &self.collection, id, now_ms)?;
        let tombstone = DocMeta {
            vv: meta.vv.clone(),
            ts: meta.ts,
            node_id: None,
            tombstone: Some(true),
        };
        ops.push(BatchOperation::put(
            meta_key(&self.domain, &self.collection, id),
            serde_json::to_string(&tombstone)?,
        ));

        if policy.enable_evidence {
            let tombstone_value = serde_json::to_value(&tombstone)?;
            let entry = build_next_evidence_entry(
                storage,
                NewEvidenceEntry::from_parts(
                    &self.domain,
                    &self.collection,
                    id,
                    EvidenceOp::Delete,
                    None,
                    Some(&tombstone_value),
                    now_ms,
                    node_id,
                ),
            )?;
            ops.extend(evidence_batch_operations(&entry)?);
        }

        storage.batch(ops)?;
        Ok(Some(LocalWrite {
            meta,
            schema: Self::policy_declaration(&policy),
        }))
    }

    /// 查询集合：索引或主键范围扫描 + 内存 filter（TS `query`）。
    ///
    /// 扫描上界用 `U+10FFFF`（TS 为 `\xFF`，会漏非 ASCII id，见模块头注）。
    pub fn query<S: StorageBackend>(&self, storage: &S, options: &QueryOptions) -> Result<QueryResult> {
        let limit = options.limit.unwrap_or(50);
        let upper = crate::storage::KEY_RANGE_UPPER_BOUND;

        let (prefix, start, end) = if let Some(index_name) = &options.index_name {
            let index_prefix = self.index_prefix(index_name);
            let encoded = options
                .index_value
                .as_ref()
                .map(|v| encode_uri_component(&js_string(v)));
            let exact = encoded.is_some() && !options.index_prefix;
            let start = match (&encoded, exact) {
                (Some(value), true) => format!("{index_prefix}{value}:"),
                (Some(value), false) => format!("{index_prefix}{value}"),
                (None, _) => index_prefix.clone(),
            };
            let end = match (&encoded, exact) {
                (Some(value), true) => format!("{index_prefix}{value}:{upper}"),
                (Some(value), false) => format!("{index_prefix}{value}{upper}"),
                (None, _) => format!("{index_prefix}{upper}"),
            };
            let start = options
                .start_after_id
                .as_ref()
                .map_or(start.clone(), |after| format!("{start}{after}\x00"));
            (index_prefix, start, end)
        } else {
            let prefix = self.key_prefix();
            let start = options
                .start_after_id
                .as_ref()
                .map_or_else(|| prefix.clone(), |after| format!("{}{after}\x00", self.doc_key(after)));
            let end = format!("{prefix}{upper}");
            (prefix.clone(), start, end)
        };

        let entries = storage.scan(&ScanOptions {
            prefix,
            start: Some(start),
            end: Some(end),
            limit: Some(limit),
            reverse: options.reverse,
        })?;

        let index_query = options.index_name.is_some();
        let mut result = QueryResult::default();
        for (key, value) in entries {
            let Some(doc_id) = self.parse_document_id(&key) else {
                continue;
            };
            let data = if index_query {
                // 索引查询回读主文档；脏索引（主文档缺失）跳过
                match self.get(storage, &doc_id)? {
                    Some(doc) => doc,
                    None => continue,
                }
            } else {
                serde_json::from_str(&value)?
            };
            if matches_filter(&data, &options.filter) {
                result.items.push(DocItem { id: doc_id, data });
            }
        }
        if result.items.len() == limit {
            result.next_cursor = result.items.last().map(|item| item.id.clone());
        }
        Ok(result)
    }

    /// TS `parseDocumentId`：主键直接剥离前缀；索引键取 `:` 分隔的末段。
    ///
    /// 有意修复：TS 要求索引键后缀 `split(':')` ≥ 4 段，而实际键形
    /// （`idx:{domain}:{collection}:{indexName}:{encValue}:{id}`，见 domain.ts 头注）
    /// 后缀恒为 3 段——TS 的索引查询因此恒为空（生产中 `indexedFields` 全为空，
    /// 该路径从未被触发，属潜在 bug）。此处按真实键形判 3 段。
    fn parse_document_id(&self, key: &str) -> Option<String> {
        let key_prefix = self.key_prefix();
        if let Some(rest) = key.strip_prefix(&key_prefix) {
            return Some(rest.to_string());
        }
        let suffix = key.strip_prefix(&self.index_prefix_base())?;
        let parts: Vec<&str> = suffix.split(':').collect();
        if parts.len() < 3 {
            return None;
        }
        Some(parts[parts.len() - 1].to_string())
    }
}

/// TS `matchesFilter`：字段缺失/null 不命中；双方按 `String(value)` 归一后比较。
fn matches_filter(doc: &Value, filter: &[QueryFilter]) -> bool {
    filter.iter().all(|condition| {
        let Some(value) = resolve_field_value(doc, &condition.field) else {
            return false;
        };
        if value.is_null() {
            return false;
        }
        let actual = js_string(value);
        let expected = js_string(&condition.value);
        match condition.op {
            FilterOp::StartsWith => actual.starts_with(&expected),
            FilterOp::Gt => actual > expected,
            FilterOp::Lt => actual < expected,
            FilterOp::Gte => actual >= expected,
            FilterOp::Lte => actual <= expected,
            FilterOp::Eq => actual == expected,
        }
    })
}

/// `sync::apply` 的集合适配器：远端应用复用同一套键布局与索引映射。
impl CollectionAdapter for DocumentCollection {
    fn get(&self, storage: &dyn StorageBackend, id: &str) -> crate::sync::SyncResult<Option<Value>> {
        let Some(raw) = storage.get(&self.doc_key(id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    fn doc_key(&self, id: &str) -> String {
        self.doc_key(id)
    }

    fn index_key(&self, index_name: &str, index_value: &str, id: &str) -> String {
        self.index_key(index_name, index_value, id)
    }

    fn build_index_map(&self, doc: Option<&Value>) -> std::collections::BTreeMap<String, String> {
        self.build_index_map_ordered(doc).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::evidence::{get_evidence_entry, get_evidence_head, verify_evidence_chain};
    use crate::schema::{declare_collection_schema, CollectionSchemaDeclaration};
    use crate::storage::MemoryStorage;
    use crate::sync::meta::get_meta;

    const NOW: i64 = 1_800_000_000_000;
    const NODE: &str = "local-node";

    fn lww_collection(indexed: &[&str]) -> DocumentCollection {
        DocumentCollection::new(
            "chat",
            "messages",
            CollectionConfig {
                indexed_fields: indexed.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        )
    }

    fn declare_lww(s: &mut MemoryStorage) {
        declare_collection_schema(
            s,
            "chat",
            "messages",
            &CollectionSchemaDeclaration::lww(),
            NOW,
        )
        .unwrap();
    }

    #[test]
    fn put_writes_doc_meta_evidence_with_ts_byte_layout() {
        let mut s = MemoryStorage::new();
        declare_lww(&mut s);
        let c = lww_collection(&[]);
        let doc = json!({"text": "hello", "from": "alice"});
        let write = c.put(&mut s, "id1", &doc, NODE, NOW).unwrap();

        // doc 键值逐字节：紧凑 JSON、键序保持插入序
        assert_eq!(
            s.get("doc:chat:messages:id1").unwrap().as_deref(),
            Some(r#"{"text":"hello","from":"alice"}"#)
        );
        // meta 键值逐字节：{vv, ts, nodeId} 键序
        assert_eq!(
            s.get("meta:chat:messages:id1").unwrap().as_deref(),
            Some(format!(r#"{{"vv":{{"{NODE}":1}},"ts":{NOW},"nodeId":"{NODE}"}}"#).as_str())
        );
        assert_eq!(write.meta.vv.get(NODE), Some(&1));

        // lww 未声明 enableEvidence → 无存证
        assert!(get_evidence_head(&s).unwrap().is_none());
    }

    #[test]
    fn put_with_evidence_and_index_diff() {
        let mut s = MemoryStorage::new();
        declare_collection_schema(
            &mut s,
            "chat",
            "messages",
            &CollectionSchemaDeclaration {
                sync_strategy: Some(SyncStrategy::Lww),
                governance: false,
                enable_evidence: true,
            },
            NOW,
        )
        .unwrap();
        let c = lww_collection(&["from", "meta.tag"]);
        c.put(&mut s, "id1", &json!({"from": "alice", "meta": {"tag": "a b"}}), NODE, NOW)
            .unwrap();
        // 索引键：值经 encodeURIComponent
        assert_eq!(
            s.get("idx:chat:messages:from:alice:id1").unwrap().as_deref(),
            Some("")
        );
        assert_eq!(
            s.get("idx:chat:messages:meta.tag:a%20b:id1").unwrap().as_deref(),
            Some("")
        );
        // 存证第 1 条
        let entry = get_evidence_entry(&s, 1).unwrap().unwrap();
        assert_eq!(entry.op, EvidenceOp::Put);
        assert_eq!(entry.prev_hash, None);
        assert_eq!(entry.node_id, NODE);

        // 更新：from 变化 → 旧索引删、新索引增；meta 计数递增
        c.put(&mut s, "id1", &json!({"from": "bob"}), NODE, NOW + 1).unwrap();
        assert!(s.get("idx:chat:messages:from:alice:id1").unwrap().is_none());
        assert!(s.get("idx:chat:messages:from:bob:id1").unwrap().is_some());
        // 字段消失的索引项也被清理
        assert!(s.get("idx:chat:messages:meta.tag:a%20b:id1").unwrap().is_none());
        let meta = get_meta(&s, "chat", "messages", "id1").unwrap().unwrap();
        assert_eq!(meta.vv.get(NODE), Some(&2));
        assert_eq!(meta.ts, NOW + 1);
        assert!(verify_evidence_chain(&s).unwrap());
    }

    #[test]
    fn append_only_rejects_overwrite_and_delete_with_ts_messages() {
        let mut s = MemoryStorage::new();
        // 未声明集合默认 append-only（最安全兜底）
        let c = lww_collection(&[]);
        c.put(&mut s, "id1", &json!({"v": 1}), NODE, NOW).unwrap();
        let err = c.put(&mut s, "id1", &json!({"v": 2}), NODE, NOW).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Collection \"messages\" is append-only: document \"id1\" already exists and cannot be overwritten"
        );
        let err = c.delete(&mut s, "id1", NODE, NOW).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Collection \"messages\" is append-only: documents cannot be deleted"
        );
        // append-only 强制存证（默认策略 enableEvidence=true）
        assert!(verify_evidence_chain(&s).unwrap());
        assert_eq!(get_evidence_head(&s).unwrap().unwrap().seq, 1);
    }

    #[test]
    fn delete_writes_tombstone_and_evidence() {
        let mut s = MemoryStorage::new();
        declare_lww(&mut s);
        let c = lww_collection(&["from"]);
        c.put(&mut s, "id1", &json!({"from": "alice"}), NODE, NOW).unwrap();
        let write = c.delete(&mut s, "id1", NODE, NOW + 5).unwrap().unwrap();
        assert!(c.get(&s, "id1").unwrap().is_none());
        assert!(s.get("idx:chat:messages:from:alice:id1").unwrap().is_none());
        // 墓碑 meta 逐字节：{vv, ts, tombstone:true}，无 nodeId
        assert_eq!(
            s.get("meta:chat:messages:id1").unwrap().as_deref(),
            Some(format!(r#"{{"vv":{{"{NODE}":2}},"ts":{},"tombstone":true}}"#, NOW + 5).as_str())
        );
        // 广播用 meta 是非墓碑版（含 nodeId）
        assert_eq!(write.meta.node_id.as_deref(), Some(NODE));
        // 不存在再删为空操作
        assert!(c.delete(&mut s, "id1", NODE, NOW).unwrap().is_none());
    }

    #[test]
    fn query_primary_pagination_reverse_and_filter() {
        let mut s = MemoryStorage::new();
        declare_lww(&mut s);
        let c = lww_collection(&[]);
        for i in 0..5 {
            c.put(&mut s, &format!("id{i}"), &json!({"n": i, "kind": if i % 2 == 0 { "even" } else { "odd" }}), NODE, NOW + i)
                .unwrap();
        }
        // 第一页（默认升序，limit 2 → next_cursor）
        let page1 = c.query(&s, &QueryOptions { limit: Some(2), ..Default::default() }).unwrap();
        assert_eq!(page1.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id0", "id1"]);
        assert_eq!(page1.next_cursor.as_deref(), Some("id1"));
        // 第二页
        let page2 = c
            .query(&s, &QueryOptions {
                limit: Some(2),
                start_after_id: page1.next_cursor.clone(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page2.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id2", "id3"]);
        assert_eq!(page2.next_cursor.as_deref(), Some("id3"));
        // 最后一页（不足 limit → 无游标）
        let page3 = c
            .query(&s, &QueryOptions {
                limit: Some(2),
                start_after_id: page2.next_cursor.clone(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page3.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id4"]);
        assert_eq!(page3.next_cursor, None);
        // 逆序
        let rev = c.query(&s, &QueryOptions { limit: Some(2), reverse: true, ..Default::default() }).unwrap();
        assert_eq!(rev.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id4", "id3"]);
        // filter：eq 命中 + gt 过滤
        let filtered = c
            .query(&s, &QueryOptions {
                limit: Some(10),
                filter: vec![QueryFilter { field: "kind".into(), value: json!("even"), op: FilterOp::Eq }],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id0", "id2", "id4"]);
        let filtered = c
            .query(&s, &QueryOptions {
                limit: Some(10),
                filter: vec![QueryFilter { field: "n".into(), value: json!(2), op: FilterOp::Gt }],
                ..Default::default()
            })
            .unwrap();
        // 数字按 String 归一比较："3" > "2" 且 "4" > "2"
        assert_eq!(filtered.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id3", "id4"]);
    }

    #[test]
    fn query_index_exact_prefix_and_stale_index() {
        let mut s = MemoryStorage::new();
        declare_lww(&mut s);
        let c = lww_collection(&["from"]);
        c.put(&mut s, "id1", &json!({"from": "alice"}), NODE, NOW).unwrap();
        c.put(&mut s, "id2", &json!({"from": "alina"}), NODE, NOW).unwrap();
        c.put(&mut s, "id3", &json!({"from": "bob"}), NODE, NOW).unwrap();
        // 中文 id 的索引键也在扫描上界内（U+10FFFF；TS `\xFF` 会漏）
        c.put(&mut s, "中文", &json!({"from": "alice"}), NODE, NOW).unwrap();

        // 精确匹配：alice 命中 id1 与中文 id
        let r = c
            .query(&s, &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("alice")),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id1", "中文"]);
        // 前缀匹配：al* 命中 alice/alina（字节序：alice:中文 < alina:id2）
        let r = c
            .query(&s, &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("al")),
                index_prefix: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(r.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["id1", "中文", "id2"]);
        // 脏索引：手写无主文档的索引键 → 跳过
        s.put("idx:chat:messages:from:ghost:missing", "").unwrap();
        let r = c
            .query(&s, &QueryOptions {
                index_name: Some("from".into()),
                index_value: Some(json!("ghost")),
                ..Default::default()
            })
            .unwrap();
        assert!(r.items.is_empty());
    }

    #[test]
    fn js_string_semantics() {
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!("a")), "a");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(2)), "2");
        assert_eq!(js_string(&json!([1, "a", null])), "1,a,");
        assert_eq!(js_string(&json!({"x": 1})), "[object Object]");
    }

    #[test]
    fn resolve_field_nested_and_array() {
        let doc = json!({"a": {"b": {"c": 7}}, "arr": [{"x": 1}]});
        assert_eq!(resolve_field_value(&doc, "a.b.c"), Some(&json!(7)));
        assert_eq!(resolve_field_value(&doc, "arr.0.x"), Some(&json!(1)));
        assert_eq!(resolve_field_value(&doc, "a.missing"), None);
        assert_eq!(resolve_field_value(&doc, "a.b.c.d"), None);
    }
}
