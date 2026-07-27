//! 查询路径：主键/二级索引范围扫描 + 内存 filter，以及 `sync::apply` 的集合适配器。

use super::*;

impl DocumentCollection {
    /// 查询集合：索引或主键范围扫描 + 内存 filter（TS `query`）。
    ///
    /// 扫描上界用 `U+10FFFF`（TS 为 `\xFF`，会漏非 ASCII id，见模块头注）。
    pub fn query<S: StorageBackend>(
        &self,
        storage: &S,
        options: &QueryOptions,
    ) -> Result<QueryResult> {
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
            let start = options.start_after_id.as_ref().map_or_else(
                || prefix.clone(),
                |after| format!("{}{after}\x00", self.doc_key(after)),
            );
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
    fn get(
        &self,
        storage: &dyn StorageBackend,
        id: &str,
    ) -> crate::sync::SyncResult<Option<Value>> {
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
