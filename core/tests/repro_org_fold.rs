//! org 域 vv per-key 计数「折叠失明」复现 + 修复回归
//! （wiki/architecture/sync/org-vv-fix.md；写法对齐个人域 `repro_grant_fold.rs`）。
//!
//! 缺陷机理（§1.2）：数据账号受理删除走 per-key bump、不推进 `p2p:vvseq`
//! 序号分配器 → 受理删除后的下一次新写分到与墓碑相同的序号 → 已收讫墓碑
//! 的对端折叠 vv 覆盖该序号 → 增量采集判 Equal 跳过 → 新记录永久不可见。
//!
//! 运行：cargo test --test repro_org_fold -- --nocapture

use spark_core::storage::{
    BatchOperation, MemoryStorage, ScanOptions, StorageBackend, StorageError,
};
use spark_core::sync::meta::{DocMeta, VersionVector};
use spark_core::sync::orgsync::{
    collect_org_collection_vv, collect_org_incremental, org_tombstone_local,
};
use spark_core::sync::personal::put_personal;
use spark_core::sync::{get_personal_meta, is_tombstone, personal_meta_key};

const ORG_ID: &str = "org_0000000000000001";
const NAME: &str = "ai-chat:finance";
const VERSION: &str = "1.0.0";
const NOW: i64 = 1_720_000_000_000;
/// 数据账号 D（受理方 nodeId，vv 记账对象）。
const NODE_D: &str = "node-d";
/// D 的序号分配器键（个人域/org 域共享一条单调序列）。
const VV_SEQ_KEY: &str = "p2p:vvseq:node-d";

fn data_key(rel: &str) -> String {
    format!(
        "{}{rel}",
        spark_core::plugindata::org_data_prefix(ORG_ID, NAME, VERSION)
    )
}

/// 核心回归：受理写 k1 → 受理删 k1（墓碑）→ 再写 k2；k2 序号须大于墓碑
/// 序号（无碰撞），且对「已收讫墓碑」（knownVv 含墓碑序号）的对端，增量
/// 采集必含 k2——修复前 k2 与墓碑同序号被判 Equal 跳过，永久失明。
#[test]
fn repro_org_tombstone_seq_collision_blindness_fixed() {
    let mut s = MemoryStorage::new();
    // D 受理写 k1（seq 1）
    put_personal(&mut s, NODE_D, &data_key("k1"), "\"v1\"", NOW).unwrap();
    // D 受理删除 k1 → 墓碑（修复前：per-key bump 得 {D:2} 但序号键不动）
    let tomb = org_tombstone_local(
        &mut s,
        NODE_D,
        ORG_ID,
        NAME,
        VERSION,
        &data_key("k1"),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(tomb.tombstone, Some(true), "墓碑标记");
    assert_eq!(tomb.vv.get(NODE_D), Some(&2), "墓碑序号 = 2");
    assert!(s.get(&data_key("k1")).unwrap().is_none(), "本体已删");
    assert_eq!(
        s.get(VV_SEQ_KEY).unwrap().as_deref(),
        Some("2"),
        "序号键与墓碑同 batch 推进（修复前不动 → 碰撞根源）"
    );
    // org 域 dlog 有序号（墓碑传播双通道之一）
    let entries =
        spark_core::sync::orgsync::org_dlog_entries_after(&s, ORG_ID, NAME, VERSION, 0).unwrap();
    assert_eq!(entries, vec![(1, data_key("k1"))], "org dlog 登记删除");

    // D 再写 k2 → 序号 3（修复前恰分到 2，与墓碑同值碰撞）
    let m2 = put_personal(&mut s, NODE_D, &data_key("k2"), "\"v2\"", NOW + 2).unwrap();
    assert_eq!(
        m2.vv.get(NODE_D),
        Some(&3),
        "k2 序号须大于墓碑序号（无碰撞）"
    );

    // 已收讫墓碑的对端折叠 vv = {D:2}：增量采集须含 k2（修复前 Equal 跳过）
    let known: VersionVector = [(NODE_D.to_string(), 2)].into_iter().collect();
    let inc = collect_org_incremental(&s, ORG_ID, NAME, VERSION, &known, 0).unwrap();
    let keys: Vec<&str> = inc.iter().map(|r| r.key.as_str()).collect();
    println!("incremental vs {{node-d:2}} = {keys:?}");
    assert!(
        keys.contains(&data_key("k2").as_str()),
        "k2 对已收讫墓碑的对端必须可见（折叠失明回归）"
    );
}

/// orgsync 平面版「同集合第二条记录」用例：两条普通记录先后写入，per-node
/// 序号使第二条（{D:2}）对已持有第一条（{D:1}）的成员可见。
#[test]
fn org_second_record_visible_to_holder_of_first() {
    let mut s = MemoryStorage::new();
    put_personal(&mut s, NODE_D, &data_key("k1"), "\"v1\"", NOW).unwrap();
    put_personal(&mut s, NODE_D, &data_key("k2"), "\"v2\"", NOW + 1).unwrap();

    let fold = collect_org_collection_vv(&s, ORG_ID, NAME, VERSION).unwrap();
    println!("org fold = {fold:?}");
    assert_eq!(fold.get(NODE_D).copied(), Some(2), "折叠应为 {{node-d:2}}");

    let known: VersionVector = [(NODE_D.to_string(), 1)].into_iter().collect();
    let inc = collect_org_incremental(&s, ORG_ID, NAME, VERSION, &known, 0).unwrap();
    let keys: Vec<String> = inc.iter().map(|r| r.key.clone()).collect();
    println!("incremental vs {{node-d:1}} = {keys:?}");
    assert_eq!(keys, vec![data_key("k2")], "只缺第二条 → 只推第二条");
}

/// batch 注入失败的存储包装（原子性用例）：读写透传，batch 恒失败。
struct FailBatchStorage {
    inner: MemoryStorage,
}

impl StorageBackend for FailBatchStorage {
    fn get(&self, key: &str) -> spark_core::storage::Result<Option<String>> {
        self.inner.get(key)
    }
    fn put(&mut self, key: &str, value: &str) -> spark_core::storage::Result<()> {
        self.inner.put(key, value)
    }
    fn delete(&mut self, key: &str) -> spark_core::storage::Result<()> {
        self.inner.delete(key)
    }
    fn batch(&mut self, _operations: Vec<BatchOperation>) -> spark_core::storage::Result<()> {
        Err(StorageError::Backend("injected batch failure".to_string()))
    }
    fn scan(&self, options: &ScanOptions) -> spark_core::storage::Result<Vec<(String, String)>> {
        self.inner.scan(options)
    }
}

/// 原子性：org_tombstone_local 的四类写（本体删除 + 墓碑 pmeta + 序号键 +
/// org dlog）同一 batch 提交——batch 失败时不留「墓碑已写、本体未删」半态
/// （修复前三次分散写，中途失败即半态）。
#[test]
fn org_tombstone_local_atomic_on_batch_failure() {
    let mut inner = MemoryStorage::new();
    put_personal(&mut inner, NODE_D, &data_key("k1"), "\"v1\"", NOW).unwrap();
    let mut s = FailBatchStorage { inner };

    let r = org_tombstone_local(
        &mut s,
        NODE_D,
        ORG_ID,
        NAME,
        VERSION,
        &data_key("k1"),
        NOW + 1,
    );
    assert!(r.is_err(), "batch 注入失败 → 整体 Err");
    // 无任何半态：本体仍在、pmeta 非墓碑、序号键仍停在 1、dlog 无条目
    assert!(s.get(&data_key("k1")).unwrap().is_some(), "本体未被删");
    let meta = get_personal_meta(&s, &data_key("k1")).unwrap().unwrap();
    assert!(!is_tombstone(&meta), "不留墓碑半态");
    assert_eq!(meta.vv.get(NODE_D), Some(&1), "pmeta 未被 bump");
    assert_eq!(
        s.get(VV_SEQ_KEY).unwrap().as_deref(),
        Some("1"),
        "序号键未推进"
    );
    let entries =
        spark_core::sync::orgsync::org_dlog_entries_after(&s, ORG_ID, NAME, VERSION, 0).unwrap();
    assert!(entries.is_empty(), "org dlog 无条目");
}

/// 存量种子衔接：预置 per-key 形态的存量 pmeta（vv {D:5}，无 `p2p:vvseq`
/// 键——升级前数据），首次走新路径的序号须大于存量分量 max（种子逻辑自动
/// 覆盖 org 键形态，零迁移）。
#[test]
fn org_tombstone_seq_seeds_above_legacy_per_key_pmeta() {
    let mut s = MemoryStorage::new();
    let legacy = DocMeta {
        vv: [(NODE_D.to_string(), 5)].into_iter().collect(),
        ts: NOW,
        node_id: Some(NODE_D.to_string()),
        tombstone: None,
    };
    s.put(&data_key("k-old"), "\"old\"").unwrap();
    s.put(
        &personal_meta_key(&data_key("k-old")),
        &serde_json::to_string(&legacy).unwrap(),
    )
    .unwrap();
    assert!(
        s.get(VV_SEQ_KEY).unwrap().is_none(),
        "前置：无序号键（存量态）"
    );

    let tomb = org_tombstone_local(
        &mut s,
        NODE_D,
        ORG_ID,
        NAME,
        VERSION,
        &data_key("k-old"),
        NOW + 1,
    )
    .unwrap();
    assert_eq!(
        tomb.vv.get(NODE_D),
        Some(&6),
        "首读种子 = 存量分量 max(5)，新序号 6（恒大于存量 → 折叠/增量语义平滑衔接）"
    );
    assert_eq!(s.get(VV_SEQ_KEY).unwrap().as_deref(), Some("6"));
}
