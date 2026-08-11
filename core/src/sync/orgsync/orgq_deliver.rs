//! O3 成员侧在线 orgq-req 投递的纯逻辑辅助（三通路共享的骨架）：在途记录
//! 读写 / TTL 清理 / 上限 / 写入回执存储 / 同步等待清除。
//!
//! 纯逻辑（存储泛型，不触碰网络/运行时）——Kernel（`data_orgq`）与 QuickJS
//! 通路（`plugin/host_env_orgq`）发完 orgq-req 后共用本模块完成在途记录生命周期
//! 与等待；实际 dm 发送（网络）由各通路注入，符合「网络只属于 p2p 模块」。

use crate::storage::{ScanOptions, StorageBackend};

/// orgq-req 在途记录键：`orgq:pending:{requestId}`。
/// 成员发出 orgq-req 时写入；应答侧（handle_orgq_resp）按 requestId 匹配后删除。
pub fn orgq_pending_key(request_id: &str) -> String {
    format!("orgq:pending:{request_id}")
}

/// 单组织在途 orgq-req 上限（F6：**按组织计数**——`orgq:pending:` 键内记录
/// orgId，计数时只统计同组织，防泄漏且与 TTL 清理（跨组织回收）一致）。
/// 超出时拒绝新请求（返回 [`orgq_pending_put`] 的 `Err`），等待 TTL 清理回收。
pub const ORGQ_PENDING_MAX: usize = 256;

/// `orgq_pending_put` 失败原因（语义化，nit）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingPutError {
    /// 该组织在途记录已达上限（[`ORGQ_PENDING_MAX`]），等待 TTL 清理回收。
    OverLimit,
}

/// orgq-req 在途记录默认 TTL（毫秒）。应答须在此窗口内到达，否则视为超时
/// 由 [`orgq_pending_cleanup_stale`] 回收（防泄漏；超时后迟到的应答因无在途
/// 记录被 handle_orgq_resp 静默丢弃）。
pub const ORGQ_PENDING_TTL_MS: i64 = 30_000;

/// 生成唯一 requestId：`ts:{monotonic}:{random}`（Z3：加随机段，防止可预测
/// 外推——requestId 在应答侧作为关联令牌，可预测会让伪造者能预填合法 id）。
pub fn orgq_gen_request_id(now_ms: i64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let rand = crate::kernel::rand_hex(4);
    format!("{now_ms:x}:{n}:{rand}")
}

/// 写入 orgq-req 在途记录（值含 orgId/collection/op/targetRootId/ts 供应答侧
/// 关联校验与 TTL 清理）。全组织在途数达上限时返回 `Err`（调用方按投递失败
/// 回退缓存语义）。
pub fn orgq_pending_put<S: StorageBackend>(
    storage: &mut S,
    request_id: &str,
    org_id: &str,
    collection: &str,
    op: &str,
    target_root_id: &str,
    now_ms: i64,
) -> Result<(), PendingPutError> {
    // F6：按组织计数——只统计同 orgId 的在途记录（与 TTL 清理扫全前缀一致，
    // 防单组织 pending 堆积拖垮他组织）。
    let same_org = storage
        .scan(&ScanOptions::prefix("orgq:pending:"))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, raw)| {
            serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .and_then(|v| {
                    v.get("orgId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .is_some_and(|oid| oid == org_id)
        })
        .count();
    if same_org >= ORGQ_PENDING_MAX {
        return Err(PendingPutError::OverLimit);
    }
    let record = serde_json::json!({
        "orgId": org_id,
        "collection": collection,
        "op": op,
        "targetRootId": target_root_id,
        "ts": now_ms,
    });
    let _ = storage.put(&orgq_pending_key(request_id), &record.to_string());
    Ok(())
}

/// 读取在途记录（未在途/不存在 → `None`）。
pub fn orgq_pending_get<S: StorageBackend>(
    storage: &S,
    request_id: &str,
) -> Option<serde_json::Value> {
    storage
        .get(&orgq_pending_key(request_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// 删除在途记录（应答已消费 / 调用方超时回收）。
pub fn orgq_pending_remove<S: StorageBackend>(storage: &mut S, request_id: &str) {
    let _ = storage.delete(&orgq_pending_key(request_id));
}

/// TTL 清理：删除所有超过 TTL 未应答的在途记录，返回清理条数。由投递路径
/// 在写新 pending 前调用（防泄漏兜底；`now_ms` 注入）。
pub fn orgq_pending_cleanup_stale<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
) -> usize {
    let prefix = "orgq:pending:";
    let mut removed = 0;
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(prefix))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in keys {
        let stale = storage
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("ts").and_then(serde_json::Value::as_i64))
            .is_some_and(|ts| now_ms.saturating_sub(ts) > ORGQ_PENDING_TTL_MS);
        if stale {
            let _ = storage.delete(&key);
            removed += 1;
        }
    }
    removed
}

/// 轮询等待在途记录被应答侧（handle_orgq_resp）消费清除。返回 `true` = 应答
/// 已到达（缓存/回执已落）；`false` = 超时（调用方清理在途并回退缓存语义）。
///
/// 这是三通路共享的同步等待骨架（纯逻辑，sleep 间隔与超时由调用方注入）：
/// Kernel（data_orgq）与 QuickJS（host_env_orgq）发完 orgq-req 后都调它。
pub fn orgq_wait_cleared<S: StorageBackend>(
    storage: &S,
    request_id: &str,
    poll_interval_ms: u64,
    timeout_ms: u64,
) -> bool {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(timeout_ms);
    loop {
        if orgq_pending_get(storage, request_id).is_none() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_interval_ms));
    }
}

/// orgq 写入回执键：`orgq:resp:{requestId}`。
/// 数据账号侧写入回执（handle_orgq_resp）落此，成员侧 `data_save` 据此判定
/// accepted/rejected/denied（`orgq:pending:` 在途记录已被应答消费删除，回执
/// 结果须独立持久化供调用方读取）。
pub fn orgq_resp_key(request_id: &str) -> String {
    format!("orgq:resp:{request_id}")
}

/// 存储 orgq 写入回执结果（含 ts 供 TTL 清理，nit）。
pub fn orgq_resp_put<S: StorageBackend>(
    storage: &mut S,
    request_id: &str,
    accepted: usize,
    rejected: usize,
    denied: bool,
    now_ms: i64,
) {
    let record = serde_json::json!({ "accepted": accepted, "rejected": rejected, "denied": denied, "ts": now_ms });
    let _ = storage.put(&orgq_resp_key(request_id), &record.to_string());
}

/// TTL 清理未消费的 orgq 写入回执（nit：`orgq:resp:` 若调用方未 `orgq_resp_take`
/// 会残留，定期回收防泄漏）。返回清理条数。
pub fn orgq_resp_cleanup_stale<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
) -> usize {
    let prefix = "orgq:resp:";
    let mut removed = 0;
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(prefix))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in keys {
        let stale = storage
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("ts").and_then(serde_json::Value::as_i64))
            .is_some_and(|ts| now_ms.saturating_sub(ts) > ORGQ_PENDING_TTL_MS);
        if stale {
            let _ = storage.delete(&key);
            removed += 1;
        }
    }
    removed
}

/// 读取并删除 orgq 写入回执（一次性消费）。
pub fn orgq_resp_take<S: StorageBackend>(
    storage: &mut S,
    request_id: &str,
) -> Option<(usize, usize, bool)> {
    let raw = storage.get(&orgq_resp_key(request_id)).ok().flatten()?;
    let _ = storage.delete(&orgq_resp_key(request_id));
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("accepted").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize,
        v.get("rejected").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize,
        v.get("denied").and_then(serde_json::Value::as_bool).unwrap_or(false),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// O3 在线投递在途记录：写入/读取/删除 + requestId 唯一性。
    #[test]
    fn orgq_pending_write_read_remove_and_unique_id() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_pending_put(&mut s, "req-1", "org_01", "c@v1", "query", "da-a", 1000).unwrap();
        let rec = orgq_pending_get(&s, "req-1").expect("在途记录已写入");
        assert_eq!(rec["orgId"], json!("org_01"));
        assert_eq!(rec["op"], json!("query"));
        assert_eq!(rec["targetRootId"], json!("da-a"), "在途记录补存目标数据账号");
        assert_eq!(rec["ts"], json!(1000));
        let a = orgq_gen_request_id(1000);
        let b = orgq_gen_request_id(1000);
        assert_ne!(a, b, "同毫秒 requestId 不重复");
        orgq_pending_remove(&mut s, "req-1");
        assert!(orgq_pending_get(&s, "req-1").is_none(), "删除后不在途");
    }

    /// O3 在线投递在途上限：达上限拒绝新请求（防泄漏）。
    #[test]
    fn orgq_pending_respects_max() {
        let mut s = crate::storage::MemoryStorage::new();
        for i in 0..ORGQ_PENDING_MAX {
            orgq_pending_put(&mut s, &format!("req-{i}"), "org_01", "c@v1", "query", "da-a", 1000)
                .unwrap();
        }
        assert!(
            orgq_pending_put(&mut s, "req-over", "org_01", "c@v1", "query", "da-a", 1000).is_err(),
            "在途达上限拒绝新请求"
        );
    }

    /// O3 TTL 清理：超过 TTL 未应答的在途记录被回收（防泄漏）。
    #[test]
    fn orgq_pending_cleanup_reaps_stale() {
        let mut s = crate::storage::MemoryStorage::new();
        let now = 1000 + ORGQ_PENDING_TTL_MS + 1000;
        s.put(
            &orgq_pending_key("req-old"),
            &json!({ "orgId": "org_01", "op": "query", "ts": now - ORGQ_PENDING_TTL_MS - 1000 })
                .to_string(),
        )
        .unwrap();
        orgq_pending_put(&mut s, "req-new", "org_01", "c@v1", "query", "da-a", now).unwrap();
        let removed = orgq_pending_cleanup_stale(&mut s, now);
        assert_eq!(removed, 1, "仅回收过期在途记录");
        assert!(orgq_pending_get(&s, "req-old").is_none());
        assert!(orgq_pending_get(&s, "req-new").is_some(), "未过期的保留");
    }

    /// O3 等待骨架：应答消费在途记录 → `orgq_wait_cleared` 返回 true（同步等待）。
    #[test]
    fn orgq_wait_cleared_returns_when_resp_consumed() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_pending_put(&mut s, "req-w", "org_01", "c@v1", "query", "da-a", 1000).unwrap();
        orgq_pending_remove(&mut s, "req-w");
        assert!(
            orgq_wait_cleared(&s, "req-w", 10, 100),
            "在途已清除 → 等待立即返回 true"
        );
    }

    /// O3 等待骨架超时：在途记录未清除 → 超时返回 false（回退缓存语义）。
    #[test]
    fn orgq_wait_cleared_times_out() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_pending_put(&mut s, "req-t", "org_01", "c@v1", "query", "da-a", 1000).unwrap();
        assert!(
            !orgq_wait_cleared(&s, "req-t", 5, 30),
            "在途未清除 → 超时返回 false"
        );
        assert!(orgq_pending_get(&s, "req-t").is_some());
    }

    /// O3 写入回执：put 后 take 一次性消费。
    #[test]
    fn orgq_resp_receipt_put_take() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_resp_put(&mut s, "req-r", 2, 1, false, 1000);
        let (a, r, d) = orgq_resp_take(&mut s, "req-r").expect("回执可取");
        assert_eq!((a, r, d), (2, 1, false));
        assert!(orgq_resp_take(&mut s, "req-r").is_none(), "回执一次性消费");
    }
}
