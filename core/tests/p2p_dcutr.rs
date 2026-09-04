//! dcutr 打洞冒烟（dcutr-hole-punch §3：loopback 三节点）：relay R + A/B
//! 各向 R 预约电路 → A 经电路地址连 B → dcutr 协商升级出直连——断言：
//! 直连地址（无 /p2p-circuit 段）在拨号侧邻居池记成功分（ConnectionEstablished
//! 既有记账路径）、relay 预约在升级后保留（回退保底不降级）、旧端（未挂
//! dcutr）保底不碍事（协商缺失即不尝试，电路中继照常）。

mod common;

use std::time::Duration;

use common::p2p::*;
use spark_core::p2p::overlay_store::OverlayPeerStore;

/// 带开关的节点启动（R 需 relay server 角色；A/B 纯客户端）。
async fn start_node_with(
    now_ms: i64,
    root_id: Option<&str>,
    enable_relay_server: bool,
    enable_dcutr: bool,
) -> (spark_core::p2p::P2pNode, std::sync::Arc<std::sync::Mutex<HostState>>, SharedStorage) {
    let storage = SharedStorage::new();
    let (host, state) = TestHost::new(root_id, storage.clone());
    let mut config = test_config(now_ms);
    config.enable_relay_server = enable_relay_server;
    config.enable_dcutr = enable_dcutr;
    // 关掉 kad：排除「identify 灌路由表 → kad 自动拨号」产生的直连干扰——
    // loopback 下 A 只能经 dcutr 的 identify 观察地址交换得知 B 的直连地址，
    // 直连成功记账成为升级的干净证据
    config.dht_mode = spark_core::p2p::DhtMode::Off;
    let node = spark_core::p2p::P2pNode::start(config, storage.clone(), Box::new(host))
        .await
        .expect("node starts");
    (node, state, storage)
}

/// 轮询 relay_status 直到谓词满足（预约/AutoNAT 判定均为异步）。
async fn wait_relay_status(
    node: &spark_core::p2p::P2pNode,
    timeout: Duration,
    what: &str,
    mut pred: impl FnMut(spark_core::p2p::LocalRelayStatus) -> bool,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = node.relay_status().await.expect("relay status");
        if pred(status) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 邻居池中该 peer 的记分卡（地址 → success_count）。
fn addr_success(storage: &SharedStorage, peer_id: &str) -> Vec<(String, u32)> {
    let mut guard = storage.0.lock().unwrap();
    let mut store = OverlayPeerStore::new(&mut *guard);
    store
        .get(peer_id)
        .ok()
        .flatten()
        .map(|r| {
            r.addr_meta
                .iter()
                .map(|(a, m)| (a.clone(), m.success_count))
                .collect()
        })
        .unwrap_or_default()
}

/// 电路地址（relay 完整传输地址 + /p2p/R/p2p-circuit + /p2p/B）。
fn circuit_addr(relay_addr: &str, relay_peer: &str, target_peer: &str) -> String {
    format!("{relay_addr}/p2p/{relay_peer}/p2p-circuit/p2p/{target_peer}")
}

/// loopback 打洞主用例：电路建立 → dcutr 升级出直连（邻居池直连地址记
/// 成功分）→ relay 预约保留。
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn dcutr_loopback_hole_punch_upgrades_to_direct() {
    let now = 1_720_000_000_000i64;
    let (mut r, _rs, _rstore) = start_node_with(now, None, true, true).await;
    let (a, _as_, storage_a) = start_node_with(now, None, false, true).await;
    let (b, _bs, storage_b) = start_node_with(now, None, false, true).await;
    let addrs_r = started_addresses(&mut r).await;
    let dialable_r = dialable(&addrs_r);
    let r_peer = r.peer_id().to_string();
    let b_peer = b.peer_id().to_string();
    let a_peer = a.peer_id().to_string();

    // A/B 直连 R（预约的前提：已连接 + hop 能力）
    connect(&a, &r_peer, &dialable_r).await;
    connect(&b, &r_peer, &dialable_r).await;

    // R 侧登记 external address（预约响应的地址来源——空集时客户端
    // NoAddressesInReservation，relay_manager.rs 实测注释；loopback 下
    // AutoNAT 不探测回环地址，走测试专用直登记，真机路径不变）
    r.__test_add_external_address(&dialable_r[0])
        .await
        .expect("register external addr");

    // A/B 各向 R 预约（maintain_tick → ensure_relay_reservations）
    a.maintain_tick().await.expect("tick a");
    b.maintain_tick().await.expect("tick b");
    wait_relay_status(&a, Duration::from_secs(30), "A 预约 R", |s| {
        !s.reservations.is_empty()
    })
    .await;
    wait_relay_status(&b, Duration::from_secs(30), "B 预约 R", |s| {
        !s.reservations.is_empty()
    })
    .await;

    // A 经电路地址连 B（identify 在电路连接上交换观察地址 → dcutr 协商）
    let circuit = circuit_addr(&dialable_r[0], &r_peer, &b_peer);
    connect(&a, &b_peer, &[circuit.clone()]).await;

    // dcutr 升级：出现对端的**直连**地址（无 /p2p-circuit 段）记成功分——
    // 记账走 ConnectionEstablished 既有 dialer 路径（哪侧先拨通记哪侧，
    // loopback 下双向都可能，两侧邻居池任一命中即证据成立）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut upgraded = false;
    while tokio::time::Instant::now() < deadline {
        let direct_hit = |storage: &SharedStorage, peer: &str| {
            addr_success(storage, peer)
                .iter()
                .any(|(addr, ok)| *ok > 0 && !addr.contains("/p2p-circuit"))
        };
        if direct_hit(&storage_a, &b_peer) || direct_hit(&storage_b, &a_peer) {
            upgraded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        upgraded,
        "dcutr 升级后应出现直连地址的成功记账（A/B 邻居池其一）"
    );

    // relay 预约保留（升直连不降级：直连可能随切网死亡，relay 是回退保底）
    let status_a = a.relay_status().await.expect("relay status a");
    assert!(
        !status_a.reservations.is_empty(),
        "升直连后 A 的 relay 预约保留"
    );

    r.stop().await;
    a.stop().await;
    b.stop().await;
}

/// 电路保底对照：电路连接照常建立 + 电路地址记成功分（中继保底可用）。
/// （旧端「不挂 dcutr」形态由 libp2p 协议协商保证——对端协议清单无
/// `/libp2p/dcutr` 即不发起升级；本仓全部节点已挂载，真实旧端矩阵留
/// 真机/联调，见设计 §3 末行。）
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn dcutr_legacy_peer_falls_back_to_circuit() {
    let now = 1_720_000_000_000i64;
    let (mut r, _rs, _rstore) = start_node_with(now, None, true, true).await;
    let (mut a, _as_, storage_a) = start_node_with(now, None, false, true).await;
    // B 为旧端形态（不挂 dcutr）
    let (b, _bs, _storage_b) = start_node_with(now, None, false, false).await;
    let addrs_r = started_addresses(&mut r).await;
    let dialable_r = dialable(&addrs_r);
    let r_peer = r.peer_id().to_string();
    let b_peer = b.peer_id().to_string();

    connect(&a, &r_peer, &dialable_r).await;
    connect(&b, &r_peer, &dialable_r).await;
    r.__test_add_external_address(&dialable_r[0])
        .await
        .expect("register external addr");
    a.maintain_tick().await.expect("tick a");
    b.maintain_tick().await.expect("tick b");
    wait_relay_status(&a, Duration::from_secs(30), "A 预约 R", |s| {
        !s.reservations.is_empty()
    })
    .await;
    wait_relay_status(&b, Duration::from_secs(30), "B 预约 R", |s| {
        !s.reservations.is_empty()
    })
    .await;

    // 电路连接照常建立（A 经电路连 B 成功）
    let circuit = circuit_addr(&dialable_r[0], &r_peer, &b_peer);
    connect(&a, &b_peer, &[circuit.clone()]).await;
    let hit = wait_for_timeout(&mut a, Duration::from_secs(10), |e| {
        matches!(e, spark_core::p2p::P2pEvent::PeerConnected { peer_id } if *peer_id == b_peer)
    })
    .await;
    assert!(hit, "电路连接建立（PeerConnected）");
    // 旧端不升级：无直连地址的成功记账（给协商留 3s 窗；邻居池内该 peer
    // 的直连形态地址 success 恒 0——升级成功的直连连接会在 dialer 侧记
    // 成功分，见主用例）
    tokio::time::sleep(Duration::from_secs(3)).await;
    let scores = addr_success(&storage_a, &b_peer);
    assert!(
        scores
            .iter()
            .all(|(addr, ok)| *ok == 0 || addr.contains("/p2p-circuit")),
        "旧端无直连升级记账，实际记分卡={scores:?}"
    );
    // relay 预约保留（电路中继保底）
    let status_a = a.relay_status().await.expect("relay status a");
    assert!(!status_a.reservations.is_empty(), "A 的 relay 预约保留");

    r.stop().await;
    a.stop().await;
    b.stop().await;
}
