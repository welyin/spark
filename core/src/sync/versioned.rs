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

    /// key 是否受管（参与同步记账）。
    ///
    /// 个人域受管前缀：pdsync category 覆盖的 key（ct:*/msg:conv:personal:* 等）。
    /// org 域受管前缀：`orgd:{orgId}:`——orgsync 受管写，经 VersionedStorage
    /// 写入即自动版本化/墓碑/删除日志（org 域 dlog）。
    /// `org:coll:` 声明记录作为**保留 all-members 系统集合**纳管（B5）：声明
    /// 先行进 orgsync 流量，全员同步（accounts 恒 AllMembers，无需查声明）。
    ///
    /// `msg:conv` 需细分（category 前缀 `msg:conv:` 过宽）：
    /// - 仅个人空间会话受管（`msg:conv:personal:*`）——组织会话走 org-pull；
    /// - 应用会话（`msg:conv:personal:app:*`）不受管——维持现状不参与
    ///   pdsync（其删除传播是另行记录的已知缺口）。
    fn managed(key: &str) -> bool {
        // org 域受管：orgd:{orgId}: 数据 + org:coll: 声明（保留系统集合）+
        // org:acl:{orgId}: 授权名单（O4，all-members 系统数据，进 orgsync 流量）
        if key.starts_with("orgd:")
            || key.starts_with("org:coll:")
            || key.starts_with("org:acl:")
            || key.starts_with("org:invpub:")
            || key.starts_with("org:member:")
        {
            return true;
        }
        if let Some(rest) = key.strip_prefix("msg:conv:") {
            return rest.starts_with("personal:") && !rest.starts_with("personal:app:");
        }
        category_for_key(key).is_some()
    }

    fn touch(&self, ts: i64) {
        self.last_local_write_ms.store(ts, Ordering::Relaxed);
    }

    /// key 是否为 org 域受管（orgd: 数据 / org:coll: 声明 / 内建 all-members
    /// 集合的存量组织键）。
    ///
    /// O2b：存量组织键（org:meta/ct:org）纳管为内建 all-members 集合，其删除
    /// 须落 **org 域 dlog**（作用域 = 所属内建集合），墓碑可经 orgsync 中继
    /// ——不再只落个人域 dlog。F7：org:inv:* 退出 orgsync（`legacy_org_key_scope`
    /// 不再命中）——删除回落为只登个人域 dlog（随 pdsync 自设备传播）。
    fn is_org_key(key: &str) -> bool {
        key.starts_with("orgd:")
            || key.starts_with("org:coll:")
            || key.starts_with("org:acl:")
            || key.starts_with("org:invpub:")
            || key.starts_with("org:member:")
            || crate::sync::orgsync::legacy_org_key_scope(key).is_some()
    }

    /// 解析 org 域 key 的作用域 (orgId, name, version)：
    /// - `orgd:{orgId}:{name}@v{version}:{key}` → parse_org_data_key；
    /// - `org:coll:{orgId}:{name}@v{version}` → 声明记录（保留系统集合）；
    /// - 存量组织键 → 所属内建集合作用域（O2b）。
    fn org_scope_of(key: &str) -> Option<(String, String, String)> {
        if key.starts_with("orgd:") {
            return crate::plugindata::parse_org_data_key(key);
        }
        if key.starts_with("org:member:") {
            // org:member:{orgId}:{rootId} → org:structure@v1（成员条目归属
            // 结构集合；成员移除墓碑经 org 域 dlog 传播，P1）
            let org_id = key.strip_prefix("org:member:")?.split(':').next()?;
            return Some((org_id.to_string(), "org:structure".to_string(), "1".to_string()));
        }
        if let Some(rest) = key.strip_prefix("org:invpub:") {
            // org:invpub:{orgId}:{inviterRoot}:{inviteeRoot} → org:invitations@v1
            let org_id = rest.split(':').next()?;
            return Some((org_id.to_string(), "org:invitations".to_string(), "1".to_string()));
        }
        if key.starts_with("org:coll:") || key.starts_with("org:acl:") {
            // org:coll:{orgId}:{name}@v{version} / org:acl:{orgId}:{name}@v{version}
            let rest = key
                .strip_prefix("org:coll:")
                .unwrap_or(key)
                .strip_prefix("org:acl:")?;
            let (org_id, rest) = rest.split_once(':')?;
            let at = rest.rfind("@v")?;
            let name = &rest[..at];
            let version = &rest[at + 2..];
            return Some((org_id.to_string(), name.to_string(), version.to_string()));
        }
        crate::sync::orgsync::legacy_org_key_scope(key)
    }

    /// 本地写入的版本化 bump：vv 本机分量取 **per-node 单调序号**
    /// （`vv_seq` 分配器，batch 首条受管写前惰性初始化为存量序号），ts=now、
    /// 清墓碑。返回 (ts, meta)。
    fn bump_local(
        &self,
        key: &str,
        node_id: &str,
        pmeta_cache: &mut HashMap<String, DocMeta>,
        vv_seq: &mut i64,
    ) -> Result<(i64, DocMeta)> {
        let mut meta = match pmeta_cache.get(key) {
            Some(m) => m.clone(),
            None => get_personal_meta(&self.inner, key)
                .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?
                .unwrap_or_default(),
        };
        *vv_seq += 1;
        meta.vv.insert(node_id.to_string(), *vv_seq);
        let ts = now_ms();
        meta.ts = ts;
        meta.node_id = Some(node_id.to_string());
        meta.tombstone = None;
        pmeta_cache.insert(key.to_string(), meta.clone());
        Ok((ts, meta))
    }

    /// 本地删除的墓碑化：vv[node]+1、ts=now、tombstone=true + 删除日志条目。
    ///
    /// dlog 路由：
    /// - `orgd:`/`org:coll:`（orgsync 专属）→ 只写 **org 域 dlog**
    ///   （`dlog:org:{orgId}:{name}@v{version}`，A→B→C 接力传播）；
    /// - 存量组织键（`org:meta`/`ct:org`，O2b 内建集合）→ **同时写
    ///   org 域 dlog 与个人域 dlog**——orgsync 成员间反熵走 org dlog，pdsync
    ///   自设备同步照旧走个人 dlog（"键不搬家、pdsync 不动"）；墓碑两路中继。
    ///   （F7：`org:inv:*` 已退出 orgsync，不在此列——只登个人域 dlog）；
    /// - 其余个人域 key → 只写个人域 dlog。
    fn tombstone_local(
        &self,
        key: &str,
        node_id: &str,
        pmeta_cache: &mut HashMap<String, DocMeta>,
        vv_seq: &mut i64,
    ) -> Result<(i64, DocMeta, Vec<BatchOperation>)> {
        let (ts, mut meta) = self.bump_local(key, node_id, pmeta_cache, vv_seq)?;
        meta.tombstone = Some(true);
        pmeta_cache.insert(key.to_string(), meta.clone());
        let mut ops = Vec::new();
        // orgsync 侧 dlog：orgd/org:coll（orgsync 专属）或存量组织键（内建集合）
        if Self::is_org_key(key)
            && let Some((org_id, name, version)) = Self::org_scope_of(key)
        {
            let (_seq, dlog_ops) = crate::sync::orgsync::org_dlog_append_ops(
                &self.inner,
                &org_id,
                &name,
                &version,
                key,
            )
            .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
            ops.extend(dlog_ops);
        }
        // 个人域 dlog：非 orgsync 专属 key（orgd:/org:coll: 之外都写——含
        // 存量组织键，保 pdsync 自设备同步不动）
        if !key.starts_with("orgd:") && !key.starts_with("org:coll:") {
            let (_seq, dlog_ops) = crate::sync::dlog::append_ops(&self.inner, key)
                .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
            ops.extend(dlog_ops);
        }
        Ok((ts, meta, ops))
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
        // per-node 单调序号分配器：首条受管写前惰性读取存量序号
        // （sync::personal::current_vv_seq，含升级种子逻辑），batch 内逐条
        // +1，batch 末尾随序号键持久化（与记录同一原子提交）。
        let node_id = self.node_id();
        let mut vv_seq: Option<i64> = None;
        for op in operations {
            match op {
                BatchOperation::Put { key, value } if Self::managed(&key) => {
                    if vv_seq.is_none() {
                        vv_seq = Some(
                            crate::sync::personal::current_vv_seq(&self.inner, &node_id).map_err(
                                |e| crate::storage::StorageError::Backend(e.to_string()),
                            )?,
                        );
                    }
                    let (ts, meta) = self.bump_local(
                        &key,
                        &node_id,
                        &mut pmeta_cache,
                        vv_seq.as_mut().expect("vv_seq initialized"),
                    )?;
                    let meta_raw = serde_json::to_string(&meta)
                        .map_err(|e| crate::storage::StorageError::Backend(e.to_string()))?;
                    out.push(BatchOperation::put(personal_meta_key(&key), meta_raw));
                    out.push(BatchOperation::put(key, value));
                    touched_ts = Some(ts);
                }
                BatchOperation::Delete { key } if Self::managed(&key) => {
                    if vv_seq.is_none() {
                        vv_seq = Some(
                            crate::sync::personal::current_vv_seq(&self.inner, &node_id).map_err(
                                |e| crate::storage::StorageError::Backend(e.to_string()),
                            )?,
                        );
                    }
                    let (ts, meta, dlog_ops) = self.tombstone_local(
                        &key,
                        &node_id,
                        &mut pmeta_cache,
                        vv_seq.as_mut().expect("vv_seq initialized"),
                    )?;
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
        if touched_ts.is_some()
            && let Some(final_seq) = vv_seq
        {
            out.push(crate::sync::personal::vv_seq_batch_op(&node_id, final_seq));
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
        assert!(
            get_personal_meta(s.raw(), "msg:conv:personal:root-x")
                .unwrap()
                .is_some()
        );
        // 组织会话：不受管（走 org-pull）
        s.put("msg:conv:org:o1:root-x", "{}").unwrap();
        // 应用会话：不受管（现状不参与 pdsync）
        s.put("msg:conv:personal:app:ai-chat", "{}").unwrap();
        assert!(
            get_personal_meta(s.raw(), "msg:conv:org:o1:root-x")
                .unwrap()
                .is_none()
        );
        assert!(
            get_personal_meta(s.raw(), "msg:conv:personal:app:ai-chat")
                .unwrap()
                .is_none()
        );
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

    /// O6：orgkey（personal 域，pdsync category `orgkey:`）经版本化句柄写入
    /// 即自动 pmeta 记账——自设备经 pdsync 才真实扩散。若绕过版本化（raw），
    /// orgkey 无 pmeta，自设备收不到密钥。
    #[test]
    fn orgkey_write_via_versioned_handle_versions_for_pdsync() {
        let mut s = new_store();
        let key = crate::sync::orgsync::orgkey_key("org_01", "fin:pay", "1", 1);
        // 经版本化句柄写 orgkey → pmeta 记账（自设备 pdsync 扩散的载体）
        s.put(&key, "c2VjcmV0").unwrap();
        assert_eq!(s.get(&key).unwrap().as_deref(), Some("c2VjcmV0"));
        let meta = get_personal_meta(s.raw(), &key).unwrap().unwrap();
        assert_eq!(
            meta.vv.get("node-a"),
            Some(&1),
            "orgkey 写经版本化自动 pmeta"
        );
        // 对照：经 raw 写则无 pmeta（自设备不可同步——O6 修复前即如此）
        let mut raw = s.raw().clone();
        raw.put(
            &crate::sync::orgsync::orgkey_key("org_01", "fin:pay", "1", 2),
            "x",
        )
        .unwrap();
        assert!(
            get_personal_meta(
                s.raw(),
                &crate::sync::orgsync::orgkey_key("org_01", "fin:pay", "1", 2)
            )
            .unwrap()
            .is_none(),
            "raw 写不记账（对照）"
        );
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

    /// O2b 工作项 2：存量组织键（内建 all-members 集合）删除须落 **org 域
    /// dlog**（作用域 = 所属内建集合，墓碑可经 orgsync 中继）——同时保留
    /// 个人域 dlog（pdsync 自设备同步不动，"键不搬家"）。
    #[test]
    fn delete_legacy_org_key_writes_org_and_personal_dlog() {
        let mut s = new_store();
        let key = "ct:org:org_01:member-x";
        s.put(key, "\"v1\"").unwrap();
        s.delete(key).unwrap();
        assert!(s.get(key).unwrap().is_none());
        // 个人域 dlog（pdsync 自设备同步照旧）
        assert_eq!(
            crate::sync::dlog::entries_after(s.raw(), 0).unwrap(),
            vec![(1, key.to_string())]
        );
        // org 域 dlog：作用域 (org_01, org:contacts, 1)
        let org_entries =
            crate::sync::orgsync::org_dlog_entries_after(s.raw(), "org_01", "org:contacts", "1", 0)
                .unwrap();
        assert_eq!(org_entries.len(), 1, "存量组织键删除落 org dlog");
        assert_eq!(org_entries[0].1, key);
    }

    /// O2b：org:meta（org:structure 单记录 whole）删除同样落 org dlog。
    #[test]
    fn delete_org_meta_key_writes_org_dlog() {
        let mut s = new_store();
        let key = "org:meta:org_01";
        s.put(key, "{\"name\":\"t\"}").unwrap();
        s.delete(key).unwrap();
        let org_entries = crate::sync::orgsync::org_dlog_entries_after(
            s.raw(),
            "org_01",
            "org:structure",
            "1",
            0,
        )
        .unwrap();
        assert_eq!(org_entries.len(), 1);
        assert_eq!(org_entries[0].1, key);
        // 个人域 dlog 也保留（pdsync 自设备同步不动）
        assert_eq!(
            crate::sync::dlog::entries_after(s.raw(), 0).unwrap().len(),
            1
        );
    }
}
