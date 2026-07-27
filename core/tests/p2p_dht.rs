//! p2p DHT 集成测试（本机 loopback 双/三真实 libp2p 节点）：公共 Kad 节点
//! 存在记录与组织地址记录（含负例）、组织私有 DHT 网关代理与成员提示回填、
//! dht_mode = Off 负例。

mod common;

use std::time::Duration;

use spark_core::org::gateway::{OrgMemberHint, org_members_dht_key};
use spark_core::org::{
    ORG_ADDRESS_RECORD_DEFAULT_TTL_MS, decode_org_address, org_address_cache_key,
    sign_org_address_record, verify_org_address_record,
};
use spark_core::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use spark_core::p2p::{P2pEvent, P2pNode, node_presence_record_key};
use spark_core::storage::StorageBackend;

use common::p2p::*;

// ---------------------------------------------------------------------------
// Kad（公共 DHT）
// ---------------------------------------------------------------------------

/// Kad：A 发布节点存在记录 → B 查询命中 → 三层确认通过 →
/// A 在 B 邻居池中的口径翻为 Exchange（未验证，区别于直连沉淀的 Connect）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kad_node_presence_put_get_and_confirm() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let addrs_a = started_addresses(&mut a).await;
    let a_peer = a.peer_id().to_string();

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let record_value = sign_presence_record(&storage_a, &a_peer, &dialable(&addrs_a), now);
    let key = node_presence_record_key(&a_peer);
    a.dht_put_record(key.as_bytes(), record_value.clone())
        .await
        .expect("dht put ok");

    // B 查询命中原始记录；每次命中都触发一次三层确认（identify 协议清单可能晚到，轮询重试）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut confirmed = false;
    while tokio::time::Instant::now() < deadline {
        let found = b.dht_get_record(key.as_bytes()).await.expect("dht get ok");
        assert_eq!(
            found.as_deref(),
            Some(record_value.as_slice()),
            "记录内容原样返回"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        let entry = {
            let mut guard = storage_b.0.lock().unwrap();
            let mut store = OverlayPeerStore::new(&mut *guard);
            store
                .get(&a_peer)
                .ok()
                .flatten()
                .map(|r| (r.source, r.verified))
        };
        if let Some((OverlayPeerSource::Exchange, false)) = entry {
            confirmed = true;
            break;
        }
    }
    assert!(confirmed, "三层确认通过后 A 应按未验证 Exchange 口径入池");

    a.stop().await;
    b.stop().await;
}

/// Kad 负例：签名与 peerId 不匹配的记录——DHT 原始检索仍返回内容，
/// 但三层确认第①层拒绝，不进邻居池。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kad_record_with_mismatched_signature_rejected() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;
    let _ = started_addresses(&mut a).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    // 伪造：用 A 的私钥签、peerId 写成一个不存在节点的（签名与 peerId 不匹配）
    let fake_keypair = libp2p::identity::Keypair::generate_ed25519();
    let fake_peer = libp2p::PeerId::from_public_key(&fake_keypair.public()).to_base58();
    let record_value = sign_presence_record(
        &storage_a,
        &fake_peer,
        &["/ip4/10.6.6.6/tcp/15002".to_string()],
        now,
    );
    let key = node_presence_record_key(&fake_peer);
    a.dht_put_record(key.as_bytes(), record_value.clone())
        .await
        .expect("dht put ok");

    let found = b.dht_get_record(key.as_bytes()).await.expect("dht get ok");
    assert!(found.is_some(), "DHT 原始检索不过滤内容");

    // 给确认链路留出时间后断言：伪造 peer 未入池
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let mut guard = storage_b.0.lock().unwrap();
    let mut store = OverlayPeerStore::new(&mut *guard);
    assert!(
        store.get(&fake_peer).ok().flatten().is_none(),
        "签名错配的记录不得入邻居池"
    );

    a.stop().await;
    b.stop().await;
}

/// dht_mode = Off：不挂 Kad，DHT 命令报 dht disabled。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dht_disabled_when_mode_off() {
    let now = 1_720_000_000_000i64;
    let mut config = test_config(now);
    config.dht_mode = spark_core::p2p::DhtMode::Off;
    let storage = SharedStorage::new();
    let (host, _state) = TestHost::new(None, storage.clone());
    let mut node = P2pNode::start(config, storage, Box::new(host))
        .await
        .expect("node starts");
    let _ = started_addresses(&mut node).await;

    let err = node.dht_get_record(b"any-key").await.unwrap_err();
    assert!(err.to_string().contains("dht disabled"), "got: {err}");
    let err = node.dht_put_record(b"k", b"v".to_vec()).await.unwrap_err();
    assert!(err.to_string().contains("dht disabled"), "got: {err}");

    node.stop().await;
}

// ---------------------------------------------------------------------------
// 组织级私有 DHT（网关代理，p2p-messages.md §15 / org.md §13-14）
// ---------------------------------------------------------------------------

/// 三节点：A = 网关节点在 orgSecret 派生 key 上 providing + 发布成员提示；
/// B = 普通成员查询命中 → {peerId, addresses} 提示经宿主回调按未验证口径入池；
/// C = 非成员，无 orgSecret 无法计算 key（构造性验证：错误 key 查询不命中）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_private_dht_gateway_provide_and_member_lookup() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _s_a) = start_node(now, None).await; // 网关
    let (mut b, state_b, storage_b) = start_node(now, None).await; // 成员
    let (mut c, _state_c, storage_c) = start_node(now, None).await; // 非成员
    let addrs_a = started_addresses(&mut a).await;
    let addrs_b = started_addresses(&mut b).await;
    let addrs_c = started_addresses(&mut c).await;
    let a_peer = a.peer_id().to_string();

    // B/C 都连接 A（kad 路由可得）
    connect(&b, &a_peer, &dialable(&addrs_a)).await;
    connect(&c, &a_peer, &dialable(&addrs_a)).await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    let _ = (addrs_b, addrs_c);

    // 网关职责：orgSecret 派生 key → start_providing + 发布 {peerId, addresses} 提示
    let org_secret = "ab".repeat(32);
    let key = org_members_dht_key(&org_secret);
    let hint = OrgMemberHint {
        peer_id: a_peer.clone(),
        addresses: dialable(&addrs_a),
    };
    let value = hint.to_record_value();
    a.dht_provide_record(key.as_bytes(), value.clone())
        .await
        .expect("dht provide ok");
    // 幂等：相同 (key, value) 重复调用为空操作
    a.dht_provide_record(key.as_bytes(), value.clone())
        .await
        .expect("dht provide idempotent");

    // 成员 B：向该 key 查记录 → 命中网关提示（内容恰为 {peerId, addresses}，无组织语义）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let found = loop {
        if let Some(found) = b.dht_get_record(key.as_bytes()).await.expect("dht get ok") {
            break found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "member lookup not found in time"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(found, value, "提示记录原样返回");
    let parsed = OrgMemberHint::from_record_value(&found).expect("hint parses");
    assert_eq!(parsed.peer_id, a_peer);
    assert!(!parsed.addresses.is_empty());

    // provider 查询：A 在该 key 上注册为 provider（§15 start_providing 生效）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let providers = loop {
        let providers = b
            .dht_get_providers(key.as_bytes())
            .await
            .expect("get providers ok");
        if !providers.is_empty() {
            break providers;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "provider not visible in time"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert!(
        providers.contains(&a_peer),
        "网关应注册为 provider: {providers:?}"
    );

    // 宿主回调回填：B 收到提示且按未验证口径入邻居池（信任边界不变）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let hints = state_b.lock().unwrap().org_member_hints.clone();
        if hints.iter().any(|h| h.peer_id == a_peer) {
            break;
        }
        // 回调由 B 的 get_record 命中触发；再查一次兜底（记录已在 B 本地缓存，直接命中）
        let _ = b.dht_get_record(key.as_bytes()).await;
        assert!(
            tokio::time::Instant::now() < deadline,
            "org member hint not delivered to host"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    {
        let mut guard = storage_b.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        let record = store.get(&a_peer).unwrap().expect("A 应在 B 邻居池");
        assert!(!record.verified, "DHT 线索一律未验证入池");
        assert_eq!(record.source, OverlayPeerSource::Exchange);
    }

    // 非成员 C：无 orgSecret → 无法计算 key（构造性验证）：
    // ① 其他 secret 派生的 key 与该组织 key 不同；② 未知 key 查询不命中、不入池
    let wrong_key = org_members_dht_key(&"cd".repeat(32));
    assert_ne!(wrong_key, key, "非持密者无法派生同一 key");
    let found = c
        .dht_get_record(wrong_key.as_bytes())
        .await
        .expect("dht get ok");
    assert!(found.is_none(), "未知 key 不可命中");
    let providers = c
        .dht_get_providers(wrong_key.as_bytes())
        .await
        .expect("get providers ok");
    assert!(providers.is_empty(), "未知 key 无 provider 可枚举");
    {
        let mut guard = storage_c.0.lock().unwrap();
        let mut store = OverlayPeerStore::new(&mut *guard);
        // 「不入池」= 无提示类线索（Exchange/Announce 等）；Connect 来源条目来自
        // kad 后台发现，与查询无关，不计入
        let hinted: Vec<String> = store
            .list_all()
            .expect("list pool ok")
            .into_iter()
            .filter(|r| r.source != OverlayPeerSource::Connect)
            .map(|r| r.peer_id)
            .collect();
        assert!(
            hinted.is_empty(),
            "未知 key 查询不得向非成员邻居池写入提示类线索: {hinted:?}"
        );
    }

    a.stop().await;
    b.stop().await;
    c.stop().await;
}

/// Kad：A 发布组织地址记录（§16，key = sha256(orgPublicKey) 原始字节）→
/// B 查询命中、内容原样、五步校验通过 → B 本地沉淀 `p2p:org-address:` 缓存。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kad_org_address_record_put_get_and_cache() {
    let now = 1_720_000_000_000i64;
    let (mut a, _state_a, _storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;
    wait_for(&mut a, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let record = sign_org_address_record(
        &signing_key,
        "org_0123456789abcdef",
        Some("星火公开组织".to_string()),
        vec!["aa".repeat(32)],
        1,
        now,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    let record_value = record.to_record_value();
    let key = decode_org_address(&record.org_address).expect("address decodes");
    a.dht_put_record(&key, record_value.clone())
        .await
        .expect("dht put ok");

    let found = b.dht_get_record(&key).await.expect("dht get ok");
    assert_eq!(
        found.as_deref(),
        Some(record_value.as_slice()),
        "记录内容原样返回"
    );
    assert!(
        verify_org_address_record(&record, now).is_ok(),
        "命中记录五步校验通过"
    );

    // B 侧 DHT 命中后应沉淀本地缓存（resolve_dht_get 的缓存路径；轮询等事件泵落盘）
    let cache_key = org_address_cache_key(&record.org_address);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let cached = loop {
        let value = storage_b.get(&cache_key).expect("storage get");
        if value.is_some() {
            break value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "组织地址记录未沉淀本地缓存"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let cached_record: spark_core::org::OrgAddressRecord =
        serde_json::from_str(&cached.unwrap()).expect("cached record parses");
    assert_eq!(cached_record, record, "缓存内容与发布记录一致");

    a.stop().await;
    b.stop().await;
}

/// Kad 负例：篡改 displayName 的组织地址记录——DHT 原始检索仍返回内容，
/// 但五步校验失败，不沉淀本地缓存。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kad_org_address_record_tampered_not_cached() {
    let now = 1_720_000_000_000i64;
    let (a, _state_a, _storage_a) = start_node(now, None).await;
    let (mut b, _state_b, storage_b) = start_node(now, None).await;
    let addrs_b = started_addresses(&mut b).await;

    connect(&a, b.peer_id(), &dialable(&addrs_b)).await;
    wait_for(&mut b, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let mut record = sign_org_address_record(
        &signing_key,
        "org_0123456789abcdef",
        Some("星火公开组织".to_string()),
        vec!["aa".repeat(32)],
        1,
        now,
        ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
    );
    record.display_name = Some("篡改的展示名".to_string());
    assert!(
        !verify_org_address_record(&record, now).is_ok(),
        "篡改后验签必须失败"
    );
    let key = decode_org_address(&record.org_address).expect("address decodes");
    a.dht_put_record(&key, record.to_record_value())
        .await
        .expect("dht put ok");

    let found = b.dht_get_record(&key).await.expect("dht get ok");
    assert!(found.is_some(), "DHT 原始检索不过滤内容");

    // 给缓存路径留出时间后断言：校验失败的记录不得沉淀
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let cache_key = org_address_cache_key(&record.org_address);
    assert!(
        storage_b.get(&cache_key).expect("storage get").is_none(),
        "验签失败的记录不得沉淀缓存"
    );

    a.stop().await;
    b.stop().await;
}
