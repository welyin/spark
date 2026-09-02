//! dm_offline 模块单元测试（拆分至独立文件，service.rs 约 250 行 + 测试罗列另置）。

use serde_json::json;

use super::*;
use crate::storage::{MemoryStorage, StorageBackend};
use crate::sync::personal::{apply_personal_remote, get_personal_meta};

const NODE_A: &str = "node-a";
const NODE_B: &str = "node-b";
/// 一小时（毫秒）。
const H: i64 = 3600 * 1000;

fn record(to: &str, msg_id: &str, kind: &str, created_at: i64) -> PendingRecord {
    PendingRecord {
        to: to.to_string(),
        message_id: msg_id.to_string(),
        kind: kind.to_string(),
        space_key: "personal".to_string(),
        conv_id: Some("conv-1".to_string()),
        envelope: json!({ "kind": kind, "to": to }),
        created_at,
    }
}

/// 存储键线形：个人 `dm:pending:{to}:{messageId}`、组织 `org:dm:pending:{orgId}:{to}:{messageId}`。
#[test]
fn storage_key_shape() {
    assert_eq!(
        pending_key(PendingSpace::Personal, "rootB", "m1"),
        "dm:pending:rootB:m1"
    );
    assert_eq!(
        pending_key(PendingSpace::Org("org_abc"), "rootB", "m1"),
        "org:dm:pending:org_abc:rootB:m1"
    );
    // pdsync category 命中个人 pending 前缀（feed 复用个人空间键）
    assert!(crate::sync::pdsync::category_for_key("dm:pending:rootB:m1").is_some());
    // 组织 pending 不进 pdsync（org-sync 通道）
    assert!(crate::sync::pdsync::category_for_key("org:dm:pending:org_abc:rootB:m1").is_none());
}

/// 入队 + 列出 + 删除 基本往返（幂等：同 messageId 覆盖）。
#[test]
fn enqueue_list_remove_roundtrip() {
    let mut s = MemoryStorage::new();
    let r1 = record("rootB", "m1", "chat", 1000);
    enqueue(&mut s, PendingSpace::Personal, "rootB", &r1, NODE_A, 1000).unwrap();
    enqueue(&mut s, PendingSpace::Personal, "rootB", &r1, NODE_A, 2000).unwrap();

    let list = list_for_recipient(&s, PendingSpace::Personal, "rootB", 3000).unwrap();
    assert_eq!(list.len(), 1, "幂等入队不重复");
    assert_eq!(list[0].0, "dm:pending:rootB:m1");
    assert_eq!(list[0].1, r1);

    remove(&mut s, &list[0].0).unwrap();
    assert!(
        list_for_recipient(&s, PendingSpace::Personal, "rootB", 3000)
            .unwrap()
            .is_empty()
    );
}

/// 不同 recipient / 不同 messageId 各自独立键。
#[test]
fn different_recipients_and_ids_isolated() {
    let mut s = MemoryStorage::new();
    enqueue(
        &mut s,
        PendingSpace::Personal,
        "rootB",
        &record("rootB", "m1", "chat", 1000),
        NODE_A,
        1000,
    )
    .unwrap();
    enqueue(
        &mut s,
        PendingSpace::Personal,
        "rootC",
        &record("rootC", "m1", "chat", 1000),
        NODE_A,
        1000,
    )
    .unwrap();
    enqueue(
        &mut s,
        PendingSpace::Personal,
        "rootB",
        &record("rootB", "m2", "feed", 1000),
        NODE_A,
        1000,
    )
    .unwrap();

    let b = list_for_recipient(&s, PendingSpace::Personal, "rootB", 3000).unwrap();
    assert_eq!(b.len(), 2);
    assert_eq!(
        list_for_recipient(&s, PendingSpace::Personal, "rootC", 3000)
            .unwrap()
            .len(),
        1
    );
    // 组织空间独立于个人空间
    let org = list_for_recipient(&s, PendingSpace::Org("org_abc"), "rootB", 3000).unwrap();
    assert!(org.is_empty());
}

/// TTL：过期记录不列出、入队时清理。
#[test]
fn ttl_eviction() {
    let mut s = MemoryStorage::new();
    let t0 = 1000;
    // 旧记录（超过 7 天）与新记录
    enqueue(
        &mut s,
        PendingSpace::Personal,
        "rootB",
        &record("rootB", "old", "chat", t0),
        NODE_A,
        t0,
    )
    .unwrap();
    let now = t0 + PENDING_TTL_MS + 1;
    enqueue(
        &mut s,
        PendingSpace::Personal,
        "rootB",
        &record("rootB", "new", "chat", now),
        NODE_A,
        now,
    )
    .unwrap();

    // 入队 new 时触发 prune，old 被清
    let list = list_for_recipient(&s, PendingSpace::Personal, "rootB", now).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].1.message_id, "new");
}

/// 单 recipient 容量：超 100 条淘汰最旧。
#[test]
fn per_recipient_cap_evicts_oldest() {
    let mut s = MemoryStorage::new();
    let start = 1000;
    // 入队 105 条（createdAt 递增，旧在前）
    for i in 0..(PER_RECIPIENT_PENDING_CAP as i64 + 5) {
        enqueue(
            &mut s,
            PendingSpace::Personal,
            "rootB",
            &record("rootB", &format!("m{i:03}"), "chat", start + i),
            NODE_A,
            start + i,
        )
        .unwrap();
    }
    let list = list_for_recipient(&s, PendingSpace::Personal, "rootB", start + 1000).unwrap();
    assert_eq!(list.len(), PER_RECIPIENT_PENDING_CAP);
    // 最旧的 5 条（m000..m004）被淘汰
    let ids: Vec<&str> = list.iter().map(|(_, r)| r.message_id.as_str()).collect();
    assert!(!ids.contains(&"m000"));
    assert!(!ids.contains(&"m004"));
    assert!(ids.contains(&"m005"));
    assert!(ids.contains(&"m104"));
}

/// 全局容量：跨 recipient 累计超 1000 条淘汰最旧（不分 recipient）。
#[test]
fn global_cap_evicts_oldest_across_recipients() {
    let mut s = MemoryStorage::new();
    let start = 1000;
    // 12 个 recipient × 90 条 = 1080 条（> 1000 全局）
    for r in 0..12u32 {
        for i in 0..90u32 {
            let to = format!("root-{r}");
            let created = start + (r * 90 + i) as i64;
            enqueue(
                &mut s,
                PendingSpace::Personal,
                &to,
                &record(&to, &format!("m{i:03}"), "chat", created),
                NODE_A,
                created,
            )
            .unwrap();
        }
    }
    // 全局扫描（个人空间前缀）应 ≤ 1000
    let all = crate::storage::ScanOptions::prefix(PENDING_PREFIX);
    let count = s.scan(&all).unwrap().len();
    assert_eq!(count, GLOBAL_PENDING_CAP, "全局 cap 1000");
    // 最旧的（root-0 的 m000..）被淘汰：root-0 只剩 ~10 条
    let r0 = list_for_recipient(&s, PendingSpace::Personal, "root-0", start + 100000).unwrap();
    assert_eq!(r0.len(), 90 - 80, "root-0 最旧的 80 条被全局淘汰");
    // 最新的 root-11 全保留
    let r11 = list_for_recipient(&s, PendingSpace::Personal, "root-11", start + 100000).unwrap();
    assert_eq!(r11.len(), 90);
}

/// pdsync 自设备收敛：A 设备入队，经 apply_personal_remote 合入 B 设备。
#[test]
fn pending_converges_via_pdsync() {
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    let key = pending_key(PendingSpace::Personal, "rootB", "m1");
    let rec = record("rootB", "m1", "chat", 1000);
    enqueue(&mut a, PendingSpace::Personal, "rootB", &rec, NODE_A, 1000).unwrap();

    // A → B（pdsync data 合入）
    let meta_a = get_personal_meta(&a, &key).unwrap().unwrap();
    let value = serde_json::to_string(&rec).unwrap();
    let res = apply_personal_remote(&mut b, &key, &value, &meta_a).unwrap();
    assert_eq!(res.as_str(), "applied");
    assert_eq!(
        list_for_recipient(&b, PendingSpace::Personal, "rootB", 3000).unwrap()[0].1,
        rec
    );
}

/// 组织空间按个人空间同构落地（键带 orgId），但暂不经 pdsync。
#[test]
fn org_space_islanded() {
    let mut s = MemoryStorage::new();
    let rec = record("rootB", "m1", "chat", 1000);
    enqueue(
        &mut s,
        PendingSpace::Org("org_abc"),
        "rootB",
        &rec,
        NODE_A,
        1000,
    )
    .unwrap();
    let list = list_for_recipient(&s, PendingSpace::Org("org_abc"), "rootB", 3000).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "org:dm:pending:org_abc:rootB:m1");
    // 个人空间不受影响
    assert!(
        list_for_recipient(&s, PendingSpace::Personal, "rootB", 3000)
            .unwrap()
            .is_empty()
    );
    // 不同 orgId 隔离
    assert!(
        list_for_recipient(&s, PendingSpace::Org("org_xyz"), "rootB", 3000)
            .unwrap()
            .is_empty()
    );
}
