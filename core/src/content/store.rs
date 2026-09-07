//! 内容面 blob 存储：按 CID（SHA-256）存取、完整性校验、GC 根标记最小语义。
//!
//! 键命名空间 `cblob:`（content blob），与 pdsync 面的 `blob:` 隔离——
//! `plugindata::blob::gc_blobs` 只扫 `blob:` 前缀，两边的 GC 互不误伤：
//!
//! - `cblob:data:{cid}` → 本体（base64；storage 后端是 String 值，对齐
//!   `plugindata::blob::save_blob` 的编码口径）；
//! - `cblob:root:{cid}` → GC 根标记集合（JSON 字符串数组）：议题成员资格、
//!   用户显式收藏等「持有理由」。有根的 blob 永不回收；
//! - `cblob:unref:{cid}` → 无根首见时间戳（两段式宽限期起点）。
//!
//! GC 最小语义（两段式宽限，对齐 pdsync blob 的口径与理由——远端重拉永远
//! 可恢复，故宽限期回收而非即刻删除）：
//! - 本体有根 → 清 unref 标记；
//! - 无根且无标记 → 置标记（首见）；
//! - 无根且标记龄期 ≥ [`CONTENT_GC_GRACE_MS`] → 删除本体 + 标记；
//! - 「持有即做种」的衔接：回收本体时调用方应同步 `stop_providing_blob`
//!   （p2p 层），退出议题 = unpin 全部议题根 + 停止做种。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

use super::cid::Cid;
use super::{ContentError, Result};

/// 本体键前缀（内容面命名空间，与 pdsync `blob:data:` 隔离）。
pub const BLOB_DATA_PREFIX: &str = "cblob:data:";
/// GC 根标记键前缀。
pub const BLOB_ROOT_PREFIX: &str = "cblob:root:";
/// 无根首见标记键前缀。
pub const BLOB_UNREF_PREFIX: &str = "cblob:unref:";

/// 骨架阶段 blob 大小上限（10 MiB，与 sys.fetch / pdsync blob 同口径；
/// 分块存取与大文件支持列后续里程碑）。
pub const CONTENT_BLOB_MAX_BYTES: usize = 10 * 1024 * 1024;

/// GC 宽限期：无根持续 7 天才回收（对齐 pdsync blob 口径——墓碑永存、
/// 重拉可恢复，宽限期吸收时钟偏移与离线窗口）。
pub const CONTENT_GC_GRACE_MS: i64 = 7 * 24 * 3600 * 1000;

/// save_blob 的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlobInfo {
    /// 内容寻址标识（SHA-256 hex 小写）。
    pub cid: Cid,
    /// 原始字节数。
    pub size: u64,
}

fn data_key(cid: &str) -> String {
    format!("{BLOB_DATA_PREFIX}{cid}")
}

fn root_key(cid: &str) -> String {
    format!("{BLOB_ROOT_PREFIX}{cid}")
}

fn unref_key(cid: &str) -> String {
    format!("{BLOB_UNREF_PREFIX}{cid}")
}

/// 保存 blob 本体（幂等：同内容同 CID，重写无害）。返回计算出的 CID。
pub fn save_blob<S: StorageBackend>(storage: &mut S, data: &[u8]) -> Result<ContentBlobInfo> {
    if data.len() > CONTENT_BLOB_MAX_BYTES {
        return Err(ContentError::TooLarge(CONTENT_BLOB_MAX_BYTES));
    }
    let cid = Cid::from_data(data);
    storage.put(&data_key(cid.as_str()), &B64.encode(data))?;
    Ok(ContentBlobInfo {
        cid,
        size: data.len() as u64,
    })
}

/// 按 CID 读本体：命中后**重新计算哈希校验完整性**（内容寻址的根本承诺——
/// 存盘损坏/被篡改一律报 [`ContentError::Integrity`]，绝不返回与 CID 不符的
/// 字节）；未命中返回 `Ok(None)`。
pub fn read_blob<S: StorageBackend>(storage: &S, cid: &Cid) -> Result<Option<Vec<u8>>> {
    let Some(b64) = storage.get(&data_key(cid.as_str()))? else {
        return Ok(None);
    };
    let data = B64
        .decode(&b64)
        .map_err(|e| ContentError::Integrity(format!("stored blob base64 decode failed: {e}")))?;
    if Cid::from_data(&data) != *cid {
        return Err(ContentError::Integrity(cid.to_string()));
    }
    Ok(Some(data))
}

/// 本体是否已在本地。
pub fn has_blob<S: StorageBackend>(storage: &S, cid: &Cid) -> bool {
    storage
        .get(&data_key(cid.as_str()))
        .ok()
        .flatten()
        .is_some()
}

/// 删除本体（同时清 unref 标记；根标记保留——重新持有时无需重建持有理由）。
pub fn delete_blob<S: StorageBackend>(storage: &mut S, cid: &Cid) -> Result<()> {
    storage.delete(&data_key(cid.as_str()))?;
    storage.delete(&unref_key(cid.as_str()))?;
    Ok(())
}

/// 列出本地持有的全部 blob CID（升序）。
pub fn list_blobs<S: StorageBackend>(storage: &S) -> Result<Vec<Cid>> {
    let mut out = Vec::new();
    for (key, _v) in storage.scan(&ScanOptions::prefix(BLOB_DATA_PREFIX))? {
        let text = key.trim_start_matches(BLOB_DATA_PREFIX);
        // 存储区出现非法 CID 键属于内部损坏，跳过而非整体失败
        if let Ok(cid) = Cid::parse(text) {
            out.push(cid);
        }
    }
    Ok(out)
}

/// 打 GC 根标记（幂等）：`root` 是持有理由标签（如 `topic:{topicId}`、
/// `user-pin`）。有根的 blob 不参与回收。
pub fn pin_root<S: StorageBackend>(storage: &mut S, cid: &Cid, root: &str) -> Result<()> {
    let mut roots = roots_of(storage, cid)?;
    if !roots.iter().any(|r| r == root) {
        roots.push(root.to_string());
        roots.sort();
        storage.put(&root_key(cid.as_str()), &serde_json::to_string(&roots)?)?;
    }
    // 有根即不在回收路径上：清掉可能存在的首见标记
    storage.delete(&unref_key(cid.as_str()))?;
    Ok(())
}

/// 移除一个 GC 根标记（幂等）；最后一个根移除后 blob 进入宽限期回收路径。
pub fn unpin_root<S: StorageBackend>(storage: &mut S, cid: &Cid, root: &str) -> Result<()> {
    let roots = roots_of(storage, cid)?;
    let kept: Vec<String> = roots.into_iter().filter(|r| r != root).collect();
    if kept.is_empty() {
        storage.delete(&root_key(cid.as_str()))?;
    } else {
        storage.put(&root_key(cid.as_str()), &serde_json::to_string(&kept)?)?;
    }
    Ok(())
}

/// 某 CID 的当前根标记集合（升序）。
pub fn roots_of<S: StorageBackend>(storage: &S, cid: &Cid) -> Result<Vec<String>> {
    match storage.get(&root_key(cid.as_str()))? {
        None => Ok(Vec::new()),
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
    }
}

/// 无根 blob 回收（两段式宽限）：
/// - 有根本体 → 清 unref 标记（重新有根的不再被误回收）；
/// - 无根无标记 → 置首见标记；
/// - 无根且标记龄期 ≥ [`CONTENT_GC_GRACE_MS`] → 回收本体 + 标记；
/// - 顺带清理无本体的孤儿 unref 标记。
///
/// 返回回收的 CID 列表（调用方应对每个返回 CID `stop_providing_blob`
/// 停止做种）。
pub fn gc_sweep<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<Vec<Cid>> {
    let mut collected = Vec::new();
    for (key, _v) in storage.scan(&ScanOptions::prefix(BLOB_DATA_PREFIX))? {
        let text = key.trim_start_matches(BLOB_DATA_PREFIX);
        let Ok(cid) = Cid::parse(text) else {
            continue;
        };
        if !roots_of(storage, &cid)?.is_empty() {
            let _ = storage.delete(&unref_key(cid.as_str()));
            continue;
        }
        match storage.get(&unref_key(cid.as_str()))? {
            None => {
                storage.put(&unref_key(cid.as_str()), &now_ms.to_string())?;
            }
            Some(raw) => {
                let since = raw.parse::<i64>().unwrap_or(now_ms);
                if now_ms - since >= CONTENT_GC_GRACE_MS {
                    storage.delete(&key)?;
                    storage.delete(&unref_key(cid.as_str()))?;
                    collected.push(cid);
                }
            }
        }
    }
    // 孤儿 unref 标记（本体已不在）同步清理
    let orphans: Vec<String> = storage
        .scan(&ScanOptions::prefix(BLOB_UNREF_PREFIX))?
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| {
            let cid = k.trim_start_matches(BLOB_UNREF_PREFIX);
            storage.get(&data_key(cid)).ok().flatten().is_none()
        })
        .collect();
    for key in orphans {
        storage.delete(&key)?;
    }
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn save_read_roundtrip_and_dedup() {
        let mut s = MemoryStorage::new();
        let data = b"content plane".repeat(100);
        let a = save_blob(&mut s, &data).unwrap();
        let b = save_blob(&mut s, &data).unwrap();
        assert_eq!(a, b, "同内容同 CID");
        assert_eq!(read_blob(&s, &a.cid).unwrap().unwrap(), data);
        assert!(has_blob(&s, &a.cid));
        // 未命中
        let missing = Cid::from_data(b"absent");
        assert!(read_blob(&s, &missing).unwrap().is_none());
        assert!(!has_blob(&s, &missing));
        assert_eq!(list_blobs(&s).unwrap(), vec![a.cid.clone()]);
    }

    #[test]
    fn read_verifies_integrity() {
        let mut s = MemoryStorage::new();
        let info = save_blob(&mut s, b"original").unwrap();
        // 篡改存盘内容：同形状 base64、不同字节
        s.put(
            &data_key(info.cid.as_str()),
            &B64.encode(b"tampered-bytes"),
        )
        .unwrap();
        let err = read_blob(&s, &info.cid).unwrap_err();
        assert!(matches!(err, ContentError::Integrity(_)), "got: {err}");
        // 损坏的 base64 同样报完整性错误
        s.put(&data_key(info.cid.as_str()), "!!!not-base64!!!")
            .unwrap();
        assert!(matches!(
            read_blob(&s, &info.cid).unwrap_err(),
            ContentError::Integrity(_)
        ));
    }

    #[test]
    fn oversized_rejected() {
        let mut s = MemoryStorage::new();
        let big = vec![0u8; CONTENT_BLOB_MAX_BYTES + 1];
        assert!(matches!(
            save_blob(&mut s, &big).unwrap_err(),
            ContentError::TooLarge(_)
        ));
    }

    #[test]
    fn root_pin_unpin_semantics() {
        let mut s = MemoryStorage::new();
        let info = save_blob(&mut s, b"file").unwrap();
        assert!(roots_of(&s, &info.cid).unwrap().is_empty());

        pin_root(&mut s, &info.cid, "topic:t1").unwrap();
        pin_root(&mut s, &info.cid, "topic:t1").unwrap(); // 幂等
        pin_root(&mut s, &info.cid, "user-pin").unwrap();
        assert_eq!(
            roots_of(&s, &info.cid).unwrap(),
            vec!["topic:t1".to_string(), "user-pin".to_string()]
        );

        unpin_root(&mut s, &info.cid, "topic:t1").unwrap();
        unpin_root(&mut s, &info.cid, "topic:t1").unwrap(); // 幂等
        assert_eq!(roots_of(&s, &info.cid).unwrap(), vec!["user-pin".to_string()]);
        unpin_root(&mut s, &info.cid, "user-pin").unwrap();
        assert!(roots_of(&s, &info.cid).unwrap().is_empty());
        // 键本身也清掉（不留空数组残留）
        assert!(s.get(&root_key(info.cid.as_str())).unwrap().is_none());
    }

    #[test]
    fn gc_two_phase_grace_and_repin() {
        let mut s = MemoryStorage::new();
        let rooted = save_blob(&mut s, b"rooted").unwrap();
        let orphan = save_blob(&mut s, b"orphan").unwrap();
        pin_root(&mut s, &rooted.cid, "topic:t1").unwrap();

        // 第一轮：orphan 置首见标记，不回收
        let collected = gc_sweep(&mut s, 1_000_000).unwrap();
        assert!(collected.is_empty());
        assert!(has_blob(&s, &orphan.cid), "宽限期内保留");

        // 宽限期未满不回收
        assert!(gc_sweep(&mut s, 1_000_000 + CONTENT_GC_GRACE_MS - 1)
            .unwrap()
            .is_empty());

        // 宽限期满：回收 orphan；rooted 不动
        let collected = gc_sweep(&mut s, 1_000_000 + CONTENT_GC_GRACE_MS).unwrap();
        assert_eq!(collected, vec![orphan.cid.clone()]);
        assert!(!has_blob(&s, &orphan.cid));
        assert!(has_blob(&s, &rooted.cid));

        // 重新有根后清标记：回到回收路径外
        let back = save_blob(&mut s, b"back").unwrap();
        gc_sweep(&mut s, 2_000_000).unwrap(); // 置标记
        pin_root(&mut s, &back.cid, "topic:t2").unwrap();
        assert!(
            s.get(&unref_key(back.cid.as_str())).unwrap().is_none(),
            "有根即清首见标记"
        );
        gc_sweep(&mut s, 2_000_000 + CONTENT_GC_GRACE_MS).unwrap();
        assert!(has_blob(&s, &back.cid), "重新有根后不被回收");
    }

    #[test]
    fn gc_cleans_orphan_unref_marks() {
        let mut s = MemoryStorage::new();
        let info = save_blob(&mut s, b"x").unwrap();
        gc_sweep(&mut s, 1000).unwrap(); // 置标记
        delete_blob(&mut s, &info.cid).unwrap(); // 本体删除会连带清标记
        // 手工造一个孤儿标记
        s.put(&unref_key(&"b".repeat(64)), "1000").unwrap();
        gc_sweep(&mut s, 2000).unwrap();
        assert!(
            s.get(&unref_key(&"b".repeat(64))).unwrap().is_none(),
            "无本体的孤儿标记被清理"
        );
    }

    #[test]
    fn delete_clears_unref_but_keeps_roots() {
        let mut s = MemoryStorage::new();
        let info = save_blob(&mut s, b"y").unwrap();
        pin_root(&mut s, &info.cid, "topic:t1").unwrap();
        gc_sweep(&mut s, 1000).unwrap();
        delete_blob(&mut s, &info.cid).unwrap();
        assert!(!has_blob(&s, &info.cid));
        assert_eq!(
            roots_of(&s, &info.cid).unwrap(),
            vec!["topic:t1".to_string()],
            "根标记保留——重新持有时无需重建持有理由"
        );
    }
}
