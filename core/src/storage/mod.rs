//! 存储抽象：对齐 TS `LevelDB` 封装（desktop/src/main/db/base.ts）的最小键值接口。
//!
//! - 键值均为 `String`（TS 侧 `Level<string, string>`，`valueEncoding: 'utf8'`）
//! - 批量操作 `BatchOperation::{Put, Delete}` 对应 TS `{type:'put'|'del'}`
//! - 前缀范围扫描 `scan` 对应 `queryRange`：`start` 含下界（gte）、`end` 排他上界（lt）
//!
//! 扫描上界语义（重要，见 core/spec/data-mgmt.md）：
//! TS `queryRange` 默认上界为 `prefix + '\xFF'`（U+00FF，UTF-8 编码 C3 BF），
//! 会静默漏扫首字节 > 0xC3 的非 ASCII key（如中文 id）；data-management 模块已统一
//! 改用 `U+10FFFF`。Rust 内核直接以 `U+10FFFF` 作为默认上界，不继承 `\xFF` 的历史坑。
//!
//! 同步接口设计：后端实现自行决定如何在异步运行时中使用（调用方包 tokio 即可）。

use std::collections::BTreeMap;
use std::ops::Bound;

#[cfg(test)]
pub(crate) mod contract;
pub mod sled;

pub use sled::SledStorage;

/// 前缀范围扫描的排他上界字符（`\u{10FFFF}`，UTF-8 编码 F4 8F BF BF）。
pub const KEY_RANGE_UPPER_BOUND: char = '\u{10FFFF}';

/// 计算前缀扫描的默认排他上界键：`prefix + U+10FFFF`。
pub fn prefix_range_end(prefix: &str) -> String {
    let mut end = String::with_capacity(prefix.len() + 4);
    end.push_str(prefix);
    end.push(KEY_RANGE_UPPER_BOUND);
    end
}

/// 存储模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 后端错误（连接、IO、序列化等）。
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// 存储模块 Result 别名。
pub type Result<T> = std::result::Result<T, StorageError>;

/// 批量操作项，对应 TS `LevelDBOperation`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOperation {
    /// 写入键值。
    Put { key: String, value: String },
    /// 删除键（TS `type: 'del'`）。
    Delete { key: String },
}

impl BatchOperation {
    /// 构造 put 操作。
    pub fn put(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Put {
            key: key.into(),
            value: value.into(),
        }
    }

    /// 构造 delete 操作。
    pub fn delete(key: impl Into<String>) -> Self {
        Self::Delete { key: key.into() }
    }
}

/// 前缀范围扫描选项，对应 TS `queryRange` 的参数。
///
/// 范围语义：`start`（含，默认 `prefix`）≤ key < `end`（排他，默认 `prefix + U+10FFFF`）。
#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    /// 前缀（`start`/`end` 缺省时由其推导）。
    pub prefix: String,
    /// 含下界（gte）；缺省为 `prefix`。
    pub start: Option<String>,
    /// 排他上界（lt）；缺省为 `prefix + U+10FFFF`。
    pub end: Option<String>,
    /// 最多返回条数（正序取前 N 条；`reverse` 时取最后 N 条）。
    pub limit: Option<usize>,
    /// 逆序返回。
    pub reverse: bool,
}

impl ScanOptions {
    /// 以前缀构造扫描选项（其余取默认）。
    pub fn prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            ..Default::default()
        }
    }

    /// 解析后的含下界。
    pub fn resolved_start(&self) -> String {
        self.start.clone().unwrap_or_else(|| self.prefix.clone())
    }

    /// 解析后的排他上界。
    pub fn resolved_end(&self) -> String {
        self.end.clone().unwrap_or_else(|| prefix_range_end(&self.prefix))
    }
}

/// 键值存储后端抽象（对齐 TS `LevelDB`：get/put/del/batch/queryRange）。
pub trait StorageBackend {
    /// 读取指定键的值，不存在返回 `Ok(None)`。
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// 写入键值。
    fn put(&mut self, key: &str, value: &str) -> Result<()>;

    /// 删除键（不存在时不报错，与 LevelDB del 语义一致）。
    fn delete(&mut self, key: &str) -> Result<()>;

    /// 原子批量操作（按顺序应用）。
    fn batch(&mut self, operations: Vec<BatchOperation>) -> Result<()>;

    /// 前缀范围扫描，按键升序（`reverse` 时降序）返回键值对。
    fn scan(&self, options: &ScanOptions) -> Result<Vec<(String, String)>>;
}

/// 内存后端（`BTreeMap`）：供单元测试与早期集成使用。
#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    map: BTreeMap<String, String>,
}

impl MemoryStorage {
    /// 创建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前键值对数量。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl StorageBackend for MemoryStorage {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.map.get(key).cloned())
    }

    fn put(&mut self, key: &str, value: &str) -> Result<()> {
        self.map.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        self.map.remove(key);
        Ok(())
    }

    fn batch(&mut self, operations: Vec<BatchOperation>) -> Result<()> {
        for op in operations {
            match op {
                BatchOperation::Put { key, value } => {
                    self.map.insert(key, value);
                }
                BatchOperation::Delete { key } => {
                    self.map.remove(&key);
                }
            }
        }
        Ok(())
    }

    fn scan(&self, options: &ScanOptions) -> Result<Vec<(String, String)>> {
        let start = options.resolved_start();
        let end = options.resolved_end();
        // start > end 时 BTreeMap::range 会 panic；LevelDB/sled 语义为空结果
        if start > end {
            return Ok(Vec::new());
        }
        let range = self
            .map
            .range((Bound::Included(start), Bound::Excluded(end)));
        let iter: Box<dyn Iterator<Item = (&String, &String)>> = if options.reverse {
            Box::new(range.rev())
        } else {
            Box::new(range)
        };
        let items = iter
            .take(options.limit.unwrap_or(usize::MAX))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::contract::check_storage_contract;

    #[test]
    fn memory_passes_storage_contract() {
        let mut s = MemoryStorage::new();
        check_storage_contract(&mut s);
    }

    #[test]
    fn get_put_delete_roundtrip() {
        let mut s = MemoryStorage::new();
        assert_eq!(s.get("k").unwrap(), None);
        s.put("k", "v").unwrap();
        assert_eq!(s.get("k").unwrap(), Some("v".to_string()));
        s.put("k", "v2").unwrap();
        assert_eq!(s.get("k").unwrap(), Some("v2".to_string()));
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        // 删除不存在的键不报错
        s.delete("k").unwrap();
    }

    #[test]
    fn batch_applies_in_order() {
        let mut s = MemoryStorage::new();
        s.batch(vec![
            BatchOperation::put("a", "1"),
            BatchOperation::put("b", "2"),
            BatchOperation::delete("a"),
        ])
        .unwrap();
        assert_eq!(s.get("a").unwrap(), None);
        assert_eq!(s.get("b").unwrap(), Some("2".to_string()));
    }

    #[test]
    fn scan_prefix_default_bounds() {
        let mut s = MemoryStorage::new();
        for (k, v) in [
            ("doc:a:1", "x"),
            ("doc:a:2", "y"),
            ("doc:b:1", "z"),
            ("meta:a:1", "m"),
        ] {
            s.put(k, v).unwrap();
        }
        let items = s.scan(&ScanOptions::prefix("doc:a:")).unwrap();
        assert_eq!(
            items,
            vec![
                ("doc:a:1".to_string(), "x".to_string()),
                ("doc:a:2".to_string(), "y".to_string())
            ]
        );
    }

    #[test]
    fn scan_upper_bound_covers_non_ascii_keys() {
        // U+10FFFF 上界必须覆盖中文等首字节 > 0xC3 的 key（TS `\xFF` 上界的历史坑）
        let mut s = MemoryStorage::new();
        s.put("doc:a:english", "1").unwrap();
        s.put("doc:a:中文", "2").unwrap();
        s.put("doc:a:😀", "3").unwrap();
        let items = s.scan(&ScanOptions::prefix("doc:a:")).unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn scan_start_inclusive_end_exclusive() {
        let mut s = MemoryStorage::new();
        for i in 0..5 {
            s.put(&format!("k{i}"), "v").unwrap();
        }
        let items = s
            .scan(&ScanOptions {
                prefix: "k".to_string(),
                start: Some("k1".to_string()),
                end: Some("k4".to_string()),
                limit: None,
                reverse: false,
            })
            .unwrap();
        assert_eq!(
            items.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["k1", "k2", "k3"]
        );
    }

    #[test]
    fn scan_limit_and_reverse() {
        let mut s = MemoryStorage::new();
        for i in 0..5 {
            s.put(&format!("k{i}"), "v").unwrap();
        }
        let items = s
            .scan(&ScanOptions {
                prefix: "k".to_string(),
                limit: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            items.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["k0", "k1"]
        );
        // reverse + limit：取最后 N 条并降序返回（对齐 LevelDB iterator）
        let items = s
            .scan(&ScanOptions {
                prefix: "k".to_string(),
                limit: Some(2),
                reverse: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            items.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["k4", "k3"]
        );
    }

    #[test]
    fn prefix_range_end_uses_u10ffff() {
        let end = prefix_range_end("doc:");
        assert_eq!(end, "doc:\u{10FFFF}");
        assert_eq!(end.as_bytes(), &[b'd', b'o', b'c', b':', 0xF4, 0x8F, 0xBF, 0xBF]);
    }
}
