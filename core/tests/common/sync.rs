//! sync_apply 集成测试共用夹具：fake CollectionAdapter、固定水位线与 meta/选项助手。

use std::collections::BTreeMap;

use serde_json::Value;
use spark_core::evidence::js_number_to_string;
use spark_core::schema::{CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::{
    ApplyRemoteOptions, CollectionAdapter, PurgeWatermark, RemoteMeta, SyncResult, VersionVector,
};

pub const DOMAIN: &str = "chat";
pub const COLLECTION: &str = "messages";
pub const NOW: i64 = 1_700_000_000_000;

/// fake 集合：对齐 TS DocumentCollection 的 docKey/indexKey/buildIndexMap。
pub struct FakeCollection {
    domain: String,
    collection: String,
    indexed_fields: Vec<String>,
}

impl FakeCollection {
    pub fn new(indexed_fields: &[&str]) -> Self {
        Self {
            domain: DOMAIN.to_string(),
            collection: COLLECTION.to_string(),
            indexed_fields: indexed_fields.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl CollectionAdapter for FakeCollection {
    fn get(&self, storage: &dyn StorageBackend, id: &str) -> SyncResult<Option<Value>> {
        let Some(raw) = storage.get(&self.doc_key(id))? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    fn doc_key(&self, id: &str) -> String {
        format!("doc:{}:{}:{id}", self.domain, self.collection)
    }

    fn index_key(&self, index_name: &str, index_value: &str, id: &str) -> String {
        format!(
            "idx:{}:{}:{index_name}:{}:{id}",
            self.domain,
            self.collection,
            spark_core::schema::encode_uri_component(index_value)
        )
    }

    fn build_index_map(&self, doc: Option<&Value>) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let Some(doc) = doc else { return map };
        for field in &self.indexed_fields {
            let Some(value) = doc.get(field) else {
                continue;
            };
            // 对齐 TS String(fieldValue)（null/undefined 跳过）
            let s = match value {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => n.as_f64().map(js_number_to_string),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            };
            if let Some(s) = s {
                map.insert(field.clone(), s);
            }
        }
        map
    }
}

/// 固定水位线：`remote_ts < watermark` 即拦截。
pub struct FixedWatermark(pub i64);

impl PurgeWatermark for FixedWatermark {
    fn is_purged_by_watermark(
        &self,
        _storage: &mut dyn StorageBackend,
        _domain: &str,
        _collection: &str,
        remote_ts: i64,
    ) -> SyncResult<bool> {
        Ok(remote_ts < self.0)
    }
}

pub fn vv(pairs: &[(&str, i64)]) -> VersionVector {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

pub fn remote_meta(pairs: &[(&str, i64)], ts: i64, node_id: &str) -> RemoteMeta {
    RemoteMeta {
        vv: vv(pairs),
        ts,
        node_id: Some(node_id.to_string()),
    }
}

pub fn default_options() -> ApplyRemoteOptions<'static> {
    ApplyRemoteOptions {
        now_ms: NOW,
        ..Default::default()
    }
}

pub fn setup() -> (MemoryStorage, FakeCollection) {
    (MemoryStorage::new(), FakeCollection::new(&["seq"]))
}

pub fn doc_key(id: &str) -> String {
    format!("doc:{DOMAIN}:{COLLECTION}:{id}")
}

pub fn index_key(value: &str, id: &str) -> String {
    format!("idx:{DOMAIN}:{COLLECTION}:seq:{value}:{id}")
}

pub fn declare_lww(s: &mut MemoryStorage, enable_evidence: bool) {
    let decl = CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence,
    };
    declare_collection_schema(s, DOMAIN, COLLECTION, &decl, NOW).unwrap();
}
