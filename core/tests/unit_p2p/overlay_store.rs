//! 覆盖网邻居记录存储单测。

use std::collections::HashSet;

use spark_core::p2p::overlay_store::*;
use spark_core::storage::MemoryStorage;

fn addr(i: usize) -> Vec<String> {
    vec![format!("/ip4/10.0.0.{i}/tcp/15002/ws")]
}

/// 自过滤上下文：None + 空集合（测试默认不做过滤）。
fn self_ctx() -> (Option<&'static str>, HashSet<String>) {
    (None, HashSet::new())
}

#[test]
fn remember_merges_and_caps_addresses() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    store
        .remember(
            "p1",
            &addr(1),
            OverlayPeerSource::Connect,
            false,
            100,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "p1",
            &addr(2),
            OverlayPeerSource::Exchange,
            false,
            200,
            sid,
            &saddrs,
        )
        .unwrap();
    let rec = store.get("p1").unwrap().unwrap();
    assert_eq!(rec.first_seen_at, 100);
    assert_eq!(rec.last_seen_at, 200);
    assert_eq!(rec.addresses.len(), 2);
    assert_eq!(rec.source, OverlayPeerSource::Exchange);
    // 重复地址去重
    store
        .remember(
            "p1",
            &addr(1),
            OverlayPeerSource::Connect,
            false,
            300,
            sid,
            &saddrs,
        )
        .unwrap();
    assert_eq!(store.get("p1").unwrap().unwrap().addresses.len(), 2);
}

#[test]
fn verified_is_sticky() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    store
        .remember(
            "p1",
            &addr(1),
            OverlayPeerSource::Announce,
            true,
            100,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "p1",
            &addr(2),
            OverlayPeerSource::Exchange,
            false,
            200,
            sid,
            &saddrs,
        )
        .unwrap();
    assert!(store.get("p1").unwrap().unwrap().verified);
}

#[test]
fn sample_prefers_verified_then_recency() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    store
        .remember(
            "old-verified",
            &addr(1),
            OverlayPeerSource::Announce,
            true,
            100,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "new-unverified",
            &addr(2),
            OverlayPeerSource::Exchange,
            false,
            900,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "old-unverified",
            &addr(3),
            OverlayPeerSource::Exchange,
            false,
            200,
            sid,
            &saddrs,
        )
        .unwrap();
    let now = 1_000_000i64;
    let sample = store
        .sample_dial_candidates(&HashSet::new(), now, 10)
        .unwrap();
    let order: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
    assert_eq!(
        order,
        vec!["old-verified", "new-unverified", "old-unverified"]
    );
    // 排除 + 无地址条目
    store
        .remember(
            "no-addr",
            &[],
            OverlayPeerSource::Exchange,
            false,
            1000,
            sid,
            &saddrs,
        )
        .unwrap();
    let sample = store
        .sample_dial_candidates(&HashSet::from(["old-verified".to_string()]), now, 10)
        .unwrap();
    let order: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
    assert_eq!(order, vec!["new-unverified", "old-unverified"]);
}

#[test]
fn dial_sample_filters_stale_beyond_freshness_window() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    let now = 1_000_000i64;
    store
        .remember(
            "fresh",
            &addr(1),
            OverlayPeerSource::Connect,
            false,
            now - 1000,
            sid,
            &saddrs,
        )
        .unwrap();
    // 超 24h 窗口（connection-policy M7：IPv6 前缀/DHCP 复用尸体地址）的陈旧候选
    store
        .remember(
            "stale",
            &addr(2),
            OverlayPeerSource::Connect,
            false,
            now - 25 * 60 * 60 * 1000,
            sid,
            &saddrs,
        )
        .unwrap();
    let sample = store
        .sample_dial_candidates(&HashSet::new(), now, 10)
        .unwrap();
    let ids: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
    assert_eq!(ids, vec!["fresh"], "超新鲜度窗口的候选应被滤除");
}

#[test]
fn dial_sample_drops_candidates_with_only_non_dialable_addresses() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    let now = 1_000_000i64;
    // 通配地址（本机监听 0.0.0.0 形态）不可拨：对齐 filter_dial_candidate，
    // 复用共享地址解析过滤确保本机通配监听不被当候选。
    store
        .remember(
            "wildcard",
            &["/ip4/0.0.0.0/tcp/15002".to_string()],
            OverlayPeerSource::Announce,
            true,
            now,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "concrete",
            &addr(1),
            OverlayPeerSource::Connect,
            false,
            now,
            sid,
            &saddrs,
        )
        .unwrap();
    let sample = store
        .sample_dial_candidates(&HashSet::new(), now, 10)
        .unwrap();
    let ids: Vec<&str> = sample.iter().map(|r| r.peer_id.as_str()).collect();
    assert_eq!(ids, vec!["concrete"], "仅含不可拨通配地址的候选应被剔除");
}

#[test]
fn exchange_sample_respects_age_window() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    let now = 1_000_000i64;
    store
        .remember(
            "fresh",
            &addr(1),
            OverlayPeerSource::Connect,
            false,
            now - 1000,
            sid,
            &saddrs,
        )
        .unwrap();
    store
        .remember(
            "stale",
            &addr(2),
            OverlayPeerSource::Connect,
            false,
            now - 15 * 24 * 60 * 60 * 1000,
            sid,
            &saddrs,
        )
        .unwrap();
    let sample = store
        .sample_for_exchange(Some("fresh"), 16, now, 14 * 24 * 60 * 60 * 1000)
        .unwrap();
    assert!(sample.is_empty()); // fresh 被排除（请求方），stale 超窗
}

#[test]
fn eviction_drops_unverified_first() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    // 填满 200：199 个未验证 + 1 个最旧的已验证
    store
        .remember(
            "verified-oldest",
            &addr(0),
            OverlayPeerSource::Announce,
            true,
            1,
            sid,
            &saddrs,
        )
        .unwrap();
    for i in 1..200usize {
        store
            .remember(
                &format!("peer-{i}"),
                &addr(i),
                OverlayPeerSource::Exchange,
                false,
                (i * 10) as i64,
                sid,
                &saddrs,
            )
            .unwrap();
    }
    assert_eq!(store.list_all().unwrap().len(), 200);
    // 再插一个 → 淘汰最久未见的未验证（peer-1，lastSeenAt=10）
    store
        .remember(
            "newcomer",
            &addr(999),
            OverlayPeerSource::Exchange,
            false,
            1_000_000,
            sid,
            &saddrs,
        )
        .unwrap();
    let all = store.list_all().unwrap();
    assert_eq!(all.len(), 200);
    assert!(all.iter().any(|r| r.peer_id == "newcomer"));
    assert!(all.iter().any(|r| r.peer_id == "verified-oldest"));
    assert!(!all.iter().any(|r| r.peer_id == "peer-1"));
}

#[test]
fn eviction_falls_back_to_verified_when_all_verified() {
    let mut storage = MemoryStorage::new();
    let mut store = OverlayPeerStore::new(&mut storage);
    let (sid, saddrs) = self_ctx();
    for i in 0..201usize {
        store
            .remember(
                &format!("peer-{i:03}"),
                &addr(i),
                OverlayPeerSource::Announce,
                true,
                (i * 10) as i64,
                sid,
                &saddrs,
            )
            .unwrap();
    }
    let all = store.list_all().unwrap();
    assert_eq!(all.len(), 200);
    assert!(!all.iter().any(|r| r.peer_id == "peer-000")); // 最旧被淘汰
}
