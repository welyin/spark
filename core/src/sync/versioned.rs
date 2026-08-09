//! 版本化存储中间件：把个人域同步的"记账"从调用点纪律变为存储层保证。
//!
//! 背景（§11.5 架构收敛）：同步记账（bump vv / pmeta / 墓碑 / 删除日志）
//! 此前要求每个写入点显式调用 `put_personal`/`delete_personal`——漏一处
//! 调用，该类数据就静默失去同步能力（组织身份不同步即此类缺陷）。本模块
//! 把记账下沉到 `StorageBackend` 包装层：
//!
//! - `put`/`delete`/`batch` 命中**受管前缀**（[`crate::sync::pdsync::category_for_key`]
//!   返回 Some 的 key）时自动完成版本化（vv bump + pmeta），delete 额外写
//!   墓碑 + 删除日志条目——与业务写入同一 batch 提交；
//! - 非受管 key（`p2p:*` 私钥、`pmeta:*`、`dlog:*`、`msg:item:*` 等）原样
//!   透传，绝不版本化；
//! - **远端合入必须使用 [`Self::raw`] 句柄**：`apply_personal_remote` 写的是
//!   对端版本的数据，经中间件会被错误地二次 bump（回声污染 vv）；
//! - **逃生舱**：少数不应推高 pmeta 的写（消息追加驱动的 conv 记录更新等）
//!   同样经 `raw()` 显式绕过。
//!
//! node_id 来源：共享格 [`SharedNodeId`]，kernel 在 open_storage / p2p 启动
//! （peerId）/ 停止（回退持久化派生 id）时更新——与 `sync_node_id()` 同语义。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use crate::storage::{BatchOperation, Result, ScanOptions, StorageBackend};
use crate::sync::meta::DocMeta;
use crate::sync::pdsync::category_for_key;
use crate::sync::personal::{get_personal_meta, personal_meta_key};

/// node_id 共享格（p2p 运行态 = peerId；离线 = 持久化身份派生 id）。
pub type SharedNodeId = Arc<Mutex<String>>;

/// 以系统当前时间（ms）初始化共享格。
pub fn shared_node_id(initial: impl Into<String>) -> SharedNodeId {
    Arc::new(Mutex::new(initial.into()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 版本化存储包装：受管前缀的本地写入自动完成同步记账。
///
/// Clone 廉价（内部均 Arc/句柄）；所有克隆共享同一 node_id 格与变更信号。
#[derive(Clone)]
pub struct VersionedStorage<S> {
    inner: S,
    node_id: SharedNodeId,
    /// 最近一次受管本地写入的时间戳（ms；0=无）——变更流触发（hello 即时
    /// 补发）的信号源，消费方在 org-sync 层。
    last_local_write_ms: Arc<AtomicI64>,
}

impl<S: StorageBackend> VersionedStorage<S> {
    /// 包装一个存储后端。
    pub fn new(inner: S, node_id: SharedNodeId) -> Self {
        Self {
            inner,
            node_id,
            last_local_write_ms: Arc::new(AtomicI64::new(0)),
        }
    }

    /// 原始句柄（远端合入 / 逃生舱写入专用——不触发任何同步记账）。
    pub fn raw(&self) -> &S {
        &self.inner
    }

    /// 原始可变句柄（同上，供需要 `&mut S` 的服务签名使用）。
    pub fn raw_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// node_id 共享格（kernel 在 p2p 启停时更新）。
    pub fn node_id_handle(&self) -> SharedNodeId {
        Arc::clone(&self.node_id)
    }

    /// 最近一次受管本地写入时间（ms；供变更流触发消费）。
    pub fn last_local_write_ms(&self) -> i64 {
        self.last_local_write_ms.load(Ordering::Relaxed)
    }

    /// 读取当前 node_id。
    fn node_id(&self) -> String {
        self.node_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// key 是否受管（参与个人域同步记账）。
    ///
    /// `msg:conv` 需细分（category 前缀 `msg:conv:` 过宽）：
    /// - 仅个人空间会话受管（`msg:conv:personal:*`）——组织会话走 org-pull；
    /// - 应用会话（`msg:conv:personal:app:*`）不受管——维持现状不参与
    ///   pdsync（其删除传播是另行记录的已知缺口）。
    fn managed(key: &str) -> bool {
        if let Some(rest) = key.strip_prefix("msg:conv:") {
            return rest.starts_with("personal:") && !rest.starts_with("personal:app:");
        }
        category_for_key(key).is_some()
    }

    fn touch(&self, ts: i64) {
        self.last_local_write_ms.store(ts, Ordering::Relaxed);
    }

    /// 本地写入的版本化 bump：vv[node]+1、ts=now、清墓碑。返回 (ts, meta)。
    fn bump_local(&self, key: &str, pmeta_cache: &mut HashMap<String, DocMeta>) -> Result<(i64, DocMeta)> {
        let mut meta = match pmeta_cache.get(key) {
            Some(m) => m.clone(),
            None => get_personal_meta(&self.inner, key)
                .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?
                .unwrap_or_default(),
        };
        let node_id = self.node_id();
        *meta.vv.entry(node_id.clone()).or_insert(0) += 1;
        let ts = now_ms();
        meta.ts = ts;
        meta.node_id = Some(node_id);
        meta.tombstone = None;
        pmeta_cache.insert(key.to_string(), meta.clone());
        Ok((ts, meta))
    }

    /// 本地删除的墓碑化：vv[node]+1、ts=now、tombstone=true + 删除日志条目。
    fn tombstone_local(
        &self,
        key: &str,
        pmeta_cache: &mut HashMap<String, DocMeta>,
    ) -> Result<(i64, DocMeta, Vec<BatchOperation>)> {
        let (ts, mut meta) = self.bump_local(key, pmeta_cache)?;
        meta.tombstone = Some(true);
        pmeta_cache.insert(key.to_string(), meta.clone());
        let (_seq, dlog_ops) = crate::sync::dlog::append_ops(&self.inner, key)
            .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
        Ok((ts, meta, dlog_ops))
    }
}

impl<S: StorageBackend> StorageBackend for VersionedStorage<S> {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.inner.get(key)
    }

    fn put(&mut self, key: &str, value: &str) -> Result<()> {
        self.batch(vec![BatchOperation::put(key, value)])
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        self.batch(vec![BatchOperation::delete(key)])
    }

    fn batch(&mut self, operations: Vec<BatchOperation>) -> Result<()> {
        let mut out = Vec::with_capacity(operations.len() * 2);
        let mut pmeta_cache: HashMap<String, DocMeta> = HashMap::new();
        let mut touched_ts: Option<i64> = None;
        for op in operations {
            match op {
                BatchOperation::Put { key, value } if Self::managed(&key) => {
                    let (ts, meta) = self.bump_local(&key, &mut pmeta_cache)?;
                    let meta_raw = serde_json::to_string(&meta)
                        .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
                    out.push(BatchOperation::put(personal_meta_key(&key), meta_raw));
                    out.push(BatchOperation::put(key, value));
                    touched_ts = Some(ts);
                }
                BatchOperation::Delete { key } if Self::managed(&key) => {
                    let (ts, meta, dlog_ops) = self.tombstone_local(&key, &mut pmeta_cache)?;
                    let meta_raw = serde_json::to_string(&meta)
                        .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
                    out.push(BatchOperation::delete(key.clone()));
                    out.push(BatchOperation::put(personal_meta_key(&key), meta_raw));
                    out.extend(dlog_ops);
                    touched_ts = Some(ts);
                }
                other => out.push(other),
            }
        }
        self.inner.batch(out)?;
        if let Some(ts) = touched_ts {
            self.touch(ts);
        }
        Ok(())
    }

    fn scan(&self, options: &ScanOptions) -> Result<Vec<(String, String)>> {
        self.inner.scan(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::dlog::entries_after;
    use crate::sync::is_tombstone;

    fn new_store() -> VersionedStorage<MemoryStorage> {
        VersionedStorage::new(MemoryStorage::new(), shared_node_id("node-a"))
    }

    #[test]
    fn put_managed_key_auto_versions() {
        let mut s = new_store();
        s.put("ct:friend:x", r#""v1""#).unwrap();
        assert_eq!(s.get("ct:friend:x").unwrap().as_deref(), Some(r#""v1""#));
        let meta = get_personal_meta(s.raw(), "ct:friend:x").unwrap().unwrap();
        assert_eq!(meta.vv.get("node-a"), Some(&1));
        assert!(!is_tombstone(&meta));
        assert!(s.last_local_write_ms() > 0);

        // 再写一次：vv 递增
        s.put("ct:friend:x", r#""v2""#).unwrap();
        let meta = get_personal_meta(s.raw(), "ct:friend:x").unwrap().unwrap();
        assert_eq!(meta.vv.get("node-a"), Some(&2));
    }

    #[test]
    fn delete_managed_key_auto_tombstones_and_journals() {
        let mut s = new_store();
        s.put("ct:friend:x", r#""v""#).unwrap();
        s.delete("ct:friend:x").unwrap();

        assert!(s.get("ct:friend:x").unwrap().is_none(), "本体已删");
        let meta = get_personal_meta(s.raw(), "ct:friend:x").unwrap().unwrap();
        assert!(is_tombstone(&meta));
        assert_eq!(meta.vv.get("node-a"), Some(&2));
        // 删除日志已追加（中间件保证，调用点零感知）
        assert_eq!(
            entries_after(s.raw(), 0).unwrap(),
            vec![(1, "ct:friend:x".to_string())]
        );
    }

    #[test]
    fn unmanaged_keys_pass_through_unversioned() {
        let mut s = new_store();
        s.put("p2p:identity:privateKey", "secret").unwrap();
        s.put("pmeta:ct:friend:x", "{}").unwrap();
        s.put("msg:item:personal:c:1", "{}").unwrap();
        s.delete("p2p:identity:privateKey").unwrap();
        assert!(s.last_local_write_ms() == 0, "非受管写不触发变更信号");
    }

    #[test]
    fn conv_scope_boundaries() {
        let mut s = new_store();
        // 个人空间普通会话：受管
        s.put("msg:conv:personal:root-x", "{}").unwrap();
        assert!(get_personal_meta(s.raw(), "msg:conv:personal:root-x")
            .unwrap()
            .is_some());
        // 组织会话：不受管（走 org-pull）
        s.put("msg:conv:org:o1:root-x", "{}").unwrap();
        // 应用会话：不受管（现状不参与 pdsync）
        s.put("msg:conv:personal:app:ai-chat", "{}").unwrap();
        assert!(get_personal_meta(s.raw(), "msg:conv:org:o1:root-x")
            .unwrap()
            .is_none());
        assert!(get_personal_meta(s.raw(), "msg:conv:personal:app:ai-chat")
            .unwrap()
            .is_none());
    }

    #[test]
    fn raw_handle_bypasses_versioning() {
        let s = new_store();
        // 远端合入路径：经 raw 写入不产生本地版本记账
        let mut raw = s.raw().clone();
        raw.put("ct:friend:x", r#""remote""#).unwrap();
        assert!(get_personal_meta(s.raw(), "ct:friend:x").unwrap().is_none());
        assert!(s.last_local_write_ms() == 0);
    }

    #[test]
    fn batch_expands_mixed_operations_in_order() {
        let mut s = new_store();
        s.batch(vec![
            BatchOperation::put("ct:friend:a", "1"),
            BatchOperation::put("local:pref", "x"),
            BatchOperation::delete("ct:friend:a"),
        ])
        .unwrap();
        assert!(s.get("ct:friend:a").unwrap().is_none());
        let meta = get_personal_meta(s.raw(), "ct:friend:a").unwrap().unwrap();
        assert!(is_tombstone(&meta));
        // 同 batch 内 put→delete：缓存连续 bump，vv=2（不是两次都从旧值 bump 成 1）
        assert_eq!(meta.vv.get("node-a"), Some(&2));
        assert_eq!(s.get("local:pref").unwrap().as_deref(), Some("x"));
    }

    #[test]
    fn node_id_switch_takes_effect() {
        let mut s = new_store();
        let handle = s.node_id_handle();
        s.put("ct:friend:x", "1").unwrap();
        *handle.lock().unwrap() = "node-b".to_string();
        s.put("ct:friend:x", "2").unwrap();
        let meta = get_personal_meta(s.raw(), "ct:friend:x").unwrap().unwrap();
        assert_eq!(meta.vv.get("node-a"), Some(&1));
        assert_eq!(meta.vv.get("node-b"), Some(&1));
    }
}
