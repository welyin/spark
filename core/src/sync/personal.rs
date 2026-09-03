//! 个人域同步存储层。
//!
//! 每个个人域记录在 `pmeta:{recordKey}` 中保存一份独立的 [`DocMeta`]（含
//! 单记录版本向量），写入通过 [`put_personal`] 自动 bump 本设备 nodeId 计数、
//! 落记录 + 落 pmeta。远端合入通过 [`apply_personal_remote`] 比较向量 → 并发时
//! LWW（ts + nodeId 字典序兜底）裁决。
//!
//! 与组织域 `meta.rs` 复用同一套 [`DocMeta`] / [`VersionVector`] /
//! [`compare_version_vectors`] 原语，只是作用域不同（记录级 vv 而非
//! collection 级 vv）。

use crate::storage::{BatchOperation, ScanOptions, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::{CompareResult, DocMeta, compare_version_vectors};

/// 节点写入序号键（`p2p:` 前缀不进 pdsync 流量）：vv 计数采用 **per-node
/// 单调序号**（该节点个人域第 N 次写），而非 per-key 计数。per-key 计数下
/// 同 category 第二条记录与第一条同值 {node:1}，折叠 max 后「对端已见第 1
/// 次写」推不出「见了哪一条」——已有同值记录的 peer 判 Equal，
/// `collect_incremental` 跳过，第二条记录**永久不可见**（同构于 §5.6 墓碑
/// 缺陷：折叠丢失 key 维度）。per-node 序号让线性写历史下折叠摘要正确表达
/// 「该节点已写到第 N 条」。
fn vv_seq_key(node_id: &str) -> String {
    format!("p2p:vvseq:{node_id}")
}

/// 序号持久化 op——调用方必须并入与记录写入**同一 batch**（原子）。
pub fn vv_seq_batch_op(node_id: &str, seq: i64) -> BatchOperation {
    BatchOperation::put(vv_seq_key(node_id), seq.to_string())
}

/// 读取节点当前写入序号。首读缺省时以「扫描全部 pmeta 取该节点分量存量
/// 最大值」为种子（一次性）——升级前 per-key 计数存量与序号同源单调，
/// 避免升级后新写序号从 1 起被存量记录「已覆盖」误判（存量记录值 ≤ 种子，
/// 新写恒大于种子，折叠/增量语义平滑衔接）。
pub fn current_vv_seq<S: StorageBackend>(storage: &S, node_id: &str) -> SyncResult<i64> {
    if let Some(raw) = storage.get(&vv_seq_key(node_id))? {
        if let Ok(v) = raw.parse::<i64>() {
            return Ok(v);
        }
    }
    let mut max_seq = 0i64;
    for (_k, raw) in storage.scan(&ScanOptions {
        prefix: PMETA_PREFIX.to_string(),
        start: None,
        end: None,
        reverse: false,
        limit: None,
    })? {
        if let Ok(meta) = serde_json::from_str::<DocMeta>(&raw)
            && let Some(v) = meta.vv.get(node_id)
        {
            max_seq = max_seq.max(*v);
        }
    }
    Ok(max_seq)
}

/// pmeta 键前缀。
pub const PMETA_PREFIX: &str = "pmeta:";

/// 根据记录 key 生成 pmeta key。
pub fn personal_meta_key(record_key: &str) -> String {
    format!("{PMETA_PREFIX}{record_key}")
}

/// [`apply_personal_remote`] 的合入结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyResult {
    /// 远端胜出，已落地。
    Applied,
    /// 本地胜出，未写入。
    LocalWins,
    /// 版本相等，未写入。
    Equal,
}

impl ApplyResult {
    /// TS 字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::LocalWins => "local",
            Self::Equal => "equal",
        }
    }

    /// 是否实际落地。
    pub fn did_apply(self) -> bool {
        matches!(self, Self::Applied)
    }
}

// ── 读写 ───────────────────────────────────────────────────────────

/// 读取个人域的 pmeta；缺失或损坏返回 `Ok(None)`。
pub fn get_personal_meta<S: StorageBackend>(
    storage: &S,
    record_key: &str,
) -> SyncResult<Option<DocMeta>> {
    let meta_key = personal_meta_key(record_key);
    let Some(raw) = storage.get(&meta_key)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// 写入个人域 pmeta。
pub fn set_personal_meta<S: StorageBackend>(
    storage: &mut S,
    record_key: &str,
    meta: &DocMeta,
) -> SyncResult<()> {
    storage.put(
        &personal_meta_key(record_key),
        &serde_json::to_string(meta)?,
    )?;
    Ok(())
}

/// 仅 bump 版本向量 + 写 pmeta（不落记录本体——调用方已自行写入）。
///
/// `ts_ms` 是 pmeta.ts 的语义时间，由调用方按记录类型选择：真实内容编辑
/// 传当前时间；消息追加等附属 bump 传记录的元数据时间（如 conv 的
/// `meta_updated_at`）——高频消息流不得推高 pmeta.ts，否则并发裁决时纯
/// 收发消息的设备仅凭 ts 胜出，吃掉对端真实的元数据编辑。
///
/// vv 分量取本机 **per-node 单调序号**（见 [`current_vv_seq`]），同 batch
/// 落序号键。
///
/// 返回更新后的 [`DocMeta`]。
pub fn bump_personal_meta<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    record_key: &str,
    ts_ms: i64,
) -> SyncResult<DocMeta> {
    let mut meta = get_personal_meta(storage, record_key)?.unwrap_or_default();
    let seq = current_vv_seq(storage, node_id)? + 1;
    meta.vv.insert(node_id.to_string(), seq);
    meta.ts = ts_ms;
    meta.node_id = Some(node_id.to_string());
    meta.tombstone = None;
    storage.batch(vec![
        BatchOperation::put(personal_meta_key(record_key), serde_json::to_string(&meta)?),
        vv_seq_batch_op(node_id, seq),
    ])?;
    Ok(meta)
}

// ── 写入入口 ────────────────────────────────────────────────────────

/// 写入个人域记录：落记录 + bump 本节点 vv + 落 pmeta（同一 batch 提交，
/// §4.2）。
///
/// 返回更新后的 [`DocMeta`]（调用方可缓存用于后续远端比较）。
pub fn put_personal<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    record_key: &str,
    value: &str,
    now_ms: i64,
) -> SyncResult<DocMeta> {
    let mut meta = get_personal_meta(storage, record_key)?.unwrap_or_default();
    let seq = current_vv_seq(storage, node_id)? + 1;
    meta.vv.insert(node_id.to_string(), seq);
    meta.ts = now_ms;
    meta.node_id = Some(node_id.to_string());
    meta.tombstone = None;

    storage.batch(vec![
        BatchOperation::put(record_key, value),
        BatchOperation::put(personal_meta_key(record_key), serde_json::to_string(&meta)?),
        vv_seq_batch_op(node_id, seq),
    ])?;

    Ok(meta)
}

// ── 快照合入（对端记账）──────────────────────────────────────────────

/// 旧通道（contact-sync / conv-sync / device-sync）快照合入专用入口。
///
/// 语义等价于"对**远端**节点 bump 的 [`put_personal`]"：记录落库、pmeta 账记到
/// `remote_node_id`（而非本机）。关键效果：
///
/// - 本机分量不被推进 → `local_personal_write_digest` 不变 → 不触发 steady hello →
///   消除"合入 → 发 hello → 对端 diff → 回 data → 再合入"的双向回环风暴
///   （见 wiki/architecture/sync/sync-storm-fix.md）。
/// - 记录仍带 pmeta、对端分量折叠进 category vv → 数据对 pdsync 依旧可见（P1 意图保留）。
///
/// **防线**：签名显式要求 `remote_node_id != local_node_id`。若调用方误把本机
/// nodeId 当记账对象（旧 bug 的根源），此处不静默接受——记录仍然落地（保持
/// 可用），但**始终打 WARN 日志**暴露误用，便于观测风暴前兆。
///
/// 调用方（入站 handler）应传 `ctx.remote_peer_id`（对端设备 p2p peerId）为
/// `remote_node_id`、`ctx.node_id`（本机设备 peerId）为 `local_node_id`。
pub fn apply_snapshot_remote<S: StorageBackend>(
    storage: &mut S,
    remote_node_id: &str,
    local_node_id: &str,
    record_key: &str,
    value: &str,
    now_ms: i64,
) -> SyncResult<DocMeta> {
    if remote_node_id == local_node_id {
        log::warn!(
            "[SNAPSHOT-ACCOUNTING] 快照合入把记账对象误为本机 node_id={local_node_id}, \
             key={record_key}; 本机分量将被推进、可能触发同步回环风暴。\
             应传对端设备 peerId 记账（contact/conv/device-sync 入站合入）",
        );
    }

    let mut meta = get_personal_meta(storage, record_key)?.unwrap_or_default();
    let seq = current_vv_seq(storage, remote_node_id)? + 1;
    meta.vv.insert(remote_node_id.to_string(), seq);
    meta.ts = now_ms;
    meta.node_id = Some(remote_node_id.to_string());
    meta.tombstone = None;

    storage.batch(vec![
        BatchOperation::put(record_key, value),
        BatchOperation::put(personal_meta_key(record_key), serde_json::to_string(&meta)?),
        vv_seq_batch_op(remote_node_id, seq),
    ])?;

    Ok(meta)
}

/// [`apply_snapshot_remote`] 的删除（墓碑）变体：对**远端**节点记账地删除记录。
///
/// 语义等价于"对远端节点 bump 的 [`delete_personal`]"：本机分量不被推进（不触发
/// steady hello → 不回环），记录以 tombstone pmeta 形式保留供后续传播。同时追加
/// 删除日志（与 [`delete_personal`] 同口径，墓碑按对端 ACK 游标驱动传播）。
///
/// 防线同 [`apply_snapshot_remote`]：`remote_node_id != local_node_id`，误用本机
/// 记账时记录仍落地（保持可用）但**始终打 WARN 日志**。
pub fn delete_snapshot_remote<S: StorageBackend>(
    storage: &mut S,
    remote_node_id: &str,
    local_node_id: &str,
    record_key: &str,
    now_ms: i64,
) -> SyncResult<DocMeta> {
    if remote_node_id == local_node_id {
        log::warn!(
            "[SNAPSHOT-ACCOUNTING] 快照合入把删除记账对象误为本机 node_id={local_node_id}, \
             key={record_key}; 本机分量将被推进、可能触发同步回环风暴",
        );
    }

    let mut meta = get_personal_meta(storage, record_key)?.unwrap_or_default();
    let seq = current_vv_seq(storage, remote_node_id)? + 1;
    meta.vv.insert(remote_node_id.to_string(), seq);
    meta.ts = now_ms;
    meta.node_id = Some(remote_node_id.to_string());
    meta.tombstone = Some(true);

    let (_seq, dlog_ops) = crate::sync::dlog::append_ops(storage, record_key)?;
    let mut ops = vec![
        BatchOperation::delete(record_key),
        BatchOperation::put(personal_meta_key(record_key), serde_json::to_string(&meta)?),
        vv_seq_batch_op(remote_node_id, seq),
    ];
    ops.extend(dlog_ops);
    storage.batch(ops)?;

    Ok(meta)
}

// ── 远端合入 ────────────────────────────────────────────────────────

/// 将远端个人域记录合入本地 sled。
///
/// 裁决顺序：
/// 1. 本地无 pmeta → 直接采纳远端。
/// 2. 版本向量比较（Equal / Local / Remote / Concurrent）：
///    - Remote → 直接采纳。
///    - Local / Equal → 不写。
///    - Concurrent → LWW（ts 大者胜；ts 相等时 nodeId 字典序大者胜，
///      保证所有设备收敛到同一结果）。
///
/// 远端胜出且 `meta.tombstone == true` 时：删除记录本体 + 落墓碑 pmeta
/// （同一 batch 提交），不把推送里的 `null` 值当记录本体落盘。
pub fn apply_personal_remote<S: StorageBackend>(
    storage: &mut S,
    record_key: &str,
    value: &str,
    remote_meta: &DocMeta,
) -> SyncResult<ApplyResult> {
    let local_meta = get_personal_meta(storage, record_key)?;

    let result = resolve_personal(local_meta.as_ref(), remote_meta);

    if matches!(result, ApplyResult::Applied) {
        let meta_raw = serde_json::to_string(remote_meta)?;
        if is_tombstone(remote_meta) {
            // 远端墓碑落地同时补登删除日志（接力传播：本机之后把它推给
            // 其他自设备，与本地删除同一条投递队列）
            let (_seq, dlog_ops) = crate::sync::dlog::append_ops(storage, record_key)?;
            let mut ops = vec![
                BatchOperation::delete(record_key),
                BatchOperation::put(personal_meta_key(record_key), meta_raw),
            ];
            ops.extend(dlog_ops);
            storage.batch(ops)?;
        } else {
            storage.batch(vec![
                BatchOperation::put(record_key, value),
                BatchOperation::put(personal_meta_key(record_key), meta_raw),
            ])?;
        }
    }

    Ok(result)
}

/// 远端合入（**不补登个人域 dlog**）：语义同 [`apply_personal_remote`]，但
/// 墓碑落地时只删本体 + 落 pmeta，**不追加 pdsync 删除日志**。
///
/// 供 org 域使用：orgsync 入站墓碑经本函数落地后由调用方补登**org 域 dlog**
/// （`dlog:org:{orgId}:{name}@v{version}`），避免把 org 记录混入个人域删除
/// 日志（B4 架构裁决：远端墓碑接力进正确的删除日志作用域）。
pub fn apply_personal_remote_no_dlog<S: StorageBackend>(
    storage: &mut S,
    record_key: &str,
    value: &str,
    remote_meta: &DocMeta,
) -> SyncResult<ApplyResult> {
    let local_meta = get_personal_meta(storage, record_key)?;
    let result = resolve_personal(local_meta.as_ref(), remote_meta);
    if matches!(result, ApplyResult::Applied) {
        let meta_raw = serde_json::to_string(remote_meta)?;
        if is_tombstone(remote_meta) {
            storage.batch(vec![
                BatchOperation::delete(record_key),
                BatchOperation::put(personal_meta_key(record_key), meta_raw),
            ])?;
        } else {
            storage.batch(vec![
                BatchOperation::put(record_key, value),
                BatchOperation::put(personal_meta_key(record_key), meta_raw),
            ])?;
        }
    }
    Ok(result)
}

/// 删除个人域记录（写 tombstone pmeta，保留 pmeta 供后续同步传播删除）。
///
/// 删除记录本体，pmeta 更新为 `{vv: bumped, ts, tombstone: true}`，并追加
/// 删除日志条目（同一 batch）——墓碑的推送由日志按对端 ACK 游标驱动，
/// 不再依赖折叠 vv 比大小（折叠丢失 key 维度，墓碑会被误判"已覆盖"）。
pub fn delete_personal<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    record_key: &str,
    now_ms: i64,
) -> SyncResult<DocMeta> {
    let mut meta = get_personal_meta(storage, record_key)?.unwrap_or_default();
    let seq = current_vv_seq(storage, node_id)? + 1;
    meta.vv.insert(node_id.to_string(), seq);
    meta.ts = now_ms;
    meta.node_id = Some(node_id.to_string());
    meta.tombstone = Some(true);

    let (_seq, dlog_ops) = crate::sync::dlog::append_ops(storage, record_key)?;
    let mut ops = vec![
        BatchOperation::delete(record_key),
        BatchOperation::put(personal_meta_key(record_key), serde_json::to_string(&meta)?),
        vv_seq_batch_op(node_id, seq),
    ];
    ops.extend(dlog_ops);
    storage.batch(ops)?;

    Ok(meta)
}

// ── 裁决逻辑 ────────────────────────────────────────────────────────

/// 比较本地与远端 pmeta，决定是否采纳远端。
fn resolve_personal(local: Option<&DocMeta>, remote: &DocMeta) -> ApplyResult {
    let Some(local) = local else {
        return ApplyResult::Applied;
    };

    let cmp = compare_version_vectors(Some(&local.vv), Some(&remote.vv));

    match cmp {
        CompareResult::Equal => ApplyResult::Equal,
        CompareResult::Local => ApplyResult::LocalWins,
        CompareResult::Remote => ApplyResult::Applied,
        CompareResult::Concurrent => {
            // LWW by ts
            if remote.ts > local.ts {
                return ApplyResult::Applied;
            }
            if remote.ts < local.ts {
                return ApplyResult::LocalWins;
            }
            // ts 相等：nodeId 字典序兜底，保证全局收敛
            let remote_nid = remote.node_id.as_deref().unwrap_or("");
            let local_nid = local.node_id.as_deref().unwrap_or("");
            if remote_nid > local_nid {
                ApplyResult::Applied
            } else {
                ApplyResult::LocalWins
            }
        }
    }
}

/// 判断墓碑：pmeta 存在且 `tombstone == true`。
pub fn is_tombstone(meta: &DocMeta) -> bool {
    meta.tombstone == Some(true)
}

// ── 测试 ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// 空的 MemoryStorage 包装（满足 StorageBackend trait）。
    fn storage() -> MemoryStorage {
        MemoryStorage::new()
    }

    const NODE_A: &str = "node-a";
    const NODE_B: &str = "node-b";
    const KEY: &str = "ct:friend:test";

    #[test]
    fn put_and_read_meta() {
        let mut s = storage();
        let meta = put_personal(&mut s, NODE_A, KEY, r#""hello""#, 1000).unwrap();
        assert_eq!(meta.vv.get(NODE_A), Some(&1));
        assert_eq!(meta.ts, 1000);
        assert_eq!(meta.node_id.as_deref(), Some(NODE_A));

        let read = get_personal_meta(&s, KEY).unwrap().unwrap();
        assert_eq!(read, meta);
    }

    #[test]
    fn put_bumps_counter() {
        let mut s = storage();
        let m1 = put_personal(&mut s, NODE_A, KEY, "1", 1000).unwrap();
        assert_eq!(m1.vv.get(NODE_A), Some(&1));

        let m2 = put_personal(&mut s, NODE_A, KEY, "2", 2000).unwrap();
        assert_eq!(m2.vv.get(NODE_A), Some(&2));
    }

    #[test]
    fn apply_remote_when_local_empty() {
        let mut s = storage();
        let remote_meta = DocMeta {
            vv: vec![(NODE_B.to_string(), 1)].into_iter().collect(),
            ts: 1000,
            node_id: Some(NODE_B.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &remote_meta).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(s.get(KEY).unwrap().unwrap(), r#""remote""#);
    }

    #[test]
    fn apply_remote_wins_by_vv() {
        let mut s = storage();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""local""#, 1000).unwrap();

        // Remote: same node, higher counter
        let remote_meta = DocMeta {
            vv: vec![(NODE_A.to_string(), 2)].into_iter().collect(),
            ts: 2000,
            node_id: Some(NODE_A.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &remote_meta).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(s.get(KEY).unwrap().unwrap(), r#""remote""#);
    }

    #[test]
    fn apply_local_wins_by_vv() {
        let mut s = storage();
        // Write twice so local counter is 2
        let _ = put_personal(&mut s, NODE_A, KEY, r#""v1""#, 1000).unwrap();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""local""#, 2000).unwrap();

        // Remote: same node, counter 1 < local 2
        let remote_meta = DocMeta {
            vv: vec![(NODE_A.to_string(), 1)].into_iter().collect(),
            ts: 1000,
            node_id: Some(NODE_A.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &remote_meta).unwrap();
        assert_eq!(r, ApplyResult::LocalWins);
        // Local unchanged
        assert!(s.get(KEY).unwrap().is_some());
    }

    #[test]
    fn apply_concurrent_lww_ts_wins() {
        let mut s = storage();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""local""#, 1000).unwrap();

        // Concurrent: different nodes
        let remote_meta = DocMeta {
            vv: vec![(NODE_B.to_string(), 1)].into_iter().collect(),
            ts: 3000,
            node_id: Some(NODE_B.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &remote_meta).unwrap();
        // ts 3000 > 1000 → remote wins
        assert_eq!(r, ApplyResult::Applied);
    }

    #[test]
    fn apply_concurrent_lww_ts_equal_nodeid_tiebreaker() {
        let mut s = storage();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""local""#, 1000).unwrap();

        // Concurrent, same ts → "node-b" > "node-a" lexicographically
        let remote_meta = DocMeta {
            vv: vec![(NODE_B.to_string(), 1)].into_iter().collect(),
            ts: 1000,
            node_id: Some(NODE_B.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &remote_meta).unwrap();
        assert_eq!(r, ApplyResult::Applied);
    }

    /// 裁决 §10.1（阶段四A P2 join stub）：stub 自举占位记录的 pmeta.ts 压 0——
    /// Concurrent 裁决按 pmeta.ts，ts=0 的零信息占位对任何真实记录皆输（多
    /// 设备账号中后加入设备的 stub 不得覆盖先加入设备的真实 whole）。同时钉
    /// 「无 ts 时间窗拦截」的对照：ts 更高的并发远端正常胜出。
    #[test]
    fn apply_concurrent_stub_ts_zero_always_loses() {
        let mut s = storage();
        // 先加入设备的真实记录（ts 是过去的真实时刻）
        let _ = put_personal(&mut s, NODE_A, KEY, r#""real""#, 1000).unwrap();
        // 后加入设备的 stub：vv 并发（{node-b:1} vs {node-a:1}）、ts=0
        let stub_meta = DocMeta {
            vv: vec![(NODE_B.to_string(), 1)].into_iter().collect(),
            ts: 0,
            node_id: Some(NODE_B.to_string()),
            ..Default::default()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""stub""#, &stub_meta).unwrap();
        assert_eq!(r, ApplyResult::LocalWins, "ts=0 的 stub 对真实记录必输");
        assert_eq!(s.get(KEY).unwrap().unwrap(), r#""real""#, "真实记录不被覆盖");
        // 对照：ts>0 的并发远端同 vv 形态会赢（ts 裁决生效，无 ts 窗拦截）
        let real_remote = DocMeta {
            ts: 3000,
            ..stub_meta.clone()
        };
        let r = apply_personal_remote(&mut s, KEY, r#""remote""#, &real_remote).unwrap();
        assert_eq!(r, ApplyResult::Applied, "ts 更高的并发远端正常胜出");
    }

    #[test]
    fn delete_personal_writes_tombstone() {
        let mut s = storage();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""data""#, 1000).unwrap();

        let meta = delete_personal(&mut s, NODE_A, KEY, 2000).unwrap();
        assert_eq!(meta.tombstone, Some(true));
        assert_eq!(meta.vv.get(NODE_A), Some(&2));
        assert!(s.get(KEY).unwrap().is_none());

        // pmeta persists
        let pmeta = get_personal_meta(&s, KEY).unwrap().unwrap();
        assert!(is_tombstone(&pmeta));
    }

    /// 远端墓碑合入：远端 meta 胜出且 `tombstone == true` 时删除记录本体
    /// （不落 null 值）+ 持久化墓碑 pmeta。
    #[test]
    fn apply_remote_tombstone_deletes_record_and_keeps_meta() {
        let mut s = storage();
        let _ = put_personal(&mut s, NODE_A, KEY, r#""data""#, 1000).unwrap();

        let tomb = DocMeta {
            vv: vec![(NODE_A.to_string(), 2)].into_iter().collect(),
            ts: 2000,
            node_id: Some(NODE_A.to_string()),
            tombstone: Some(true),
        };
        // 推送侧墓碑记录 value 恒 null（序列化后为 "null"）——不得落为本体
        let r = apply_personal_remote(&mut s, KEY, "null", &tomb).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert!(s.get(KEY).unwrap().is_none(), "记录本体应被删除");
        let pmeta = get_personal_meta(&s, KEY).unwrap().unwrap();
        assert!(is_tombstone(&pmeta));
        assert_eq!(pmeta.vv.get(NODE_A), Some(&2));

        // 墓碑重放：vv 相等 → Equal，幂等
        let r2 = apply_personal_remote(&mut s, KEY, "null", &tomb).unwrap();
        assert_eq!(r2, ApplyResult::Equal);
    }

    /// 真实双设备双向同步：A、B 各用各的 nodeId，各自写入后互相同步，
    /// 版本向量正确累积两个分量，且双方收敛到同一内容。
    ///
    /// 场景：A 建记录 → A 同步给 B → B 再改 → B 同步回 A → 最终两边一致。
    #[test]
    fn two_device_roundtrip_converges() {
        let mut a = storage();
        let mut b = storage();

        // 1) A 建记录
        let meta_a = put_personal(&mut a, NODE_A, KEY, r#""v1""#, 1000).unwrap();
        assert_eq!(meta_a.vv.get(NODE_A), Some(&1));

        // 2) A → B：把 A 的 pmeta + 值同步给 B
        let r = apply_personal_remote(&mut b, KEY, r#""v1""#, &meta_a).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(b.get(KEY).unwrap().unwrap(), r#""v1""#);
        // B 端 pmeta 保留 A 的 vv 分量（B 尚未写入，无 B 分量）
        let b_meta = get_personal_meta(&b, KEY).unwrap().unwrap();
        assert_eq!(b_meta.vv.get(NODE_A), Some(&1));

        // 3) B 在 A 基础上再改（B 分量 +1），vv 累积两个分量
        let meta_b = put_personal(&mut b, NODE_B, KEY, r#""v2""#, 2000).unwrap();
        assert_eq!(meta_b.vv.get(NODE_A), Some(&1));
        assert_eq!(meta_b.vv.get(NODE_B), Some(&1));

        // 4) B → A：A 没有 B 分量，远端（B）vv 含 B 分量 → B 胜出，A 收敛到 v2
        let r = apply_personal_remote(&mut a, KEY, r#""v2""#, &meta_b).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(a.get(KEY).unwrap().unwrap(), r#""v2""#);
        let a_meta = get_personal_meta(&a, KEY).unwrap().unwrap();
        // A 端 vv 现在同时含 A、B 两个分量（B 的分量来自远端 pmeta）
        assert_eq!(a_meta.vv.get(NODE_A), Some(&1));
        assert_eq!(a_meta.vv.get(NODE_B), Some(&1));

        // 5) 双方收敛到同一 vv → 再互相同步均为 Equal，不再改写。
        //    B 端最新 pmeta 为步骤 3 的 meta_b（含 A、B 两分量）。
        let r_a = apply_personal_remote(&mut b, KEY, r#""v2""#, &a_meta).unwrap();
        assert_eq!(r_a, ApplyResult::Equal);
        let r_b = apply_personal_remote(&mut a, KEY, r#""v2""#, &meta_b).unwrap();
        assert_eq!(r_b, ApplyResult::Equal);
    }

    /// 双设备并发写冲突：A、B 在互不知情时同时修改同一记录，版本向量
    /// 各自都只含自己的分量 → Concurrent。ts 相等时按 nodeId 字典序兜底，
    /// 所有设备必然收敛到同一个胜者（这里是 node-b）。
    #[test]
    fn two_device_concurrent_conflict_tiebreaker() {
        let mut a = storage();
        let mut b = storage();

        // 两侧各自并发写（互不知情，ts 相等）
        let meta_a = put_personal(&mut a, NODE_A, KEY, r#""fromA""#, 5000).unwrap();
        let meta_b = put_personal(&mut b, NODE_B, KEY, r#""fromB""#, 5000).unwrap();

        // A 收 B：Concurrent（各自含对方没有的分量），ts 相等 → node-b 胜
        let r = apply_personal_remote(&mut a, KEY, r#""fromB""#, &meta_b).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(a.get(KEY).unwrap().unwrap(), r#""fromB""#);

        // B 收 A：同样 Concurrent，ts 相等 → node-b 胜（B 自己）
        let r = apply_personal_remote(&mut b, KEY, r#""fromA""#, &meta_a).unwrap();
        assert_eq!(r, ApplyResult::LocalWins);
        assert_eq!(b.get(KEY).unwrap().unwrap(), r#""fromB""#);

        // 收敛：双方内容一致，都保留 node-b 的版本
        assert_eq!(a.get(KEY).unwrap().unwrap(), b.get(KEY).unwrap().unwrap());
    }

    /// 双设备并发写，ts 不同 → ts 大者胜（仍是确定性收敛）。
    #[test]
    fn two_device_concurrent_ts_wins() {
        let mut a = storage();
        let mut b = storage();

        let _ = put_personal(&mut a, NODE_A, KEY, r#""old""#, 1000).unwrap();
        // B 后写（ts 更大）
        let meta_b = put_personal(&mut b, NODE_B, KEY, r#""new""#, 9000).unwrap();

        // A 收 B：Concurrent，ts 9000 > 1000 → node-b 胜
        let r = apply_personal_remote(&mut a, KEY, r#""new""#, &meta_b).unwrap();
        assert_eq!(r, ApplyResult::Applied);
        assert_eq!(a.get(KEY).unwrap().unwrap(), r#""new""#);
    }
}
