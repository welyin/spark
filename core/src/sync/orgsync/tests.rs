//! orgsync 单元测试：复制组判定、dlog 键生成与生命周期、hello/need/data
//! body 构造-解析往返、增量采集与墓碑补推、分批切分。
//!
//! 对应 org-orgsync.md §20.1–20.4。

use super::*;
use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
use crate::plugindata::{
    Accounts, MergeRule, org_data_prefix, org_dlog_entry_prefix, org_dlog_seen_key,
    org_dlog_seq_key, org_dlog_wm_key,
};
use crate::storage::{MemoryStorage, ScanOptions, StorageBackend};
use crate::sync::meta::{DocMeta, VersionVector};
use serde_json::{Value, json};

// ── 复制组判定 ───────────────────────────────────────────────────────

fn member(root_id: &str) -> OrganizationMember {
    OrganizationMember {
        root_id: root_id.to_string(),
        role: OrganizationRole::Member,
        joined_at: 1000,
        added_by: "creator".to_string(),
        node_info: None,
        nickname: None,
        avatar: None,
        signature: None,
        gender: None,
        region: None,
        use_personal_identity: None,
        access_key: None,
        kind: None,
        org_binding: None,
        extra: Default::default(),
    }
}

fn make_record(members: &[&str], data_accounts: &[&str]) -> OrganizationRecord {
    let record = OrganizationRecord {
        org_id: "org_0000000000000001".to_string(),
        name: "test-org".to_string(),
        description: String::new(),
        avatar: String::new(),
        base_plugin_domain: None,
        created_at: 1000,
        created_by: "creator".to_string(),
        updated_at: 1000,
        members: members.iter().map(|rid| member(rid)).collect(),
        sync: None,
        gateways: vec![],
        data_accounts: data_accounts.iter().map(|r| r.to_string()).collect(),
        org_address: None,
        is_public: false,
        domain_type: None,
        extra: Default::default(),
    };
    record
}

#[test]
fn replication_group_all_members_includes_all() {
    let record = make_record(&["a", "b", "c"], &[]);
    assert!(is_in_replication_group(&record, "a", Accounts::AllMembers));
    assert!(is_in_replication_group(&record, "b", Accounts::AllMembers));
    assert!(!is_in_replication_group(&record, "x", Accounts::AllMembers));
}

#[test]
fn replication_group_data_accounts_only_data_accounts() {
    let record = make_record(&["a", "b", "c"], &["a", "c"]);
    assert!(is_in_replication_group(
        &record,
        "a",
        Accounts::DataAccounts
    ));
    assert!(!is_in_replication_group(
        &record,
        "b",
        Accounts::DataAccounts
    ));
    assert!(is_in_replication_group(
        &record,
        "c",
        Accounts::DataAccounts
    ));
    assert!(!is_in_replication_group(
        &record,
        "x",
        Accounts::DataAccounts
    ));
}

#[test]
fn replication_group_members_collects_correctly() {
    let record = make_record(&["a", "b", "c", "d"], &["a", "c"]);
    let all = replication_group_members(&record, Accounts::AllMembers);
    assert_eq!(all.len(), 4);
    let data = replication_group_members(&record, Accounts::DataAccounts);
    assert_eq!(data.len(), 2);
    assert!(data.contains(&"a".to_string()));
    assert!(data.contains(&"c".to_string()));
}

// ── org dlog 键生成与 append/seen/watermark/GC ───────────────────────

#[test]
fn org_dlog_keys_use_org_prefix() {
    // 键前缀/序号键/水位键/已收键均走 org 域命名空间
    let prefix = org_dlog_entry_prefix("org_01", "finance:ledger", "1.0.0");
    assert!(prefix.starts_with("dlog:org:org_01:finance:ledger@v1.0.0:entry:"));
    let seq_key = org_dlog_seq_key("org_01", "finance:ledger", "1.0.0");
    assert!(seq_key.starts_with("dlog:org:"));
    assert!(seq_key.ends_with(":seq"));
    // B6：wm/seen 键按 (rootId, peerId) 设备粒度
    let wm_key = org_dlog_wm_key("org_01", "finance:ledger", "1.0.0", "root-a", "peer-1");
    assert!(wm_key.contains(":wm:root-a:peer-1"));
    let seen_key = org_dlog_seen_key("org_01", "finance:ledger", "1.0.0", "root-a", "peer-1");
    assert!(seen_key.contains(":seen:root-a:peer-1"));
}

#[test]
fn org_dlog_append_and_read_roundtrip() {
    let mut s = MemoryStorage::new();
    let (seq, ops) = org_dlog_append_ops(&s, "org_01", "c", "1", "orgd:org_01:c@v1:k").unwrap();
    assert_eq!(seq, 1);
    s.batch(ops).unwrap();
    assert_eq!(org_dlog_current_seq(&s, "org_01", "c", "1").unwrap(), 1);

    // 读回
    let entries = org_dlog_entries_after(&s, "org_01", "c", "1", 0).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, 1);
    assert_eq!(entries[0].1, "orgd:org_01:c@v1:k");
}

#[test]
fn org_dlog_seen_monotonic() {
    let mut s = MemoryStorage::new();
    assert_eq!(
        org_dlog_get_seen(&s, "org_01", "c", "1", "root-a", "peer-1").unwrap(),
        0
    );
    org_dlog_set_seen(&mut s, "org_01", "c", "1", "root-a", "peer-1", 5).unwrap();
    assert_eq!(
        org_dlog_get_seen(&s, "org_01", "c", "1", "root-a", "peer-1").unwrap(),
        5
    );
    // 只增不减
    org_dlog_set_seen(&mut s, "org_01", "c", "1", "root-a", "peer-1", 3).unwrap();
    assert_eq!(
        org_dlog_get_seen(&s, "org_01", "c", "1", "root-a", "peer-1").unwrap(),
        5
    );
    // 不同设备各自独立
    assert_eq!(
        org_dlog_get_seen(&s, "org_01", "c", "1", "root-a", "peer-2").unwrap(),
        0
    );
}

#[test]
fn org_dlog_watermark_monotonic() {
    let mut s = MemoryStorage::new();
    let ack = org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-1", 3).unwrap();
    assert_eq!(ack, 3);
    let ack2 = org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-1", 5).unwrap();
    assert_eq!(ack2, 5);
    // 只增不减
    let ack3 = org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-1", 2).unwrap();
    assert_eq!(ack3, 5);
}

#[test]
fn org_dlog_gc_clears_below_threshold() {
    let mut s = MemoryStorage::new();
    for k in &["a", "b", "c", "d"] {
        let (_seq, ops) = org_dlog_append_ops(&s, "org_01", "c", "1", &format!("k:{k}")).unwrap();
        s.batch(ops).unwrap();
    }
    // GC threshold=2 → 删除 seq 1,2
    let removed = org_dlog_gc(&mut s, "org_01", "c", "1", 2).unwrap();
    assert_eq!(removed, 2);
    // 剩余 seq 3,4
    let entries = org_dlog_entries_after(&s, "org_01", "c", "1", 0).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, 3);
    assert_eq!(entries[1].0, 4);
}

#[test]
fn org_dlog_gc_threshold_uses_min_device_watermark() {
    let mut s = MemoryStorage::new();
    // B6：按设备水位取 min。root-a 两台设备水位 2 和 9，root-b 一台设备 5 →
    // threshold = min(2,9,5) = 2
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-a1", 2).unwrap();
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-a2", 9).unwrap();
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-b", "peer-b1", 5).unwrap();
    let members = vec![
        "root-a".to_string(),
        "root-b".to_string(),
        "self".to_string(),
    ];
    let threshold = org_dlog_gc_threshold(&s, "org_01", "c", "1", &members, "self").unwrap();
    assert_eq!(threshold, 2);
}

/// F8：min 只覆盖等待集合（复制组成员）内的键——漂移/已退出成员残留的 wm
/// 键（不在 replication_members 内）不得永久压低阈值。
#[test]
fn org_dlog_gc_threshold_ignores_stale_watermark_keys_outside_wait_set() {
    let mut s = MemoryStorage::new();
    // 等待集合 = [root-a, self]（root-a 水位 8）；残留 root-gone（已退出成员）
    // 水位 1 + 漂移 peer 残留（root-a 旧 peer）水位 1——均不得压低阈值。
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-a1", 8).unwrap();
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-gone", "peer-gone", 1).unwrap();
    let members = vec!["root-a".to_string(), "self".to_string()];
    let threshold = org_dlog_gc_threshold(&s, "org_01", "c", "1", &members, "self").unwrap();
    assert_eq!(
        threshold, 8,
        "min 只覆盖等待集合内成员水位，残留键不压低阈值"
    );
}

#[test]
fn org_dlog_gc_threshold_blocks_when_member_has_no_device_watermark() {
    let mut s = MemoryStorage::new();
    // root-a 有设备水位，但 root-b 无任何设备水位记录 → 阻塞不清（返回 0）
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-a1", 7).unwrap();
    let members = vec![
        "root-a".to_string(),
        "root-b".to_string(),
        "self".to_string(),
    ];
    let threshold = org_dlog_gc_threshold(&s, "org_01", "c", "1", &members, "self").unwrap();
    assert_eq!(threshold, 0, "无记录阻塞不清");
}

/// 阶段四A P1 钉住（org-member-split §2.1「GC 注意」）：成员移除 = 成员记录
/// 墓碑，被踢者随 whole 合入**出成员表**（等待集合按当前成员表动态计算）——
/// 其无任何设备水位记录也不阻塞其墓碑的 GC（若在等待集合内则会阻塞，见上
/// 例）；其残留 wm 键也不压低阈值（F8 口径的 P1 场景化）。
#[test]
fn org_dlog_gc_threshold_kicked_member_does_not_block() {
    let mut s = MemoryStorage::new();
    // 剩余成员 root-a 有水位 6；被踢者 root-kicked 无任何水位记录
    org_dlog_set_watermark(&mut s, "org_01", "c", "1", "root-a", "peer-a1", 6).unwrap();
    // 场景一：被踢者已出复制组（等待集合按当前成员表）→ 不阻塞
    let members_after_kick = vec!["root-a".to_string(), "self".to_string()];
    let threshold =
        org_dlog_gc_threshold(&s, "org_01", "c", "1", &members_after_kick, "self").unwrap();
    assert_eq!(threshold, 6, "被踢者出表后不阻塞 GC（其墓碑可清）");
    // 场景二（对照）：被踢者仍在成员表且无水位 → 阻塞
    let members_before_kick = vec![
        "root-a".to_string(),
        "root-kicked".to_string(),
        "self".to_string(),
    ];
    let blocked =
        org_dlog_gc_threshold(&s, "org_01", "c", "1", &members_before_kick, "self").unwrap();
    assert_eq!(blocked, 0, "在表成员无水位仍阻塞（对照）");
}

#[test]
fn org_dlog_gc_threshold_empty_wait_set_returns_current_seq() {
    let mut s = MemoryStorage::new();
    let (_seq, ops) = org_dlog_append_ops(&s, "org_01", "c", "1", "k").unwrap();
    s.batch(ops).unwrap();
    // 只有本机一个成员 → 无等待集合 → 返回当前最大序号
    let members = vec!["self".to_string()];
    let threshold = org_dlog_gc_threshold(&s, "org_01", "c", "1", &members, "self").unwrap();
    assert_eq!(threshold, 1);
}

// ── hello body 构造-解析往返 ─────────────────────────────────────────

#[test]
fn hello_roundtrip() {
    let mut collections = serde_json::Map::new();
    let vv: VersionVector = [("node-a".to_string(), 3)].into_iter().collect();
    collections.insert(
        "finance:ledger@v1.0.0".to_string(),
        json!({"vv": vv, "dlogAck": 7}),
    );
    let body = build_orgsync_hello("org_01", collections, &["data".to_string()], "pc");
    let parsed = parse_orgsync_hello(&body).unwrap();
    assert_eq!(parsed.0, "org_01");
    assert_eq!(parsed.2, vec!["data"]);
    assert_eq!(parsed.3, "pc");
    assert_eq!(parsed.1.len(), 1);
    let (p_vv, p_ack, p_degraded) = &parsed.1["finance:ledger@v1.0.0"];
    assert_eq!(p_vv.get("node-a"), Some(&3));
    assert_eq!(*p_ack, 7);
    assert!(!p_degraded, "未标注 degraded → false");
}

/// F4：hello 解析保留 degraded 标注（数据账号对 filtered 集合插件未运行降级）。
#[test]
fn hello_roundtrip_preserves_degraded() {
    let mut collections = serde_json::Map::new();
    let vv: VersionVector = [("node-a".to_string(), 1)].into_iter().collect();
    collections.insert(
        "ai-chat:ledger@v1.0.0".to_string(),
        json!({"vv": vv, "dlogAck": 0, "degraded": true}),
    );
    let body = build_orgsync_hello("org_01", collections, &["data".to_string()], "pc");
    let parsed = parse_orgsync_hello(&body).unwrap();
    let (_, _, degraded) = &parsed.1["ai-chat:ledger@v1.0.0"];
    assert!(degraded, "hello 摘要 degraded:true 被保留");
}

/// B2：hello 的 dlogAck 按收件人逐成员生成——`collect_org_collections`
/// 的 dlogAck = 我已收讫**对端**该集合删除日志最大序号（`org_dlog_get_seen`），
/// 每个收件人（rootId, peerId）取各自已收序号，不共享同一值。
#[test]
fn collect_org_collections_per_recipient_dlog_ack() {
    let mut s = MemoryStorage::new();
    // 写一条 org dlog（已收序号来自对端推进我们的日志？不——dlogAck 是对端
    // 删除日志的 seen。这里直接为两个收件人设置不同的 seen 值）
    org_dlog_set_seen(&mut s, "org_01", "c", "1", "member-a", "peer-a1", 3).unwrap();
    org_dlog_set_seen(&mut s, "org_01", "c", "1", "member-b", "peer-b1", 9).unwrap();

    let collections = vec![("c".to_string(), "1".to_string())];
    // 收件人 A：dlogAck = 3
    let map_a = collect_org_collections(&s, "org_01", &collections, "member-a", "peer-a1").unwrap();
    assert_eq!(map_a["c@v1"]["dlogAck"], json!(3));
    // 收件人 B：dlogAck = 9
    let map_b = collect_org_collections(&s, "org_01", &collections, "member-b", "peer-b1").unwrap();
    assert_eq!(map_b["c@v1"]["dlogAck"], json!(9));
}

#[test]
fn need_roundtrip() {
    let vv: VersionVector = [("node-a".to_string(), 5)].into_iter().collect();
    let body = build_orgsync_need("org_01", "c@v1", &vv, 3);
    let parsed = parse_orgsync_need(&body).unwrap();
    assert_eq!(parsed.0, "org_01");
    assert_eq!(parsed.1, "c@v1");
    assert_eq!(parsed.2.get("node-a"), Some(&5));
    assert_eq!(parsed.3, 3);
}

#[test]
fn data_roundtrip() {
    let records = vec![
        OrgsyncRecord {
            key: "orgd:org_01:c@v1:k1".to_string(),
            value: json!("hello"),
            meta: DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: 1000,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            },
            dseq: None,
        },
        OrgsyncRecord {
            key: "orgd:org_01:c@v1:k2".to_string(),
            value: Value::Null,
            meta: DocMeta {
                vv: [("node-a".to_string(), 2)].into_iter().collect(),
                ts: 2000,
                node_id: Some("node-a".to_string()),
                tombstone: Some(true),
            },
            dseq: Some(3),
        },
    ];
    let body = build_orgsync_data_batch("org_01", "c@v1", &records, 0, 2);
    let parsed = parse_orgsync_data(&body).unwrap();
    assert_eq!(parsed.0, "org_01");
    assert_eq!(parsed.1, "c@v1");
    assert_eq!(parsed.2.len(), 2);
    assert_eq!(parsed.2[0].key, "orgd:org_01:c@v1:k1");
    assert_eq!(parsed.2[1].dseq, Some(3));
    assert!(parsed.2[1].meta.tombstone.unwrap_or(false));
}

// ── 增量采集与墓碑补推 ──────────────────────────────────────────────

#[test]
fn collect_org_incremental_finds_newer_records() {
    let mut s = MemoryStorage::new();
    let prefix = org_data_prefix("org_01", "c", "1");
    // 先模拟两条数据：直接用 raw 写入（避开 VersionedStorage 的自动记账——
    // 这些是"已存在"的数据），然后手动写 pmeta
    s.put(&format!("{prefix}a"), r#""old""#).unwrap();
    s.put(&format!("{prefix}b"), r#""old""#).unwrap();
    // 手动写 pmeta
    let meta_a = DocMeta {
        vv: [("node-a".to_string(), 2)].into_iter().collect(),
        ts: 2000,
        node_id: Some("node-a".to_string()),
        ..Default::default()
    };
    let meta_b = DocMeta {
        vv: [("node-a".to_string(), 1), ("node-b".to_string(), 3)]
            .into_iter()
            .collect(),
        ts: 3000,
        node_id: Some("node-b".to_string()),
        ..Default::default()
    };
    s.put(
        &format!("pmeta:{prefix}a"),
        &serde_json::to_string(&meta_a).unwrap(),
    )
    .unwrap();
    s.put(
        &format!("pmeta:{prefix}b"),
        &serde_json::to_string(&meta_b).unwrap(),
    )
    .unwrap();

    // knownVv: node-a=1, node-b=1 → a 领先（node-a:2>1），b 并发（node-a:1==1, node-b:3>1）
    let known: VersionVector = [("node-a".to_string(), 1), ("node-b".to_string(), 1)]
        .into_iter()
        .collect();
    let records = collect_org_incremental(&s, "org_01", "c", "1", &known, 0).unwrap();
    // a 本机领先 → 纳入；b 并发 → 纳入（CompareResult 不是 Remote/Equal）
    assert!(records.iter().any(|r| r.key == format!("{prefix}a")));
    assert!(records.iter().any(|r| r.key == format!("{prefix}b")));

    // knownVv 包含 node-b:3 → b Equal，不纳入
    let known2: VersionVector = [("node-a".to_string(), 1), ("node-b".to_string(), 3)]
        .into_iter()
        .collect();
    let records2 = collect_org_incremental(&s, "org_01", "c", "1", &known2, 0).unwrap();
    assert!(!records2.iter().any(|r| r.key == format!("{prefix}b")));
}

#[test]
fn collect_org_tombstones_returns_dlog_entries() {
    let mut s = MemoryStorage::new();
    let prefix = org_data_prefix("org_01", "c", "1");
    // 先追加删除日志
    let (seq1, ops1) = org_dlog_append_ops(&s, "org_01", "c", "1", &format!("{prefix}t1")).unwrap();
    s.batch(ops1).unwrap();
    let (seq2, ops2) = org_dlog_append_ops(&s, "org_01", "c", "1", &format!("{prefix}t2")).unwrap();
    s.batch(ops2).unwrap();

    // 给被删记录写墓碑 pmeta
    let tomb_meta = DocMeta {
        vv: [("node-a".to_string(), 1)].into_iter().collect(),
        ts: 1000,
        node_id: Some("node-a".to_string()),
        tombstone: Some(true),
    };
    s.put(
        &format!("pmeta:{prefix}t1"),
        &serde_json::to_string(&tomb_meta).unwrap(),
    )
    .unwrap();
    s.put(
        &format!("pmeta:{prefix}t2"),
        &serde_json::to_string(&tomb_meta).unwrap(),
    )
    .unwrap();

    let tombs = collect_org_tombstones_after(&s, "org_01", "c", "1", 0).unwrap();
    assert_eq!(tombs.len(), 2);
    assert_eq!(tombs[0].dseq, Some(seq1));
    assert_eq!(tombs[1].dseq, Some(seq2));

    // after=seq1 → 只返回 seq2
    let tombs2 = collect_org_tombstones_after(&s, "org_01", "c", "1", seq1).unwrap();
    assert_eq!(tombs2.len(), 1);
    assert_eq!(tombs2[0].dseq, Some(seq2));
}

// ── 分批切分 ─────────────────────────────────────────────────────────

#[test]
fn split_orgsync_batches_respects_byte_limit() {
    let records: Vec<OrgsyncRecord> = (0..5)
        .map(|i| OrgsyncRecord {
            key: format!("orgd:org_01:c@v1:k{i}"),
            value: json!({"data": "x".repeat(5000)}), // ~5KB each
            meta: DocMeta::default(),
            dseq: None,
        })
        .collect();
    // limit = 12KB → 5 × ~5KB = ~25KB → 3 batches
    let batches = split_orgsync_batches(records, 12_000);
    assert!(batches.len() >= 2, "should split into multiple batches");
    let total: usize = batches.iter().map(|b| b.len()).sum();
    assert_eq!(total, 5);
}

#[test]
fn split_orgsync_batches_single_batch_when_small() {
    let records: Vec<OrgsyncRecord> = (0..3)
        .map(|i| OrgsyncRecord {
            key: format!("k{i}"),
            value: json!("small"),
            meta: DocMeta::default(),
            dseq: None,
        })
        .collect();
    let batches = split_orgsync_batches(records, 1_000_000);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 3);
}

/// B5/F2-P1：`org:coll:` 声明并入 org:structure 键域全员同步——声明 pmeta
/// 折叠进 **org:structure** 集合 vv、随 org:structure 增量传播；所属插件
/// 集合的折叠/增量不再 per-collection 携带声明（避免双通道重复）。
#[test]
fn org_coll_declaration_is_synced_as_system_collection() {
    use crate::sync::personal::put_personal;
    let mut s = MemoryStorage::new();
    let decl_key = crate::plugindata::org_decl_key("org_01", "finance:ledger", "1.0.0");
    // 声明记录 + pmeta（版本化）
    s.put(&decl_key, r#"{"name":"finance:ledger"}"#).unwrap();
    // 一条普通数据：须先落数据、再落声明 pmeta——per-node 序号下声明 pmeta
    // （node-a:5）会把 node-a 序号种子到 5，数据后写将拿到序号 6，反而高于
    // 声明自身，增量过滤场景失焦。先写数据 → 数据 vv={node-a:1} 保持"旧"。
    let prefix = org_data_prefix("org_01", "finance:ledger", "1.0.0");
    put_personal(&mut s, "node-a", &format!("{prefix}k"), "\"v\"", 1000).unwrap();
    let decl_meta = DocMeta {
        vv: [("node-a".to_string(), 5)].into_iter().collect(),
        ts: 2000,
        node_id: Some("node-a".to_string()),
        ..Default::default()
    };
    s.put(
        &format!("pmeta:{decl_key}"),
        &serde_json::to_string(&decl_meta).unwrap(),
    )
    .unwrap();

    // F2-P1：声明 pmeta 折叠进 org:structure（all-members）集合 vv
    let folded = collect_org_collection_vv(&s, "org_01", "org:structure", "1").unwrap();
    assert!(
        folded.get("node-a").copied().unwrap_or(0) >= 5,
        "声明 pmeta 折叠进 org:structure vv"
    );
    // 插件集合折叠不再携带声明（decl 不在 orgd: 键域）
    let plugin_fold = collect_org_collection_vv(&s, "org_01", "finance:ledger", "1.0.0").unwrap();
    assert_eq!(
        plugin_fold.get("node-a").copied(),
        Some(1),
        "插件集合折叠只含数据记录"
    );

    // 增量：org:structure 通道 knownVv node-a=4 → 声明（node-a:5 领先）纳入
    let known: VersionVector = [("node-a".to_string(), 4)].into_iter().collect();
    let inc = collect_org_incremental(&s, "org_01", "org:structure", "1", &known, 0).unwrap();
    assert!(
        inc.iter().any(|r| r.key == decl_key),
        "声明记录经 org:structure 增量传播（声明先行全员可达）"
    );
    // 插件集合增量只含数据记录（knownVv 空 → 数据纳入；声明不内联携带）
    let inc_plugin = collect_org_incremental(
        &s,
        "org_01",
        "finance:ledger",
        "1.0.0",
        &Default::default(),
        0,
    )
    .unwrap();
    assert!(
        inc_plugin.iter().any(|r| r.key == format!("{prefix}k")),
        "数据记录纳入插件集合增量"
    );
    assert!(
        !inc_plugin.iter().any(|r| r.key == decl_key),
        "声明不再随所属集合流量携带"
    );
}

/// B4：远端墓碑接力进正确的删除日志作用域——`apply_personal_remote_no_dlog`
/// 不污染个人域 dlog，org 域调用方补登 org dlog（dlog:org:）即可接力 A→B→C。
#[test]
fn org_tombstone_relay_appends_org_dlog_not_personal() {
    use crate::sync::personal::apply_personal_remote_no_dlog;
    let mut s = MemoryStorage::new();
    let prefix = org_data_prefix("org_01", "c", "1");
    let key = format!("{prefix}k");

    // 远端墓碑：vv 领先，tombstone=true
    let remote_meta = DocMeta {
        vv: [("node-a".to_string(), 2)].into_iter().collect(),
        ts: 2000,
        node_id: Some("node-a".to_string()),
        tombstone: Some(true),
    };

    // 先落一条本地数据（vv=1），再合入远端墓碑（vv=2 领先 → 采纳）
    s.put(&key, "\"v1\"").unwrap();
    // 手动写本地 pmeta
    let local_meta = DocMeta {
        vv: [("node-a".to_string(), 1)].into_iter().collect(),
        ts: 1000,
        node_id: Some("node-a".to_string()),
        ..Default::default()
    };
    s.put(
        &format!("pmeta:{key}"),
        &serde_json::to_string(&local_meta).unwrap(),
    )
    .unwrap();

    // no-dlog apply：墓碑落地，但不追加个人域 dlog
    let result = apply_personal_remote_no_dlog(&mut s, &key, "null", &remote_meta).unwrap();
    assert!(result.did_apply());
    assert!(s.get(&key).unwrap().is_none(), "墓碑删除本体");

    // 个人域 dlog（dlog: 无前缀）为空——不被 orgd 污染
    let personal_dlog: Vec<_> = s
        .scan(&ScanOptions::prefix("dlog:entry:"))
        .unwrap()
        .into_iter()
        .collect();
    assert!(personal_dlog.is_empty(), "个人域 dlog 不被 org 记录污染");

    // org 域 dlog 补登：作用域 dlog:org:{orgId}:{name}@v{version}
    let (_seq, ops) = org_dlog_append_ops(&s, "org_01", "c", "1", &key).unwrap();
    s.batch(ops).unwrap();
    let entries = org_dlog_entries_after(&s, "org_01", "c", "1", 0).unwrap();
    assert_eq!(entries.len(), 1, "org 域墓碑接力进 org dlog");
    assert_eq!(entries[0].1, key);
}

// ── diff 裁决 ────────────────────────────────────────────────────────

#[test]
fn diff_org_equal_remote_local_concurrent() {
    let a1: VersionVector = [("a".to_string(), 1)].into_iter().collect();
    let a2: VersionVector = [("a".to_string(), 2)].into_iter().collect();
    let b1: VersionVector = [("b".to_string(), 1)].into_iter().collect();

    assert!(matches!(
        diff_org_collection(&a1, &a1),
        OrgDiffOutcome::Equal
    ));
    assert!(matches!(
        diff_org_collection(&a1, &a2),
        OrgDiffOutcome::LocalBehind { .. }
    ));
    assert!(matches!(
        diff_org_collection(&a2, &a1),
        OrgDiffOutcome::LocalAhead
    ));
    assert!(matches!(
        diff_org_collection(&a1, &b1),
        OrgDiffOutcome::Concurrent
    ));
}

// ── 设备类（F7：与 pdsync 复用同一份 local_device_class）───────────────

#[test]
fn local_device_class_is_pc_or_mobile() {
    let dc = crate::sync::pdsync::local_device_class();
    assert!(dc == "pc" || dc == "mobile");
}

// ── O2b 内建 all-members 集合注册与键域归属 ────────────────────────────

/// 内建集合名称/版本/线上标识。
#[test]
fn builtin_collection_names_versions() {
    let s = BuiltinOrgCollection::Structure;
    assert_eq!(s.name(), "org:structure");
    assert_eq!(s.version(), "1");
    assert_eq!(s.full_name(), "org:structure@v1");
    assert_eq!(s.merge(), MergeRule::Whole);
    assert_eq!(BuiltinOrgCollection::Contacts.merge(), MergeRule::LwwRecord);
    // F7：org:invites 退出 orgsync（邀请记录回归 personal 域自设备同步）；
    // batch3 §2：org:invitations 管理面投影集合加入（invpub 键域）
    assert!(BuiltinOrgCollection::all().len() == 3);
    assert_eq!(BuiltinOrgCollection::Invitations.name(), "org:invitations");
    assert_eq!(
        BuiltinOrgCollection::Invitations.merge(),
        MergeRule::LwwRecord
    );
}

/// 键域归属：内建集合映射到存量键前缀（键不搬家，零迁移）。
#[test]
fn builtin_collection_key_domains() {
    let org = "org_0000000000000001";
    // F2-P1：`org:coll:{orgId}:`（集合声明）并入——声明全员可见，不经 hello
    // 复制组裁剪。阶段四A P1：`org:member:{orgId}:`（per-member 成员记录）
    // 并入——全员流动。阶段四F：`org:evi:anchor:{orgId}:`（存证节点锚）并入
    // ——全员流动。C7：`org:acl:{orgId}:`（O4 授权名单）随 encrypted 轴退役
    // 移出本键域。C1：`org:genesis:`（创世策略记录）/ `org:policy:`（策略
    // 修订链）/ `org:verifiers:`（验证人信任声明）并入——组织策略与信任锚
    // 属结构集合，全员流动。policy §2：`org:policydoc:`（发布策略文档，
    // sigSet 合入校验）并入——同集合全员流动。退出留史：`org:cleave:`
    // （共同体退出留史记录，append-only）并入——全员流动，各节点据此确定性
    // 推导空域只读档案状态。
    assert_eq!(
        BuiltinOrgCollection::Structure.data_prefixes(org),
        vec![
            "org:meta:org_0000000000000001",
            "org:coll:org_0000000000000001:",
            "org:member:org_0000000000000001:",
            "org:evi:anchor:org_0000000000000001:",
            "org:genesis:org_0000000000000001",
            "org:policy:org_0000000000000001:",
            "org:verifiers:org_0000000000000001",
            "org:policydoc:org_0000000000000001",
            "org:cleave:org_0000000000000001:"
        ]
    );
    assert_eq!(
        BuiltinOrgCollection::Contacts.data_prefixes(org),
        vec!["ct:org:org_0000000000000001:"]
    );
    assert_eq!(
        BuiltinOrgCollection::Invitations.data_prefixes(org),
        vec!["org:invpub:org_0000000000000001:"]
    );
    // 线上标识键
    assert_eq!(
        BuiltinOrgCollection::Structure.decl_key(org),
        "org:coll:org_0000000000000001:org:structure@v1"
    );
}

/// 集合数据键域解析：内建 → 存量前缀；插件 → orgd: 前缀。
#[test]
fn collection_data_prefixes_resolution() {
    assert_eq!(
        collection_data_prefixes("org_01", "org:contacts", "1"),
        vec!["ct:org:org_01:"]
    );
    // 插件集合走 orgd:
    assert_eq!(
        collection_data_prefixes("org_01", "ai-chat:finance", "1.0.0"),
        vec!["orgd:org_01:ai-chat:finance@v1.0.0:"]
    );
}

/// 存量组织键 → 内建集合作用域解析（删除日志路由用）。
#[test]
fn legacy_org_key_scope_maps_to_builtin_collections() {
    // org:meta:{orgId}
    let (oid, name, ver) = legacy_org_key_scope("org:meta:org_01").unwrap();
    assert_eq!(oid, "org_01");
    assert_eq!(name, "org:structure");
    assert_eq!(ver, "1");
    // ct:org:{orgId}:
    let (oid, name, _) = legacy_org_key_scope("ct:org:org_01:x").unwrap();
    assert_eq!(oid, "org_01");
    assert_eq!(name, "org:contacts");
    // F7：org:inv:in/out 退出 orgsync——不再映射任何内建集合（删除只登
    // 个人域 dlog，墓碑随 pdsync 自设备传播）
    assert!(legacy_org_key_scope("org:inv:in:org_01:peer-a").is_none());
    assert!(legacy_org_key_scope("org:inv:out:org_01:peer-a").is_none());
    // 非存量组织键 → None（插件 orgd: 走 parse_org_data_key）
    assert!(legacy_org_key_scope("orgd:org_01:c@v1:k").is_none());
    assert!(legacy_org_key_scope("ct:friend:x").is_none());
}

/// 内建集合的增量采集覆盖存量键前缀（键不搬家）：写入 ct:org:{orgId}:*
/// 记录（pmeta）→ collect_org_incremental 按 knownVv 纳入。
#[test]
fn builtin_collection_incremental_scans_legacy_prefix() {
    use crate::sync::personal::put_personal;
    let mut s = MemoryStorage::new();
    // org:contacts 集合的数据在 ct:org:{orgId}:* 前缀（非 orgd:）
    put_personal(&mut s, "node-a", "ct:org:org_01:member-x", "\"v1\"", 1000).unwrap();
    // org:structure 集合的数据在 org:meta:{orgId}（单记录 whole）
    put_personal(
        &mut s,
        "node-a",
        "org:meta:org_01",
        "{\"name\":\"t\"}",
        1001,
    )
    .unwrap();

    // 折叠 vv 应包含各内建集合的数据（node-a 分量）
    assert!(
        collect_org_collection_vv(&s, "org_01", "org:contacts", "1")
            .unwrap()
            .get("node-a")
            .is_some()
    );
    assert!(
        collect_org_collection_vv(&s, "org_01", "org:structure", "1")
            .unwrap()
            .get("node-a")
            .is_some()
    );

    // 增量采集：knownVv 空 → 三条存量记录全部纳入各自集合
    let contacts =
        collect_org_incremental(&s, "org_01", "org:contacts", "1", &VersionVector::new(), 0)
            .unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].key, "ct:org:org_01:member-x");
    let structure =
        collect_org_incremental(&s, "org_01", "org:structure", "1", &VersionVector::new(), 0)
            .unwrap();
    assert_eq!(structure.len(), 1);
    assert_eq!(structure[0].key, "org:meta:org_01");
    // F7：org:inv:* 已非任何 orgsync 集合键域——`org:invites` 集合不存在，
    // 折叠/增量为空
    assert!(
        collect_org_collection_vv(&s, "org_01", "org:invites", "1")
            .unwrap()
            .is_empty()
    );
}

/// 内建集合删除日志：collect_org_tombstones_after 只认本集合键域的墓碑。
#[test]
fn builtin_collection_tombstones_are_filtered_to_key_domain() {
    let mut s = MemoryStorage::new();
    // 给 org:contacts 集合的数据追加删除日志条目
    let key = "ct:org:org_01:member-x";
    let (_seq, ops) = org_dlog_append_ops(&s, "org_01", "org:contacts", "1", key).unwrap();
    s.batch(ops).unwrap();
    // 墓碑 pmeta
    let tomb = DocMeta {
        vv: [("node-a".to_string(), 2)].into_iter().collect(),
        ts: 2000,
        node_id: Some("node-a".to_string()),
        tombstone: Some(true),
    };
    s.put(
        &format!("pmeta:{key}"),
        &serde_json::to_string(&tomb).unwrap(),
    )
    .unwrap();

    // org:contacts 采集到该墓碑；org:structure 采集不到（键域外）
    let contacts = collect_org_tombstones_after(&s, "org_01", "org:contacts", "1", 0).unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].key, key);
    let structure = collect_org_tombstones_after(&s, "org_01", "org:structure", "1", 0).unwrap();
    assert!(structure.is_empty(), "键域外墓碑不误采集");
}

/// 卫生批项3：已退出成员的 org dlog wm/seen 键清理——按 org 清该成员全部
/// 设备粒度键，其他成员/其他 org 的键不受影响。
#[test]
fn org_dlog_remove_member_marks_scoped() {
    use crate::plugindata::{org_dlog_seen_key, org_dlog_wm_key};
    let mut s = MemoryStorage::new();
    let org = "org_01";
    let (x, y) = ("x".repeat(64), "y".repeat(64));
    // X 的两台设备 wm + seen（两个集合）+ Y 的一台 + 另一个 org 的 X
    let keys = vec![
        org_dlog_wm_key(org, "ai-chat:finance", "1.0.0", &x, "peer-x1"),
        org_dlog_seen_key(org, "ai-chat:finance", "1.0.0", &x, "peer-x2"),
        org_dlog_wm_key(org, "org:structure", "1", &x, "peer-x1"),
        org_dlog_wm_key(org, "ai-chat:finance", "1.0.0", &y, "peer-y1"),
        org_dlog_wm_key("org_02", "ai-chat:finance", "1.0.0", &x, "peer-x1"),
    ];
    for k in &keys {
        s.put(k, "3").unwrap();
    }
    let removed = super::dlog::org_dlog_remove_member_marks(&mut s, org, &x).unwrap();
    assert_eq!(removed, 3, "X 在本 org 的 wm+seen 全清");
    assert!(s.get(&keys[3]).unwrap().is_some(), "Y 的键保留");
    assert!(s.get(&keys[4]).unwrap().is_some(), "其他 org 的 X 键保留");
    // 幂等：再清为零
    assert_eq!(
        super::dlog::org_dlog_remove_member_marks(&mut s, org, &x).unwrap(),
        0
    );
}
