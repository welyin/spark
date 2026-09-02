//! orgq（O3）成员侧离线写入队列：`orgq:queue:{orgId}:{collection}:`。
//!
//! 成员对 data-accounts 集合的写请求在**全部数据账号离线**时落此队列持久化，
//! 数据账号上线（hello roles 含 data 到达）后经统一同步链路冲刷（send-then-
//! delete，见 `kernel/inbound_dm/orgsync.rs`）。纯逻辑（存储泛型）。
//!
//! 从 `orgq.rs` 拆出的子模块（Z5 650 行硬线）。

use crate::storage::{ScanOptions, StorageBackend};
use serde_json::Value;

/// 离线写入队列前缀：`orgq:queue:{orgId}:{collection}:`。
/// 队列值 = orgq-req 写入请求 body（含 records + requestId），flush 时装配信封。
pub fn orgq_queue_prefix(org_id: &str, collection: &str) -> String {
    format!("orgq:queue:{org_id}:{collection}:")
}

/// 单条离线写入队列键：`orgq:queue:{orgId}:{collection}:{key}`（按相对 key 去重，
/// 同 key 后写覆盖——离线期间对同一 key 的多次写只保留最新，与 LWW 收敛一致）。
pub fn orgq_queue_key(org_id: &str, collection: &str, key: &str) -> String {
    format!("{}{}", orgq_queue_prefix(org_id, collection), key)
}

/// 某组织是否已有离线写入排队（`has_queue` 判定辅助）。
pub fn orgq_queue_has_data<S: StorageBackend>(storage: &S, org_id: &str) -> bool {
    !storage
        .scan(&ScanOptions::prefix(&format!("orgq:queue:{org_id}:")))
        .unwrap_or_default()
        .is_empty()
}

/// 成员侧离线写入入队：全部数据账号离线时，把对 data-accounts 集合的单条写
/// 请求（save=value、del=Value::Null 墓碑）写入本地 orgq 队列持久化，数据账号
/// 上线（hello roles 含 data）后经 `orgq_queue_read_by_org` 冲刷为 orgq-req
/// 写入信封。
///
/// 队列值 = `{"collection": "<集合全名 name@v{version}>", "key": "<相对key>",
/// "value": ...}`——collection 语义权威在值内（自身含冒号），键仅作去重与
/// 定位。按相对 key 去重：离线期间同 key 多次写只留最新（LWW 收敛一致）。
pub fn orgq_queue_put<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), crate::storage::StorageError> {
    let record = serde_json::json!({
        "collection": collection,
        "key": key,
        "value": value,
    });
    storage.put(
        &orgq_queue_key(org_id, collection, key),
        &record.to_string(),
    )
}

/// 某集合全部离线写入队列条目（重放为 orgq-req 写入记录）。队列值存
/// `{"key":rel,"value":...}`（写请求记录，不带 meta）；drain 即重放并删除。
/// 返回 `OrgqWriteRecord` 列表——flush 时逐条重放为 orgq-req 写入信封。
pub fn orgq_queue_drain<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
) -> Vec<crate::sync::orgsync::OrgqWriteRecord> {
    let prefix = orgq_queue_prefix(org_id, collection);
    let mut drained = Vec::new();
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in keys {
        let rel = key.strip_prefix(&prefix).unwrap_or(&key).to_string();
        let value = storage
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|rec| rec.get("value").cloned())
            .unwrap_or(Value::Null);
        let _ = storage.delete(&key);
        drained.push(crate::sync::orgsync::OrgqWriteRecord { key: rel, value });
    }
    drained
}

/// 某组织全部离线写入队列按集合分组：(collection, 待重放记录)。上线冲刷用。
/// 队列值存 `{"collection": "<集合名>", "key": "<相对key>", "value": ...}`——
/// collection 自身含冒号（`{plugin}:{rest}`），故从值内读取而非解析键（键仅作
/// 去重与定位）。返回后删除（重放不留残）。
pub fn orgq_queue_drain_by_org<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
) -> Vec<(String, Vec<crate::sync::orgsync::OrgqWriteRecord>)> {
    let by_collection = orgq_queue_read_by_org(storage, org_id);
    for key in storage
        .scan(&ScanOptions::prefix(&format!("orgq:queue:{org_id}:")))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
    {
        let _ = storage.delete(&key);
    }
    by_collection
}

/// 只读读取某组织全部离线写入队列按集合分组（**不删除**，send-then-delete
/// 冲刷用——投递失败保留队列条目重新投递）。
pub fn orgq_queue_read_by_org<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Vec<(String, Vec<crate::sync::orgsync::OrgqWriteRecord>)> {
    let prefix = format!("orgq:queue:{org_id}:");
    let mut by_collection: std::collections::HashMap<
        String,
        Vec<crate::sync::orgsync::OrgqWriteRecord>,
    > = std::collections::HashMap::new();
    for (key, _) in storage
        .scan(&ScanOptions::prefix(&prefix))
        .unwrap_or_default()
    {
        let rec: Value = storage
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(Value::Null);
        let collection = rec.get("collection").and_then(Value::as_str).unwrap_or("");
        let rel = rec.get("key").and_then(Value::as_str).unwrap_or("");
        let value = rec.get("value").cloned().unwrap_or(Value::Null);
        if !collection.is_empty() {
            by_collection
                .entry(collection.to_string())
                .or_default()
                .push(crate::sync::orgsync::OrgqWriteRecord {
                    key: rel.to_string(),
                    value,
                });
        }
    }
    by_collection.into_iter().collect()
}

/// 删除某集合的全部离线写入队列条目（受理回执确认后清理，send-then-delete
/// 冲刷的删除端）。返回删除条数。
pub fn orgq_queue_clear_by_collection<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
) -> usize {
    let prefix = orgq_queue_prefix(org_id, collection);
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let mut removed = 0;
    for key in keys {
        if storage.delete(&key).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 成员被移除时尽力擦除该组织的成员侧缓存与离线写入队列（`best-effort`）。
/// 覆盖 `orgq:cache:{orgId}:*` 与 `orgq:queue:{orgId}:*` 命名空间——缓存非副本、
/// 移除后不再有资格持有；离线队列同理（对已退出组织无意义）。
/// 同时清理在线数据账号目录（`orgq:da:online:{orgId}:*`，避免陈旧提示残留）。
pub fn orgq_wipe_org_local<S: StorageBackend>(storage: &mut S, org_id: &str) -> usize {
    let mut removed = 0;
    for prefix in [
        format!("orgq:cache:{org_id}:"),
        format!("orgq:queue:{org_id}:"),
        format!("orgq:da:online:{org_id}:"),
    ] {
        let keys: Vec<String> = storage
            .scan(&ScanOptions::prefix(&prefix))
            .unwrap_or_default()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        for key in keys {
            let _ = storage.delete(&key);
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// O3 成员写入主动入队：`orgq_queue_put` 写入队列值
    /// `{"collection":..., "key":..., "value":...}`，drain 后重放为写请求记录。
    #[test]
    fn orgq_queue_put_enqueues_and_drains() {
        let mut s = crate::storage::MemoryStorage::new();
        let col = "finance:ledger@v1";
        // save：写值
        orgq_queue_put(&mut s, "org_01", col, "k1", &json!({"amt": 1})).unwrap();
        // del：写墓碑（value:null）
        orgq_queue_put(&mut s, "org_01", col, "k2", &Value::Null).unwrap();
        assert!(orgq_queue_has_data(&s, "org_01"));
        let drained = orgq_queue_drain(&mut s, "org_01", col);
        assert_eq!(drained.len(), 2);
        let k1 = drained.iter().find(|r| r.key == "k1").unwrap();
        assert_eq!(k1.value["amt"], json!(1), "save 入队保留值");
        let k2 = drained.iter().find(|r| r.key == "k2").unwrap();
        assert!(k2.value.is_null(), "del 入队为墓碑 null");
        // drain 后队列清空
        assert!(!orgq_queue_has_data(&s, "org_01"));
    }
}
