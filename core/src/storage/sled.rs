//! sled 持久化后端：kernel 门面的默认存储引擎。
//!
//! - 目录即数据库：`SledStorage::open(path)` 打开（不存在则创建）；
//! - `Clone` 共享同一 `sled::Db`（内部 Arc，线程安全）——p2p 事件循环与
//!   kernel 门面可各持一份句柄并发访问同一库；
//! - 键序为字节序，与 Rust `str` 的字典序（UTF-8 字节序）一致，
//!   故 scan 语义与 [`super::MemoryStorage`] 逐条对齐（见 `contract` 契约测试）；
//! - `batch` 用 `sled::Batch` + `apply_batch` 保证原子应用。

use std::ops::Bound;
use std::path::Path;

use super::{BatchOperation, Result, ScanOptions, StorageBackend, StorageError};

fn backend_err(context: &str, e: impl std::fmt::Display) -> StorageError {
    StorageError::Backend(format!("sled {context}: {e}"))
}

/// sled 持久化后端。
#[derive(Clone)]
pub struct SledStorage {
    db: sled::Db,
}

impl SledStorage {
    /// 打开（或创建）指定目录的 sled 数据库。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = sled::open(path.as_ref()).map_err(|e| backend_err("open", e))?;
        Ok(Self { db })
    }

    /// 刷盘（kernel shutdown 时调用；sled 自身也有周期 flush，此为确定性收尾）。
    pub fn flush(&self) -> Result<()> {
        self.db.flush().map_err(|e| backend_err("flush", e))?;
        Ok(())
    }

    /// 当前键值对数量（诊断用）。
    pub fn len(&self) -> usize {
        self.db.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.db.is_empty()
    }
}

impl std::fmt::Debug for SledStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SledStorage")
            .field("len", &self.db.len())
            .finish_non_exhaustive()
    }
}

impl StorageBackend for SledStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .db
            .get(key.as_bytes())
            .map_err(|e| backend_err("get", e))?;
        value
            .map(|v| {
                String::from_utf8(v.to_vec()).map_err(|e| backend_err("get utf8", e))
            })
            .transpose()
    }

    fn put(&mut self, key: &str, value: &str) -> Result<()> {
        self.db
            .insert(key.as_bytes(), value.as_bytes())
            .map_err(|e| backend_err("put", e))?;
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        self.db
            .remove(key.as_bytes())
            .map_err(|e| backend_err("delete", e))?;
        Ok(())
    }

    fn batch(&mut self, operations: Vec<BatchOperation>) -> Result<()> {
        let mut batch = sled::Batch::default();
        for op in operations {
            match op {
                BatchOperation::Put { key, value } => {
                    batch.insert(key.into_bytes(), value.into_bytes());
                }
                BatchOperation::Delete { key } => {
                    batch.remove(key.into_bytes());
                }
            }
        }
        self.db
            .apply_batch(batch)
            .map_err(|e| backend_err("batch", e))?;
        Ok(())
    }

    fn scan(&self, options: &ScanOptions) -> Result<Vec<(String, String)>> {
        let start = options.resolved_start().into_bytes();
        let end = options.resolved_end().into_bytes();
        let range = self
            .db
            .range((Bound::Included(start), Bound::Excluded(end)));
        let iter: Box<dyn Iterator<Item = std::result::Result<(sled::IVec, sled::IVec), sled::Error>>> =
            if options.reverse {
                Box::new(range.rev())
            } else {
                Box::new(range)
            };
        let mut items = Vec::new();
        for item in iter.take(options.limit.unwrap_or(usize::MAX)) {
            let (k, v) = item.map_err(|e| backend_err("scan", e))?;
            let key = String::from_utf8(k.to_vec()).map_err(|e| backend_err("scan utf8", e))?;
            let value = String::from_utf8(v.to_vec()).map_err(|e| backend_err("scan utf8", e))?;
            items.push((key, value));
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::contract::check_storage_contract;

    #[test]
    fn sled_passes_storage_contract() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = SledStorage::open(dir.path()).unwrap();
        check_storage_contract(&mut storage);
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = SledStorage::open(dir.path()).unwrap();
            s.put("doc:a:1", "v1").unwrap();
            s.batch(vec![
                BatchOperation::put("doc:a:2", "v2"),
                BatchOperation::put("meta:a:1", "{\"ts\":1}"),
            ])
            .unwrap();
            s.flush().unwrap();
        }
        let s = SledStorage::open(dir.path()).unwrap();
        assert_eq!(s.get("doc:a:1").unwrap(), Some("v1".to_string()));
        assert_eq!(
            s.scan(&ScanOptions::prefix("doc:a:")).unwrap().len(),
            2
        );
    }

    #[test]
    fn clone_shares_same_db() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = SledStorage::open(dir.path()).unwrap();
        let b = a.clone();
        a.put("k", "v").unwrap();
        assert_eq!(b.get("k").unwrap(), Some("v".to_string()));
    }
}
