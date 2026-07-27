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
//!
//! 代码组织：本文件为公共类型/错误/键布局与只读点查；写路径（put/delete，
//! 含索引 diff、meta 与存证）在 `write`，查询（主键/索引扫描 + 内存 filter）
//! 与 `sync::apply` 适配器在 `query`；单测在 `tests`。

mod query;
#[cfg(test)]
mod tests;
mod write;

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
    #[error(
        "Collection \"{collection}\" is append-only: document \"{id}\" already exists and cannot be overwritten"
    )]
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
        resolve_collection_policy(
            storage,
            &self.domain,
            &self.collection,
            self.policy_hint.as_ref(),
        )
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
}
