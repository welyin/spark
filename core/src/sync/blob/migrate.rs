//! 存量附件补登与引用收集（A4，协议 §16）。
//!
//! - **补登（持续调和，幂等稳态）**：每轮扫描「`pdoc:` 引用 ∩
//!   `blob:data:` 在库」的 hash——层内已完整则删 `blob:data:` 去重，
//!   否则经 `save_blob` 注册进 blob 层（manifest/chunk/presence）后删
//!   `blob:data:`。P6 hash == blob 层 cid（同一 sha256hex），既有
//!   `$blob` 引用零改写。持续调和把存量与增量（插件 saveBlob / P6 拉取
//!   落库仍先写 `blob:data:`）统一收口；
//! - **引用收集器** `collect_references`：`pdoc:` / `feed:inbox:` /
//!   `msg:item:` 三前缀递归收集 `$blob` 引用——v1 全部引用形态，
//!   **GC 安全前提**（新增引用形态必须先修协议 §16.3 再扩收集器）；
//! - **GC 周期**：`throttle_gc`（10 分钟）+ [`super::gc_unreferenced`]
//!   ——未被引用的层内 blob 清 chunk/manifest/presence（§15.5）。

use super::{SyncResult, has_blob_complete, is_hex64};
use crate::storage::{ScanOptions, StorageBackend};

/// GC 节流键（本地键，十进制 ASCII ms）。
pub const GC_LAST_KEY: &str = "blob:gc:last";
/// GC 节流间隔（与驱逐节流同口径）。
pub const GC_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// 引用收集器扫描的键前缀（v1 全部 `$blob` 引用形态，协议 §16.3）。
pub const REFERENCE_PREFIXES: &[&str] = &["pdoc:", "feed:inbox:", "msg:item:"];

/// 收集全部 `$blob` 引用 cid（去重升序，GC 与规划的共同输入）。
pub fn collect_references<S: StorageBackend>(storage: &S) -> SyncResult<Vec<String>> {
    let mut refs = std::collections::BTreeSet::new();
    for prefix in REFERENCE_PREFIXES {
        for (_key, raw) in storage.scan(&ScanOptions::prefix(*prefix))? {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            for hash in crate::plugindata::blob::blob_refs_in(&value) {
                if is_hex64(&hash) {
                    refs.insert(hash);
                }
            }
        }
    }
    Ok(refs.into_iter().collect())
}

/// 仅 `pdoc:` 引用（补登调和的驱动集——feed 收存非个人域数据不迁入，
/// 协议 §16.1）。
fn collect_pdoc_references<S: StorageBackend>(storage: &S) -> SyncResult<Vec<String>> {
    let mut refs = std::collections::BTreeSet::new();
    for (_key, raw) in storage.scan(&ScanOptions::prefix("pdoc:"))? {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        for hash in crate::plugindata::blob::blob_refs_in(&value) {
            if is_hex64(&hash) {
                refs.insert(hash);
            }
        }
    }
    Ok(refs.into_iter().collect())
}

/// 补登调和：「`pdoc:` 引用 ∩ `blob:data:` 在库」逐个迁入 blob 层。
/// 返回本轮处理数（迁入 + 去重删除）。**幂等**：稳态（无待迁项）零写。
///
/// 单个项失败（解码/注册错误）跳过——诚实留待下轮，不阻塞其余项。
pub fn reconcile_registrations<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    device_uid: &str,
    now_ms: i64,
) -> SyncResult<usize> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;

    let mut migrated = 0usize;
    for hash in collect_pdoc_references(storage)? {
        let data_key = crate::plugindata::blob::blob_data_key(&hash);
        let Some(b64) = storage.get(&data_key)? else {
            continue; // blob:data 不在库（已迁走/尚未拉到）
        };
        if has_blob_complete(storage, &hash) {
            // 层内已完整：删 blob:data 去重
            storage.delete(&data_key)?;
            migrated += 1;
            continue;
        }
        let Ok(data) = B64.decode(&b64) else {
            continue; // 存盘损坏：跳过（P6 侧语义自行兜底）
        };
        if super::save_blob(storage, node_id, device_uid, &data, now_ms).is_err() {
            continue; // 注册失败：下轮再来
        }
        // 存量访问时间未知：不写 access（= 0 最老，驱逐龄期诚实，§16.1）
        storage.delete(&super::quota::blob_access_key(&hash))?;
        storage.delete(&data_key)?;
        migrated += 1;
    }
    Ok(migrated)
}

/// GC 节流判定：距上次不足 [`GC_INTERVAL_MS`] → false。通过则记录本次。
pub fn throttle_gc<S: StorageBackend>(storage: &mut S, now_ms: i64) -> SyncResult<bool> {
    if let Some(raw) = storage.get(GC_LAST_KEY)?
        && let Ok(last) = raw.parse::<i64>()
        && now_ms - last < GC_INTERVAL_MS
    {
        return Ok(false);
    }
    storage.put(GC_LAST_KEY, &now_ms.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// 造一份「存量」：P6 形态 blob:data + pdoc 引用。
    fn seed_legacy(s: &mut MemoryStorage, data: &[u8], with_ref: bool) -> String {
        let info = crate::plugindata::blob::save_blob(s, data).unwrap();
        if with_ref {
            s.put(
                &format!("pdoc:plugin-x:doc-{}", &info.hash[..8]),
                &serde_json::json!({
                    "title": "附件文档",
                    "file": { "$blob": info.hash, "name": "a.bin" },
                })
                .to_string(),
            )
            .unwrap();
        }
        info.hash
    }

    #[test]
    fn reconcile_registrations_migrates_and_is_idempotent() {
        let mut s = MemoryStorage::new();
        let data = pattern(300_000); // 多 chunk（>256KiB）
        let hash = seed_legacy(&mut s, &data, true);
        // 未引用的不迁（feed/孤儿 blob:data 保留）
        let orphan = seed_legacy(&mut s, &pattern(100), false);

        let n = reconcile_registrations(&mut s, "node-a", "uid-a", 1000).unwrap();
        assert_eq!(n, 1, "只迁 pdoc 引用项");
        // 层内完整可读；blob:data 已删；access 未写（最老）
        assert!(has_blob_complete(&s, &hash));
        assert_eq!(
            super::super::read_blob_quiet(&s, &hash).unwrap().as_deref(),
            Some(data.as_slice())
        );
        assert!(
            s.get(&crate::plugindata::blob::blob_data_key(&hash))
                .unwrap()
                .is_none(),
            "blob:data 迁入后删除"
        );
        assert!(
            super::super::quota::get_access(&s, &hash).unwrap().is_none(),
            "存量不写 access"
        );
        // presence 已补登
        let records = super::super::list_presence(&s, &hash).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].device_uid, "uid-a");
        // 幂等：二次调和零处理
        assert_eq!(
            reconcile_registrations(&mut s, "node-a", "uid-a", 2000).unwrap(),
            0
        );
        // 孤儿未动
        assert!(
            s.get(&crate::plugindata::blob::blob_data_key(&orphan))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn collect_references_covers_all_prefixes() {
        let mut s = MemoryStorage::new();
        let h1 = "aa".repeat(32);
        let h2 = "bb".repeat(32);
        let h3 = "cc".repeat(32);
        let h_bad = "not-hex";
        s.put("pdoc:p:d1", &serde_json::json!({ "f": { "$blob": h1 } }).to_string())
            .unwrap();
        s.put(
            "feed:inbox:plug:0000000000001:f1",
            &serde_json::json!({ "payload": { "img": { "$blob": h2 } } }).to_string(),
        )
        .unwrap();
        s.put(
            "msg:item:conv:m1",
            &serde_json::json!({ "text": "见图", "att": [{ "$blob": h3 }] }).to_string(),
        )
        .unwrap();
        s.put("pdoc:p:d2", &serde_json::json!({ "f": { "$blob": h_bad } }).to_string())
            .unwrap();
        let refs = collect_references(&s).unwrap();
        assert_eq!(refs, vec![h1, h2, h3], "三前缀全覆盖 + hex64 过滤 + 去重升序");
    }

    /// GC 负向：补登 + 周期后，仍被引用的 cid 必存活；引用消失的被清。
    #[test]
    fn gc_after_reconcile_keeps_referenced_and_clears_orphaned() {
        let mut s = MemoryStorage::new();
        let d1 = pattern(1000);
        let d2 = pattern(2000);
        let h_kept = seed_legacy(&mut s, &d1, true);
        let h_doomed = seed_legacy(&mut s, &d2, true);
        reconcile_registrations(&mut s, "node-a", "uid-a", 1000).unwrap();
        assert!(has_blob_complete(&s, &h_kept) && has_blob_complete(&s, &h_doomed));

        // 删除 h_doomed 的 pdoc 引用（消息/文件删除 → 引用消失）
        s.delete(&format!("pdoc:plugin-x:doc-{}", &h_doomed[..8])).unwrap();
        let referenced = collect_references(&s).unwrap();
        assert_eq!(referenced, vec![h_kept.clone()]);
        let cleared =
            super::super::gc_unreferenced(&mut s, "node-a", "uid-a", &referenced, 3000).unwrap();
        assert_eq!(cleared, vec![h_doomed.clone()]);
        // 负向：被引用的必存活
        assert_eq!(
            super::super::read_blob_quiet(&s, &h_kept).unwrap().as_deref(),
            Some(d1.as_slice())
        );
        // 无引用的 chunk+meta+presence 全清
        assert!(super::super::get_manifest(&s, &h_doomed).unwrap().is_none());
        assert!(
            super::super::list_presence(&s, &h_doomed)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn throttle_gc_window() {
        let mut s = MemoryStorage::new();
        assert!(throttle_gc(&mut s, 1000).unwrap());
        assert!(!throttle_gc(&mut s, 1000 + GC_INTERVAL_MS - 1).unwrap());
        assert!(throttle_gc(&mut s, 1000 + GC_INTERVAL_MS).unwrap());
    }
}
