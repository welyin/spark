//! 内容面集成测试（本机 loopback 真实 libp2p 节点）：
//! blob 内容寻址存储 ↔ Kad provider「持有即做种」最小路径、按 CID 经
//! `/spark/blob-fetch/1.0.0` 直连拉回本体（含接收侧哈希校验负例）、leaf 模式
//! 与非法 CID 负例、公告指针线形的 CID 衔接（public-topics §七 /
//! community-affairs 内容面 / social-feed §7.3）。

mod common;

use std::time::Duration;

use spark_core::content::{self, Cid};
use spark_core::p2p::{P2pEvent, P2pNode};
use spark_core::storage::StorageBackend as _;

use common::p2p::*;

/// 持有即做种全路径：A 存 blob + 打根标记 + provide → B 经 Kad 检索到
/// provider（A 的 peerId）→ A 停止做种（幂等）。衔接点：公告 payload 中的
/// `{"$blob": cid}` 引用经 extract_blob_cids 提取后正是被 provide 的 CID。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_provide_and_find_providers_loopback() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, mut storage_a) = start_node(now, None).await;
    let (mut b, _state_b, _storage_b) = start_node(now, None).await;
    let addrs_a = started_addresses(&mut a).await;
    let _ = started_addresses(&mut b).await;
    let a_peer = a.peer_id().to_string();

    // B 连接 A（kad 路由可得；A 的 start_providing 需要已知路由节点）
    connect(&b, &a_peer, &dialable(&addrs_a)).await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 内容面存储：按 CID 存取 + 完整性校验 + GC 根标记
    let data = b"topic file content v1".repeat(100);
    let info = content::save_blob(&mut storage_a, &data).expect("save blob");
    let cid = info.cid.clone();
    assert_eq!(info.size, data.len() as u64);
    content::pin_root(&mut storage_a, &cid, "topic:demo").expect("pin root");
    assert_eq!(
        content::read_blob(&storage_a, &cid).expect("read").as_deref(),
        Some(data.as_slice()),
        "读出字节与写入一致（完整性校验通过）"
    );

    // 衔接点：公告指针线形的 payload 引用该 blob → 提取出的 CID 与存取一致
    let announce_payload = serde_json::json!({
        "kind": "topic-file",
        "title": "示例议题附件",
        "file": { "$blob": cid.as_str(), "name": "data.bin", "size": info.size }
    });
    let cids = content::extract_blob_cids(&announce_payload);
    assert_eq!(cids, vec![cid.clone()], "公告 payload 引用提取出同一 CID");

    // 持有即做种：A 声明 provider（幂等）
    a.provide_blob(cid.as_str()).await.expect("provide ok");
    a.provide_blob(cid.as_str())
        .await
        .expect("provide idempotent");

    // B 检索：provider 集合应含 A 的 peerId（轮询等 kad 传播）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let providers = loop {
        let providers = b
            .find_blob_providers(cid.as_str())
            .await
            .expect("find providers ok");
        if providers.contains(&a_peer) {
            break providers;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "blob provider not visible in time"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert!(providers.contains(&a_peer));

    // 退出议题 / 不再持有：停止做种（幂等，未提供也是空操作）
    a.stop_providing_blob(cid.as_str())
        .await
        .expect("stop provide ok");
    a.stop_providing_blob(cid.as_str())
        .await
        .expect("stop provide idempotent");
    a.stop_providing_blob(&Cid::from_data(b"never provided").as_str())
        .await
        .expect("stop never-provided is no-op");

    a.stop().await;
    b.stop().await;
}

/// leaf 模式（移动端）：持有副本但不服务——provide 报错，检索照常（只消费）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_provide_rejected_in_leaf_mode() {
    let now = 1_720_000_000_000i64;
    let mut config = test_config(now);
    config.leaf_mode = true;
    let storage = SharedStorage::new();
    let (host, _state) = TestHost::new(None, storage.clone());
    let mut node = P2pNode::start(config, storage, Box::new(host))
        .await
        .expect("node starts");
    let _ = started_addresses(&mut node).await;

    let cid = Cid::from_data(b"leaf held blob");
    let err = node.provide_blob(cid.as_str()).await.unwrap_err();
    assert!(err.to_string().contains("leaf mode"), "got: {err}");

    // 检索不受限（叶子只消费不服务）：空结果也是正常应答
    let providers = node
        .find_blob_providers(cid.as_str())
        .await
        .expect("leaf can query providers");
    assert!(providers.is_empty(), "无持有者可见时为空列表");

    // 停止做种在 leaf 下同样是幂等空操作
    node.stop_providing_blob(cid.as_str())
        .await
        .expect("stop provide no-op in leaf mode");

    node.stop().await;
}

/// 非法 CID 在 API 边界即拒绝（不进事件循环）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_commands_reject_malformed_cid() {
    let now = 1_720_000_000_000i64;
    let (mut node, _state, _storage) = start_node(now, None).await;
    let _ = started_addresses(&mut node).await;

    for bad in ["not-a-cid", "ABC", &"A".repeat(64), &"g".repeat(64)] {
        let err = node.provide_blob(bad).await.unwrap_err();
        assert!(err.to_string().contains("invalid cid"), "got: {err}");
        let err = node.find_blob_providers(bad).await.unwrap_err();
        assert!(err.to_string().contains("invalid cid"), "got: {err}");
        let err = node.stop_providing_blob(bad).await.unwrap_err();
        assert!(err.to_string().contains("invalid cid"), "got: {err}");
    }

    node.stop().await;
}

/// dht_mode = Off：blob provide / 检索均报 dht disabled（与 dht 线形同口径）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_commands_disabled_when_dht_off() {
    let now = 1_720_000_000_000i64;
    let mut config = test_config(now);
    config.dht_mode = spark_core::p2p::DhtMode::Off;
    let storage = SharedStorage::new();
    let (host, _state) = TestHost::new(None, storage.clone());
    let mut node = P2pNode::start(config, storage, Box::new(host))
        .await
        .expect("node starts");
    let _ = started_addresses(&mut node).await;

    let cid = Cid::from_data(b"off mode blob");
    let err = node.provide_blob(cid.as_str()).await.unwrap_err();
    assert!(err.to_string().contains("dht disabled"), "got: {err}");
    let err = node.find_blob_providers(cid.as_str()).await.unwrap_err();
    assert!(err.to_string().contains("dht disabled"), "got: {err}");

    node.stop().await;
}

/// 按 CID 拉取本体全路径（持有即做种传输协议）：A 存 blob + provide →
/// B 经 Kad 找到 provider → `/spark/blob-fetch/1.0.0` 拉回本体 → 落库字节
/// 与源一致（接收侧哈希校验通过）→ B 自动成为 provider（A 随后能检索到 B）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_fetch_loopback_and_auto_provide() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, mut storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_a = started_addresses(&mut a).await;
    let _ = started_addresses(&mut b).await;
    let a_peer = a.peer_id().to_string();
    let b_peer = b.peer_id().to_string();

    connect(&b, &a_peer, &dialable(&addrs_a)).await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // A 持有并做种；B 本地无此 blob
    let data = b"fetch me across the wire".repeat(200);
    let info = content::save_blob(&mut storage_a, &data).expect("save blob");
    let cid = info.cid.clone();
    content::pin_root(&mut storage_a, &cid, "topic:fetch-demo").expect("pin root");
    a.provide_blob(cid.as_str()).await.expect("provide ok");
    assert!(
        !content::has_blob(&storage_b, &cid),
        "B 起始不持有该 blob"
    );

    // B 经 Kad 检索到 provider（轮询等传播）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let providers = b
            .find_blob_providers(cid.as_str())
            .await
            .expect("find providers ok");
        if providers.contains(&a_peer) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "blob provider not visible in time"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // B 按 CID 向 A 拉取本体（双方已连接）：响应校验 + 落库在事件循环内完成
    let fetched = b
        .fetch_blob(&a_peer, cid.as_str())
        .await
        .expect("fetch blob ok");
    assert_eq!(fetched.cid, cid);
    assert_eq!(fetched.size, data.len() as u64);
    assert_eq!(
        content::read_blob(&storage_b, &cid)
            .expect("read")
            .as_deref(),
        Some(data.as_slice()),
        "拉回的字节与源一致（重算哈希匹配 CID）"
    );

    // 持有即做种：B 拉到副本即自动登记 provider，A 随后能检索到 B
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let providers = a
            .find_blob_providers(cid.as_str())
            .await
            .expect("find providers ok");
        if providers.contains(&b_peer) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fetcher did not auto-provide in time"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    a.stop().await;
    b.stop().await;
}

/// 负例：provider 存盘损坏（内容与 CID 不符）时，应答侧读出重算校验失败 →
/// 回 integrity-error 拒绝服务（绝不发出与 CID 不符的字节）；请求侧报错
/// 收敛，不落库也不做种。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_fetch_rejects_integrity_error() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, mut storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_a = started_addresses(&mut a).await;
    let _ = started_addresses(&mut b).await;
    let a_peer = a.peer_id().to_string();

    connect(&b, &a_peer, &dialable(&addrs_a)).await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // A 存 blob 后篡改存盘字节（CID 键不变、内容换掉），并照常做种
    let data = b"original bytes".repeat(50);
    let info = content::save_blob(&mut storage_a, &data).expect("save blob");
    let cid = info.cid.clone();
    use base64::Engine as _;
    let tampered = base64::engine::general_purpose::STANDARD.encode(b"tampered bytes");
    storage_a
        .put(
            &format!("{}{cid}", content::store::BLOB_DATA_PREFIX),
            &tampered,
        )
        .expect("tamper store");
    a.provide_blob(cid.as_str()).await.expect("provide ok");

    // B 拉取：应答侧完整性校验拒绝服务，请求侧报错收敛
    let err = b
        .fetch_blob(&a_peer, cid.as_str())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("integrity-error"),
        "got: {err}"
    );
    assert!(
        !content::has_blob(&storage_b, &cid),
        "校验失败不落库（B 不持有损坏副本）"
    );

    a.stop().await;
    b.stop().await;
}
