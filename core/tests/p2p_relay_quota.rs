//! relay 配额超限拒绝（relay-strategy §1「有配额」；network §六「配额超限
//! 拒绝」单测）：R 预约名额压到 1 模拟满载——A 先得预约、B 持续重试均被拒
//! （预约恒空、失败沉默）；A 释放名额后 B 补位成功（拒绝是配额所致而非配置
//! 错误；换备零成本——relay 无状态）。
//!
//! 时限（2h）/字节量（256MiB）配额的接线断言见
//! `p2p/behaviour.rs::tests::relay_server_config_wires_quota_constants`；
//! 执行在 libp2p relay server 内部（既有能力，零自研）。

mod common;

use std::time::Duration;

use common::p2p::*;

/// 轮询 relay_status 直到谓词满足（与 p2p_dcutr 同款节奏）。
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

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn relay_reservation_quota_exceeded_denies_latecomer() {
    let now = 1_720_000_000_000i64;
    // R：relay server，预约名额压到 1（满载模拟）
    let storage_r = SharedStorage::new();
    let (host_r, _rs) = TestHost::new(None, storage_r.clone());
    let mut config_r = test_config(now);
    config_r.enable_relay_server = true;
    config_r.relay_max_reservations = Some(1);
    let mut r = spark_core::p2p::P2pNode::start(config_r, storage_r.clone(), Box::new(host_r))
        .await
        .expect("R starts");
    let (a, _as_, _sa) = start_node(now, None).await;
    let (b, _bs, _sb) = start_node(now, None).await;
    let addrs_r = started_addresses(&mut r).await;
    let dialable_r = dialable(&addrs_r);
    let r_peer = r.peer_id().to_string();

    connect(&a, &r_peer, &dialable_r).await;
    connect(&b, &r_peer, &dialable_r).await;
    // 预约响应的地址来源（dcutr 测试同口径；loopback 下走测试专用直登记）
    r.__test_add_external_address(&dialable_r[0])
        .await
        .expect("register external addr");

    // A 先得预约（占满名额 1）
    a.maintain_tick().await.expect("tick a");
    wait_relay_status(&a, Duration::from_secs(30), "A 预约 R", |s| {
        !s.reservations.is_empty()
    })
    .await;

    // B 持续重试一个窗口：名额满载，每次预约请求都被拒——预约恒空
    // （拒绝由 transport 关闭电路监听上行，客户端清理后沉默重试，不刷屏）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        b.maintain_tick().await.expect("tick b");
        let st = b.relay_status().await.expect("relay status b");
        assert!(
            st.reservations.is_empty(),
            "名额满载下 B 的预约必须被拒，实际: {:?}",
            st.reservations
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // A 的既有预约不受 B 重试影响（配额拒绝不波及已占用者）
    let st_a = a.relay_status().await.expect("relay status a");
    assert!(
        !st_a.reservations.is_empty(),
        "A 的既有预约保持，实际: {:?}",
        st_a.reservations
    );

    // A 释放（连接断开 → R 侧名额回收）后 B 补位成功：拒绝确为配额所致；
    // 换备零成本（relay 无状态，下轮重试即入）
    a.stop().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        b.maintain_tick().await.expect("tick b");
        let st = b.relay_status().await.expect("relay status b");
        if !st.reservations.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "A 释放名额后 B 应补位成功"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    r.stop().await;
    b.stop().await;
}
