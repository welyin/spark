//! EventLoop 拨号状态机测试（不依赖真实网络：构造 `EventLoop` 后直接喂
//! `SwarmEvent` 断言 pending 状态迁移；`swarm.dial` 只同步发起异步拨号，
//! 不 poll 即不产生真实 IO）。
//!
//! 覆盖：同地址去重等待者随连接建立被服务、OutgoingConnectionError 按
//! ConnectionId 精确归属、拨号方耗尽唤醒等待者、connect 匹配与调用方
//! 放弃后的滞留惰性回收。

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use libp2p::core::{ConnectedPoint, Endpoint};
use libp2p::swarm::{ConnectionId, DialError, SwarmEvent};
use libp2p::{Multiaddr, PeerId};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::api::Command;
use super::event_loop::{EventLoop, OrgAttempt, OrgAttemptKind, OrgTx};
use super::{BehaviourOptions, build_swarm};
use crate::p2p::announce::NodeAnnounceValidator;
use crate::p2p::behaviour::SparkBehaviourEvent;
use crate::p2p::constants::{PLUGIN_ANNOUNCE_MIN_POW_BITS, PLUGIN_ANNOUNCE_RELAY_TENURE_MS};
use crate::p2p::direct::MinIntervalRateLimiter;
use crate::p2p::envelope::EnvelopeSigner;
use crate::p2p::host::NoopHost;
use crate::p2p::overlay_store::OverlayPeerStore;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::plugin_announce::PluginAnnounceValidator;
use crate::storage::MemoryStorage;

/// 测试用 EventLoop（内存存储 + NoopHost；字段初始化与 node/mod.rs 的
/// 生产构造一一对应）。
async fn test_loop() -> EventLoop<MemoryStorage> {
    test_loop_with(BehaviourOptions::default(), true).await
}

/// 带装配开关的测试 EventLoop（R1 显式关闭路径等需要变体时用）。
async fn test_loop_with(
    options: BehaviourOptions,
    enable_relay_server: bool,
) -> EventLoop<MemoryStorage> {
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let swarm = build_swarm(&keypair, &options).await.expect("build swarm");
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (dm_completion_tx, dm_completion_rx) = mpsc::unbounded_channel();
    let (dial_timeout_tx, dial_timeout_rx) = mpsc::unbounded_channel();
    EventLoop {
        swarm,
        storage: MemoryStorage::new(),
        host: Box::new(NoopHost),
        keypair,
        signer: EnvelopeSigner::generate(),
        now_fn: Arc::new(|| 0),
        app_version: "test".to_string(),
        leaf_mode: false,
        cmd_rx,
        cmd_tx,
        event_tx,
        announce_validator: NodeAnnounceValidator::new(),
        exchange_limiter: MinIntervalRateLimiter::new(0),
        recovery_limiter: MinIntervalRateLimiter::new(0),
        last_announced_at: 0,
        overlay_exchange_cursor: 0,
        started_emitted: false,
        port_persisted: false,
        pending_connects: Vec::new(),
        pending_overlay_dials: HashMap::new(),
        version_probe_in_flight: HashSet::new(),
        pending_version: HashMap::new(),
        pending_exchange: HashMap::new(),
        pending_recovery: HashMap::new(),
        pending_recovery_extra: HashMap::new(),
        pending_forward: HashMap::new(),
        pending_forward_extra: HashMap::new(),
        pending_org_attempts: Vec::new(),
        challenge_limiter: MinIntervalRateLimiter::new(0),
        dm_limiter: MinIntervalRateLimiter::new(0),
        peer_protocols: HashMap::new(),
        pending_challenge: HashMap::new(),
        pending_challenge_confirm: HashMap::new(),
        pending_dht_put: HashMap::new(),
        pending_dht_get: HashMap::new(),
        pending_dht_providers: HashMap::new(),
        provided_records: HashMap::new(),
        dht_tick_counter: 0,
        dht_republish_ticks: crate::p2p::constants::DHT_REPUBLISH_TICKS,
        pending_network_change: None,
        last_network_change_fired_at: None,
        last_network_snapshot: None,
        rediscovery_states: HashMap::new(),
        rediscovery_dht_queries: HashMap::new(),
        pending_rediscovery_confirm: HashMap::new(),
        relay_reservations: Vec::new(),
        relay_reservations_inflight: std::collections::HashSet::new(),
        enable_relay_server,
        relay_pool_queries: std::collections::HashSet::new(),
        relay_pool_candidates: Vec::new(),
        relay_pool_stability: std::collections::HashMap::new(),
        relay_pool_record_queries: std::collections::HashSet::new(),
        last_relay_pool_query_at: 0,
        network_change_log: Vec::new(),
        relay_stability_low: false,
        nat_status: super::NatStatusLabel::default(),
        upnp_mapping: None,
        upnp_failed: false,
        dm_completion_tx,
        dm_completion_rx,
        dial_timeout_tx,
        dial_timeout_rx,
        pending_dm_inbound: HashMap::new(),
        next_dm_task_id: 0,
        plugin_announce_validator: PluginAnnounceValidator::new(PLUGIN_ANNOUNCE_MIN_POW_BITS),
        plugin_announce_tenure_ms: PLUGIN_ANNOUNCE_RELAY_TENURE_MS,
        peer_connected_since: HashMap::new(),
        topic_cache: HashMap::new(),
        org_pull_blackhole: false,
        stalled_pull_channels: Vec::new(),
    }
}

/// dm 类 attempt（目标地址列表 + 应答通道）。
fn dm_attempt(
    targets: &[&str],
    peer: PeerId,
) -> (
    OrgAttempt,
    oneshot::Receiver<crate::p2p::Result<Option<Value>>>,
) {
    let (tx, rx) = oneshot::channel();
    let attempt = OrgAttempt {
        kind: OrgAttemptKind::Dm,
        targets: targets
            .iter()
            .map(|s| s.to_string())
            .collect::<VecDeque<_>>(),
        batch: Vec::new(),
        current_peer: Some(peer),
        request_json: "{}".to_string(),
        in_flight: None,
        dial_issued: false,
        waiting_base: None,
        tx: OrgTx::Dm(tx),
    };
    (attempt, rx)
}

/// 取 attempt 当前批次/等待者的 base 地址（测试断言用）。
fn current_base(a: &OrgAttempt) -> Option<String> {
    a.batch
        .first()
        .map(|d| d.addr.clone())
        .or_else(|| a.waiting_base.clone())
}

/// 把 attempt 经 dial_next 走一轮后（拨号或登记等待）放回 pending 队尾。
fn push_dialed(el: &mut EventLoop<MemoryStorage>, mut attempt: OrgAttempt) {
    el.dial_next_org_target(&mut attempt);
    el.pending_org_attempts.push(attempt);
}

fn conn_established(peer: PeerId, remote: &str) -> SwarmEvent<SparkBehaviourEvent> {
    SwarmEvent::ConnectionEstablished {
        peer_id: peer,
        connection_id: ConnectionId::new_unchecked(900),
        endpoint: ConnectedPoint::Dialer {
            address: remote.parse::<Multiaddr>().expect("valid addr"),
            role_override: Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::New,
        },
        num_established: NonZeroU32::new(1).expect("non-zero"),
        concurrent_dial_errors: None,
        established_in: Duration::from_millis(1),
    }
}

fn conn_error(
    connection_id: ConnectionId,
    peer_id: Option<PeerId>,
) -> SwarmEvent<SparkBehaviourEvent> {
    SwarmEvent::OutgoingConnectionError {
        connection_id,
        peer_id,
        error: DialError::Aborted,
    }
}

/// 同地址去重：先到的 attempt 实际拨号，后到的登记等待；连接建立事件
/// 到来时两者都要发出请求（不首个匹配即停）。
#[tokio::test]
async fn conn_established_serves_dialer_and_waiter() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let addr = "/ip4/127.0.0.1/tcp/4001";
    let (a, _rxa) = dm_attempt(&[addr], peer);
    let (b, _rxb) = dm_attempt(&[addr], peer);
    push_dialed(&mut el, a);
    push_dialed(&mut el, b);
    assert_eq!(el.pending_org_attempts.len(), 2);
    assert!(el.pending_org_attempts[0].dial_issued, "先到者实际拨号");
    assert!(
        !el.pending_org_attempts[1].dial_issued,
        "同地址后来者登记等待"
    );
    assert!(
        el.pending_org_attempts[1].waiting_base.is_some(),
        "同地址后来者登记等待"
    );

    el.handle_swarm_event(conn_established(peer, addr));
    assert!(
        el.pending_org_attempts
            .iter()
            .all(|a| a.in_flight.is_some()),
        "等待者也要随已建立连接发出请求"
    );
}

/// OutgoingConnectionError 按 ConnectionId 精确归属：无关连接的失败不
/// 推进 attempt（防止级联耗尽），本 attempt 拨号的失败才移除该在途目标。
#[tokio::test]
async fn conn_error_advances_only_matching_attempt() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let addr = "/ip4/127.0.0.1/tcp/4001";
    let addr2 = "/ip4/127.0.0.1/tcp/4002";
    let (a, _rx) = dm_attempt(&[addr, addr2], peer);
    push_dialed(&mut el, a);
    // 分批并发：addr 与 addr2 同批在途
    assert_eq!(el.pending_org_attempts[0].batch.len(), 2);
    let my_conn = el.pending_org_attempts[0].batch[0].conn_id;

    // 无关连接的失败（如 mdns/其他 attempt 的拨号）：不推进
    el.handle_swarm_event(conn_error(ConnectionId::new_unchecked(999), None));
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(
        el.pending_org_attempts[0].batch.len(),
        2,
        "无关失败不得移除本 attempt 在途目标"
    );

    // 本 attempt 拨号的失败（unknown_peer_id 拨原始地址，peer_id=None）：
    // 移除该在途目标；批内其余目标仍并发竞速
    el.handle_swarm_event(conn_error(my_conn, None));
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(
        el.pending_org_attempts[0].batch.len(),
        1,
        "本 attempt 的失败仅移除该目标，其余仍在途"
    );
    assert!(el.pending_org_attempts[0].dial_issued);
}

/// 拨号方目标耗尽：调用方收到终态，同地址等待者被唤醒自行走目标流程。
#[tokio::test]
async fn exhausted_dialer_wakes_addr_waiter() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let addr = "/ip4/127.0.0.1/tcp/4001";
    let addr3 = "/ip4/127.0.0.1/tcp/4003";
    // A 只有一个目标（失败即耗尽）；B 同地址等待、自身还有后续目标
    let (a, rxa) = dm_attempt(&[addr], peer);
    let (b, _rxb) = dm_attempt(&[addr, addr3], peer);
    push_dialed(&mut el, a);
    push_dialed(&mut el, b);
    assert!(!el.pending_org_attempts[1].dial_issued, "B 登记等待");
    assert!(
        el.pending_org_attempts[1].waiting_base.is_some(),
        "B 等待 addr"
    );
    let conn_a = el.pending_org_attempts[0].batch[0].conn_id;

    el.handle_swarm_event(conn_error(conn_a, None));

    assert_eq!(el.pending_org_attempts.len(), 1, "A 已终结，只剩被唤醒的 B");
    assert!(el.pending_org_attempts[0].dial_issued, "B 被唤醒后自行拨号");
    assert_eq!(
        current_base(&el.pending_org_attempts[0]).as_deref(),
        Some(addr3),
        "B 从自己的下一目标继续"
    );
    // A 的调用方收到「未送达」终态（Dm 语义 Ok(None)）
    assert!(matches!(rxa.await, Ok(Ok(None))));
}

/// connect 命令同样按 ConnectionId 精确归属；调用方超时放弃（rx drop）
/// 的滞留项在下一次 begin_connect 被惰性回收。
#[tokio::test]
async fn connect_error_advances_by_conn_id_and_stale_pruned() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let info = PeerNodeInfo {
        peer_id: Some(peer.to_base58()),
        addresses: vec![
            "/ip4/127.0.0.1/tcp/4101".to_string(),
            "/ip4/127.0.0.1/tcp/4102".to_string(),
        ],
    };
    let (tx, rx) = oneshot::channel();
    el.begin_connect(info.clone(), tx);
    assert_eq!(el.pending_connects.len(), 1);
    // 分批并发：两个输入地址各生成 raw + /p2p 变体 = 4 目标，同批在途
    assert_eq!(el.pending_connects[0].in_flight.len(), 4);
    let conn = el.pending_connects[0].in_flight[0].conn_id;

    // 无关连接的失败：不推进
    el.handle_swarm_event(conn_error(ConnectionId::new_unchecked(999), None));
    assert_eq!(el.pending_connects.len(), 1);
    assert_eq!(el.pending_connects[0].in_flight.len(), 4);

    // 本连接的失败：移除该在途目标，批内其余仍在途
    el.handle_swarm_event(conn_error(conn, None));
    assert_eq!(el.pending_connects.len(), 1);
    assert_eq!(el.pending_connects[0].in_flight.len(), 3);
    assert!(
        !el.pending_connects[0]
            .in_flight
            .iter()
            .any(|d| d.conn_id == conn),
        "失败的在途目标已移除"
    );

    // 调用方超时放弃（rx drop）→ 下次 begin_connect 惰性回收滞留项
    drop(rx);
    let (tx2, _rx2) = oneshot::channel();
    el.begin_connect(info, tx2);
    assert_eq!(
        el.pending_connects.len(),
        1,
        "已放弃的滞留项被回收，只剩新入队的"
    );
}

/// 调用方放弃的 org/dm attempt 在下一次 begin_dm_attempt 被惰性回收。
#[tokio::test]
async fn stale_org_attempts_pruned_on_begin() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let (a, rxa) = dm_attempt(&["/ip4/127.0.0.1/tcp/4001"], peer);
    el.pending_org_attempts.push(a);
    drop(rxa); // 调用方超时放弃

    let (tx, _rx) = oneshot::channel();
    el.begin_dm_attempt(
        PeerNodeInfo {
            peer_id: Some(PeerId::random().to_base58()),
            addresses: Vec::new(),
        },
        serde_json::json!({}),
        tx,
    );
    assert!(
        el.pending_org_attempts.is_empty(),
        "已放弃的滞留 attempt 被回收"
    );
}

/// 竞速拨号失败归属（N2 + V5）：「DHT 命中后拨号待确认」阶段（Racing + 暂存）
/// 与纯缓存拨号竞速（Racing、无 DHT 在途、无暂存）失败都回 Idle 收尾（不卡
/// Racing）；DHT 查询仍在途（Racing、有 dht_query_id、无暂存）不受影响，等
/// DHT 查询结果收尾；无状态 peer 不受影响。
#[tokio::test]
async fn rediscovery_dial_failure_attribution() {
    use super::rediscovery::RediscoveryState;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    // 无状态 peer（普通拨号失败）：不产生任何竞速状态
    el.on_rediscovery_dial_failed(peer);
    assert!(el.rediscovery_states.get(&peer).is_none());
    // 并行 A 阶段（Racing、DHT 查询在途、无暂存）：不受影响，等 DHT 查询结果收尾
    el.start_rediscovery(peer);
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Racing {
                dht_query_id: Some(_),
                ..
            })
        ),
        "start_rediscovery 后应处于 Racing（DHT 查询在途），实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
    el.on_rediscovery_dial_failed(peer);
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Racing { .. })
    ));
    // 纯缓存拨号竞速（Racing、无 DHT 在途、无暂存）：拨号失败即收尾回 Idle（V5），
    // 否则 peer 永久卡 Racing、被 Racing 去重守卫永久拒绝后续触发
    el.rediscovery_states.insert(
        peer,
        RediscoveryState::Racing {
            started_at: 0,
            dht_query_id: None,
        },
    );
    el.on_rediscovery_dial_failed(peer);
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Idle)
        ),
        "纯缓存拨号失败后应回 Idle 收尾，实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
    // 拨号待确认阶段（Racing + 暂存）：回 Idle、清暂存（竞速收尾）
    el.rediscovery_states.insert(
        peer,
        RediscoveryState::Racing {
            started_at: 0,
            dht_query_id: None,
        },
    );
    el.pending_rediscovery_confirm.insert(
        peer,
        crate::p2p::announce::NodeAnnounce {
            msg_type: "spark-node-announce".to_string(),
            version: 1,
            peer_id: peer.to_base58(),
            addresses: vec![],
            timestamp: 0,
            signature: String::new(),
        },
    );
    el.on_rediscovery_dial_failed(peer);
    assert!(
        el.pending_rediscovery_confirm.get(&peer).is_none(),
        "暂存的 announce 应被清理"
    );
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Idle)
        ),
        "竞速拨号失败应回 Idle 静默，实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
}

/// 竞速失败收尾（§4.8 铁律）：DHT 未命中 / 确认失败 / 竞速拨号失败后，peer
/// 从 Racing 回 Idle（而非卡死），且后续可再次 `start_rediscovery` 重新竞速。
#[tokio::test]
async fn rediscovery_failure_aborts_to_idle() {
    use super::rediscovery::RediscoveryState;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    // 未触发前无状态；触发竞速后进入 Racing
    el.start_rediscovery(peer);
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Racing { .. })
        ),
        "start_rediscovery 后应进入 Racing，实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
    // 竞速在途时再次触发被守卫拒绝（不重复竞速）
    el.start_rediscovery(peer);
    assert_eq!(
        el.rediscovery_states
            .get(&peer)
            .map(|s| matches!(s, RediscoveryState::Racing { .. })),
        Some(true),
        "Racing 中重复触发应被守卫拒绝"
    );
    // ① DHT 未命中 → 回 Idle
    el.on_rediscovery_dht_miss(peer, None);
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Idle)
        ),
        "DHT 未命中后应回 Idle，实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
    // 回 Idle 后可再次触发竞速
    el.start_rediscovery(peer);
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Racing { .. })
    ));
    // ② 确认失败（通过竞速拨号失败路径）→ 回 Idle、清暂存
    el.pending_rediscovery_confirm.insert(
        peer,
        crate::p2p::announce::NodeAnnounce {
            msg_type: "spark-node-announce".to_string(),
            version: 1,
            peer_id: peer.to_base58(),
            addresses: vec![],
            timestamp: 0,
            signature: String::new(),
        },
    );
    el.on_rediscovery_dial_failed(peer);
    assert!(
        el.pending_rediscovery_confirm.get(&peer).is_none(),
        "竞速收尾应清暂存 announce"
    );
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Idle)
    ));
    // ③ 回 Idle 后仍可再次触发竞速（不卡死）
    el.start_rediscovery(peer);
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Racing { .. })
    ));
}

/// 电路监听关闭（N3）：清理该 relay 的预约与 in-flight 标记使其可被重选；
/// 非电路地址（普通 TCP 监听）关闭不影响预约状态。
#[tokio::test]
async fn circuit_listener_closed_clears_reservation_state() {
    use super::relay_manager::RelayReservation;
    let mut el = test_loop().await;
    let relay = PeerId::random();
    let mut circuit = Multiaddr::empty();
    circuit.push(libp2p::multiaddr::Protocol::P2p(relay.into()));
    circuit.push(libp2p::multiaddr::Protocol::P2pCircuit);
    el.relay_reservations_inflight.insert(relay);
    el.relay_reservations.push(RelayReservation {
        relay_peer: relay,
        circuit_addr: circuit.clone(),
        created_at: 0,
    });
    // 非电路地址：不影响
    el.on_circuit_listener_closed(&["/ip4/127.0.0.1/tcp/15002".parse().unwrap()]);
    assert!(el.relay_reservations_inflight.contains(&relay));
    assert_eq!(el.relay_reservations.len(), 1);
    // 电路地址：预约与 in-flight 都清理
    el.on_circuit_listener_closed(&[circuit]);
    assert!(el.relay_reservations_inflight.is_empty());
    assert!(el.relay_reservations.is_empty());
}

/// 单目标应用层拨号超时（与 OutgoingConnectionError 同路径）：黑洞目标
/// 到期被放弃、推进下一目标；旧拨号迟到的失败/超时消息因 conn id 已
/// 轮换不再匹配，不得二次推进。
#[tokio::test]
async fn dial_timeout_advances_past_blackhole_target() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let addr = "/ip4/10.255.255.1/tcp/4001"; // 黑洞目标（无 RST，OS 超时数十秒）
    let addr2 = "/ip4/127.0.0.1/tcp/4002";
    let (a, _rx) = dm_attempt(&[addr, addr2], peer);
    push_dialed(&mut el, a);
    // 分批并发：黑洞与可用地址同批在途
    assert_eq!(el.pending_org_attempts[0].batch.len(), 2);
    let conn1 = el.pending_org_attempts[0].batch[0].conn_id;

    // 应用层超时到期：移除黑洞目标，批内可用地址仍在途竞速
    el.fail_org_dial(conn1);
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(el.pending_org_attempts[0].batch.len(), 1);
    assert_eq!(
        el.pending_org_attempts[0].batch[0].addr, addr2,
        "黑洞目标被移除，addr2 仍在途"
    );
    assert!(el.pending_org_attempts[0].dial_issued);

    // 旧拨号迟到的超时/失败事件：conn 已不在批次，不得再次移除（否则误耗尽）
    el.fail_org_dial(conn1);
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(el.pending_org_attempts[0].batch.len(), 1);
}

/// 拨号超时的定时器接线：实际拨号后 spawn 的定时任务在
/// DIRECT_DIAL_TARGET_TIMEOUT_MS（4s，真实等待）后把本次 ConnectionId
/// 送回事件循环。
#[tokio::test]
async fn dial_timeout_timer_fires_for_issued_dial() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let (a, _rx) = dm_attempt(&["/ip4/127.0.0.1/tcp/4001"], peer);
    push_dialed(&mut el, a);
    let conn = el.pending_org_attempts[0].batch[0].conn_id;
    let fired = tokio::time::timeout(
        Duration::from_millis(crate::p2p::constants::DIRECT_DIAL_TARGET_TIMEOUT_MS + 5_000),
        el.dial_timeout_rx.recv(),
    )
    .await
    .expect("超时消息应在单目标拨号超时后送达")
    .expect("channel open");
    assert_eq!(fired, conn, "送回的是本次拨号的 ConnectionId");
}

/// 覆盖网孤岛自举（connection-policy M8，事件驱动）：tick 不再发起任何拨号
/// ——0 连接时跑 tick 也零拨号；自举只在事件点（`bootstrap_overlay_dial`：
/// 启动/网络变更确认）拨一轮。
#[tokio::test]
async fn overlay_bootstrap_event_driven_not_tick() {
    use crate::p2p::overlay_store::OverlayPeerSource;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let peer_str = peer.to_base58();
    // 预置一个近期见过的邻居池候选
    let now = 0i64;
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        store
            .remember(
                &peer_str,
                &["/ip4/10.0.0.9/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                now,
                None,
                &HashSet::new(),
            )
            .unwrap();
    }
    // M8：tick 不再拨号（哪怕 0 连接孤岛）
    let stats = el.run_keepalive_tick();
    assert_eq!(stats.overlay_dialed, 0, "tick 内不得发起覆盖网拨号");
    assert!(
        el.pending_overlay_dials.is_empty(),
        "tick 不得产生 pending 拨号"
    );
    // 事件点（启动/网络变更确认）→ 自举拨一轮
    el.bootstrap_overlay_dial();
    assert!(
        el.pending_overlay_dials.contains_key(&peer),
        "孤岛事件点应自举拨邻居池队首候选"
    );
}

/// 自举失败挂点（OutgoingConnectionError）：清 pending_overlay_dials 并记
/// last_dial_result=failure——候选排序沉底（M8 排序制），无退避/名单状态。
/// 成功挂点记 success 提到队首。
#[tokio::test]
async fn overlay_bootstrap_result_marks_dial_result() {
    use crate::p2p::overlay_store::OverlayPeerSource;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let peer_str = peer.to_base58();
    let now = 0i64;
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        store
            .remember(
                &peer_str,
                &["/ip4/10.0.0.9/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                now,
                None,
                &HashSet::new(),
            )
            .unwrap();
    }
    // 事件点自举拨出 → pending_overlay_dials 记录该 peer
    el.bootstrap_overlay_dial();
    assert!(el.pending_overlay_dials.contains_key(&peer));
    // 真实失败路径：OutgoingConnectionError 挂点清 pending + 记 failure 沉底
    el.handle_swarm_event(conn_error(ConnectionId::new_unchecked(777), Some(peer)));
    assert!(
        !el.pending_overlay_dials.contains_key(&peer),
        "失败挂点应清 pending_overlay_dials"
    );
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        let rec = store.get(&peer_str).unwrap().expect("记录存在");
        assert_eq!(
            rec.last_dial_result.as_deref(),
            Some("failure"),
            "失败应记 last_dial_result=failure（排序沉底）"
        );
    }
    // 成功挂点（ConnectionEstablished）：记 success 提到队首
    el.pending_overlay_dials.insert(peer, ());
    el.handle_swarm_event(conn_established(peer, "/ip4/10.0.0.9/tcp/15002"));
    let mut store = OverlayPeerStore::new(&mut el.storage);
    let rec = store.get(&peer_str).unwrap().expect("记录存在");
    assert_eq!(
        rec.last_dial_result.as_deref(),
        Some("success"),
        "成功应记 last_dial_result=success（排序队首）"
    );
}

/// 入站（listener）连接的远端地址是对端 NAT 源 IP:临时端口（回拨必败），
/// 不得进邻居池拨号候选（否则 merge_neighbor_addresses 让它占据 DM 拨号
/// 队首烧光外层预算）；出站（dialer）方向地址经验证可达，照常入池。
#[tokio::test]
async fn inbound_remote_addr_not_recorded_as_dial_candidate() {
    let mut el = test_loop().await;
    let inbound_peer = PeerId::random();
    let inbound_addr = "/ip4/203.0.113.9/tcp/54321"; // NAT 源地址（临时端口）
    el.handle_swarm_event(SwarmEvent::ConnectionEstablished {
        peer_id: inbound_peer,
        connection_id: ConnectionId::new_unchecked(901),
        endpoint: ConnectedPoint::Listener {
            local_addr: "/ip4/127.0.0.1/tcp/15002".parse().expect("valid addr"),
            send_back_addr: inbound_addr.parse().expect("valid addr"),
        },
        num_established: NonZeroU32::new(1).expect("non-zero"),
        concurrent_dial_errors: None,
        established_in: Duration::from_millis(1),
    });
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        let addrs = store
            .get(&inbound_peer.to_base58())
            .expect("read ok")
            .map(|r| r.addresses)
            .unwrap_or_default();
        assert!(
            !addrs.iter().any(|a| a == inbound_addr),
            "入站源地址不得入池，实际: {addrs:?}"
        );
    }

    // 对照：出站方向的远端地址经成功拨号验证可达，入池
    let outbound_peer = PeerId::random();
    let outbound_addr = "/ip4/203.0.113.10/tcp/15002";
    el.handle_swarm_event(conn_established(outbound_peer, outbound_addr));
    let mut store = OverlayPeerStore::new(&mut el.storage);
    let addrs = store
        .get(&outbound_peer.to_base58())
        .expect("read ok")
        .map(|r| r.addresses)
        .unwrap_or_default();
    assert!(
        addrs.iter().any(|a| a == outbound_addr),
        "出站地址应入池，实际: {addrs:?}"
    );
}

/// 分批并发：批内任一连通即收手（清空其余在途拨号），其余迟到失败不再影响。
#[tokio::test]
async fn connect_batch_any_success_stops_rest() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let info = PeerNodeInfo {
        peer_id: Some(peer.to_base58()),
        addresses: vec![
            "/ip4/127.0.0.1/tcp/4201".to_string(),
            "/ip4/127.0.0.1/tcp/4202".to_string(),
        ],
    };
    let (tx, _rx) = oneshot::channel();
    el.begin_connect(info, tx);
    assert_eq!(el.pending_connects[0].in_flight.len(), 4);
    let conn1 = el.pending_connects[0].in_flight[0].conn_id;
    // 其中一路连通 → 收手：清空其余在途拨号
    el.handle_swarm_event(conn_established(peer, "/ip4/127.0.0.1/tcp/4201"));
    assert!(el.pending_connects.is_empty(), "连通即收手，connect 终结");
    // 其余在途拨号的迟到失败（conn_id 已不在批次）不得误操作
    el.handle_swarm_event(conn_error(conn1, None));
    assert!(el.pending_connects.is_empty());
}

/// 分批并发：全批失败才开下一批；批内未耗尽不推进。
#[tokio::test]
async fn connect_batch_full_failure_opens_next() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    // 5 个地址 → 两批（4 + 1）；构造超过批大小
    let addresses: Vec<String> = (4201..=4205)
        .map(|p| format!("/ip4/127.0.0.1/tcp/{p}"))
        .collect();
    // 为控制批大小，手动构造 attempt 而非走 begin_connect（begin_connect 首批
    // 即填满 DIAL_BATCH_SIZE）
    let (a, _rx) = dm_attempt(
        &addresses
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        peer,
    );
    // 先拨第一批 4 个（DIAL_BATCH_SIZE）
    push_dialed(&mut el, a);
    assert_eq!(el.pending_org_attempts[0].batch.len(), 4, "首批 4 个在途");
    assert_eq!(el.pending_org_attempts[0].targets.len(), 1, "剩 1 个待拨");
    // 逐个失败直至本批全败 → 开下一批
    let batch1: Vec<_> = el.pending_org_attempts[0]
        .batch
        .iter()
        .map(|d| d.conn_id)
        .collect();
    for conn in &batch1 {
        el.handle_swarm_event(conn_error(*conn, None));
    }
    // 全批失败后开下一批（1 个）
    assert_eq!(el.pending_org_attempts[0].batch.len(), 1, "下一批 1 个");
    assert_eq!(el.pending_org_attempts[0].targets.len(), 0);
}

/// tick 本地地址对比：地址变化即武装防抖一次性定时器；首 tick 只记录基线不
/// 触发。闹钟到点回发 NetworkChangeFired：基线与当前快照一致（虚警）则不执行
/// 任何重连动作。
#[tokio::test]
async fn tick_detects_local_addr_change_and_arms_debounce() {
    let mut el = test_loop().await;
    // 首 tick：只记录基线，不武装
    el.run_keepalive_tick();
    assert!(el.pending_network_change.is_none(), "首 tick 不触发");
    assert!(el.last_network_snapshot.is_some());
    // 模拟地址变化：把旧快照改写为与当前快照不同，再探测（测试环境无真实
    // 监听地址，network_snapshot 为空，故用非空快照模拟「地址变了」）。
    let changed: Vec<String> = vec!["/ip4/10.1.1.1/tcp/15002".to_string()];
    el.last_network_snapshot = Some(changed.clone());
    el.detect_local_network_change();
    assert!(
        el.pending_network_change.is_some(),
        "地址变化应武装防抖闹钟"
    );
    // 闹钟到点回发：基线（changed）与当前快照（空）不同 → 视为真实变化，
    // 执行动作并清除闹钟标记（动作本身是幂等的重发布/重拨，无断言面，验证不 panic）
    el.handle_command(Command::NetworkChangeFired { base: changed });
    assert!(el.pending_network_change.is_none(), "闹钟响后应清除标记");
}

/// NetworkChangeFired 虚警路径：基线与当前快照一致 → 清除标记但不执行动作。
#[tokio::test]
async fn network_change_fired_noop_when_unchanged() {
    let mut el = test_loop().await;
    let current = el.network_snapshot();
    el.pending_network_change = Some(0);
    el.handle_command(Command::NetworkChangeFired { base: current });
    assert!(el.pending_network_change.is_none());
}

/// leaf 模式（mobile-leaf-mode §3）：tick 不再出现 exchanged/announced/DHT 重发
/// 痕迹——exchange 轮选、node-announce、DHT 记录/provide 重发全跳过，dht 计数
/// 不推进；relay 预约补充（ensure_relay_reservations）保留不断言（无 relay 候选
/// 时天然空操作）。
#[tokio::test]
async fn leaf_tick_skips_exchange_announce_and_dht_republish() {
    let mut el = test_loop().await;
    el.leaf_mode = true;
    let stats = el.run_keepalive_tick();
    assert_eq!(stats.exchanged, 0, "leaf 下不得发起 peer-exchange");
    assert!(!stats.announced, "leaf 下不得发布 node-announce");
    assert!(
        el.pending_exchange.is_empty(),
        "leaf 下不得有在途 exchange 请求"
    );
    assert_eq!(
        el.dht_tick_counter, 0,
        "leaf 下 DHT 重发计数不推进（存在记录/provide 重发关闭）"
    );
    // 对照：非 leaf 首个 tick 计数推进到 1
    let mut el2 = test_loop().await;
    el2.run_keepalive_tick();
    assert_eq!(el2.dht_tick_counter, 1);
}

/// leaf 模式（§3）：announce 发布空操作（返回 false、不刷新发布时间）；
/// org provide（网关职责）直接拒绝且不登记 provided_records（内核按 dht off
/// 同口径静默重试）。
#[tokio::test]
async fn leaf_publish_announce_noop_and_provide_rejected() {
    let mut el = test_loop().await;
    el.leaf_mode = true;
    assert_eq!(
        el.publish_announce().expect("publish_announce"),
        false,
        "leaf 下 announce 发布为空操作"
    );
    assert_eq!(el.last_announced_at, 0, "leaf 下不刷新发布时间");

    let (tx, rx) = oneshot::channel();
    el.begin_dht_provide(b"k".to_vec(), b"v".to_vec(), tx);
    let result = rx.await.expect("propose channel open");
    assert!(result.is_err(), "leaf 下 org provide 应被拒绝");
    assert!(
        el.provided_records.is_empty(),
        "leaf 下不得登记 provide 重发"
    );
}

/// leaf 模式守卫反面（M-C）：begin_exchange 直接 Ok(0) 不产生在途请求；
/// bootstrap_overlay_dial 有候选也不拨；seed_kad_routing 空操作（不 panic）。
/// 非 leaf 对照已由 `overlay_bootstrap_event_driven_not_tick`（事件点自举拨
/// 一轮）覆盖。
#[tokio::test]
async fn leaf_guards_block_exchange_and_overlay_maintenance() {
    use crate::p2p::overlay_store::OverlayPeerSource;
    let mut el = test_loop().await;
    el.leaf_mode = true;
    // peer-exchange 请求关闭：Ok(0) 且无在途
    let (tx, rx) = oneshot::channel();
    el.begin_exchange(&PeerId::random().to_base58(), tx);
    assert!(matches!(rx.await, Ok(Ok(0))), "leaf 下 exchange 返回 Ok(0)");
    assert!(
        el.pending_exchange.is_empty(),
        "leaf 下无在途 exchange 请求"
    );
    // overlay 孤岛自举关闭：预置候选也不拨
    let peer = PeerId::random();
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        store
            .remember(
                &peer.to_base58(),
                &["/ip4/10.0.0.9/tcp/15002".to_string()],
                OverlayPeerSource::Connect,
                false,
                0,
                None,
                &HashSet::new(),
            )
            .unwrap();
    }
    el.bootstrap_overlay_dial();
    assert!(
        el.pending_overlay_dials.is_empty(),
        "leaf 下孤岛自举不得拨号"
    );
    // kad 播种关闭（守护返回即空操作，验证不 panic 不拨号）
    el.seed_kad_routing();
}

// ------------------------------------------------------------------
// R1/R2（relay-implementation §2）：AutoNAT 驱动 relay 角色启停 + spark:relay 共享池
// ------------------------------------------------------------------

/// 构造 AutoNAT StatusChanged 行为事件。
fn autonat_status_event(
    old: libp2p::autonat::NatStatus,
    new: libp2p::autonat::NatStatus,
) -> SwarmEvent<SparkBehaviourEvent> {
    SwarmEvent::Behaviour(SparkBehaviourEvent::Autonat(
        libp2p::autonat::Event::StatusChanged { old, new },
    ))
}

/// R1：AutoNAT 状态驱动 relay server 角色启停 + R2 就绪 provide / 摘牌撤下。
/// 初始挂载为现状兼容（显式 true）；Private 摘牌、Public 重挂并登记共享池
/// provide（tick 周期重发段复用 provided_records）。
#[tokio::test]
async fn autonat_status_drives_relay_server_role() {
    use libp2p::autonat::NatStatus;
    let mut el = test_loop().await;
    assert!(
        el.swarm.behaviour().relay_server.as_ref().is_some(),
        "显式 true 初始挂载（现状兼容）"
    );
    // 公网 external 地址（R2 provide 载荷取公网段）
    el.swarm
        .add_external_address("/ip4/203.0.113.1/tcp/4001".parse().unwrap());

    // Public → 角色在 + spark:relay provide 登记
    el.handle_swarm_event(autonat_status_event(
        NatStatus::Unknown,
        NatStatus::Public("/ip4/203.0.113.1/tcp/4001".parse().unwrap()),
    ));
    assert!(
        el.swarm.behaviour().relay_server.as_ref().is_some(),
        "Public 启用 relay 角色"
    );
    let value = el
        .provided_records
        .get(crate::p2p::constants::SPARK_RELAY_KEY.as_bytes())
        .expect("Public 就绪即登记 spark:relay provide（周期重发并入 tick）");
    let hint = super::relay_manager::RelayProviderHint::from_record_value(value)
        .expect("provide 载荷可解析");
    assert_eq!(hint.peer_id, el.self_peer_id().to_base58());
    assert!(
        hint.addresses.iter().any(|a| a.contains("203.0.113.1")),
        "载荷含公网段地址，实际: {:?}",
        hint.addresses
    );

    // Private → 摘牌 + 撤 provide（不接受新预约，既有预约由连接 handler 到期）
    el.handle_swarm_event(autonat_status_event(
        NatStatus::Public("/ip4/203.0.113.1/tcp/4001".parse().unwrap()),
        NatStatus::Private,
    ));
    assert!(
        el.swarm.behaviour().relay_server.as_ref().is_none(),
        "Private 摘牌"
    );
    assert!(
        !el.provided_records
            .contains_key(crate::p2p::constants::SPARK_RELAY_KEY.as_bytes()),
        "Private 撤下共享池 provide"
    );
}

/// R1 优先级：显式 `enable_relay_server: false`（用户手动关/移动端）优先于
/// AutoNAT 判定——Public 也不启用角色、不 provide。
#[tokio::test]
async fn autonat_explicit_off_wins_over_public() {
    use libp2p::autonat::NatStatus;
    let mut el = test_loop_with(
        BehaviourOptions {
            enable_relay_server: false,
            ..Default::default()
        },
        false,
    )
    .await;
    el.handle_swarm_event(autonat_status_event(
        NatStatus::Unknown,
        NatStatus::Public("/ip4/203.0.113.1/tcp/4001".parse().unwrap()),
    ));
    assert!(
        el.swarm.behaviour().relay_server.as_ref().is_none(),
        "显式关闭优先：Public 也不启用"
    );
    assert!(el.provided_records.is_empty(), "显式关闭不 provide");
}

/// R2 客户端：无已连接候选时 ensure 触发共享池查询（in-flight 去重）；
/// 查询结果 providers 落候选源（R3 消费）。
#[tokio::test]
async fn relay_pool_query_registers_candidates() {
    let mut el = test_loop().await;
    assert!(el.relay_reservations.is_empty());
    // 无已连接候选 → 触发 spark:relay 查询
    el.ensure_relay_reservations();
    assert_eq!(el.relay_pool_queries.len(), 1, "无候选应发起共享池查询");
    el.ensure_relay_reservations();
    assert_eq!(
        el.relay_pool_queries.len(),
        1,
        "查询在途不重复发起（in-flight 守卫）"
    );
    // 模拟查询结果：provider 落候选源 + 触发拨号尝试
    let provider = PeerId::random();
    let qid = *el
        .relay_pool_queries
        .iter()
        .next()
        .expect("query in flight");
    el.resolve_dht_providers(
        qid,
        Ok(libp2p::kad::GetProvidersOk::FoundProviders {
            key: libp2p::kad::RecordKey::new(&crate::p2p::constants::SPARK_RELAY_KEY),
            providers: [provider].into_iter().collect(),
        }),
    );
    assert!(el.relay_pool_queries.is_empty(), "结果分流后清理在途");
    assert_eq!(
        el.relay_pool_candidates,
        vec![provider],
        "provider 落为候选源（R3 排序消费）"
    );
}

/// R2 链路（双真实节点，loopback）：A（非 leaf）provide `spark:relay` →
/// B（leaf，kad client）get_providers 发现 A 为候选。
#[tokio::test]
async fn relay_pool_provide_and_discover_two_nodes() {
    use crate::p2p::host::NoopHost;
    use crate::p2p::{P2pConfig, P2pNode};
    let base = P2pConfig {
        preferred_port: Some(0),
        port_scan: false,
        enable_mdns: false,
        enable_upnp: false,
        enable_ws: false,
        enable_ipv6: false,
        keepalive_interval: None,
        ..Default::default()
    };
    let node_a = P2pNode::start(
        P2pConfig {
            enable_relay_server: true,
            ..base.clone()
        },
        MemoryStorage::new(),
        Box::new(NoopHost),
    )
    .await
    .expect("start A");
    // B 为 leaf：kad client（只查不发），R2 客户端形态
    let node_b = P2pNode::start(
        P2pConfig {
            leaf_mode: true,
            ..base.clone()
        },
        MemoryStorage::new(),
        Box::new(NoopHost),
    )
    .await
    .expect("start B");

    // 监听地址经 NewListenAddr 事件异步生效：先等到 A 的地址可见
    let mut info_a = node_a.local_node_info().await.expect("A local info");
    for _ in 0..20 {
        if !info_a.addresses.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        info_a = node_a.local_node_info().await.expect("A local info");
    }
    assert!(!info_a.addresses.is_empty(), "A 监听地址应可见");
    node_b
        .connect_peer(&PeerNodeInfo {
            peer_id: info_a.peer_id.clone(),
            addresses: info_a.addresses.clone(),
        })
        .await
        .expect("B connect A");

    // A 对约定键 provide（载荷 = peerId + 可达地址集，与 R1 就绪路径同型）
    let value = super::relay_manager::RelayProviderHint {
        peer_id: node_a.peer_id().to_string(),
        addresses: info_a.addresses.clone(),
        stability: None,
    }
    .to_record_value();
    node_a
        .dht_provide_record(crate::p2p::constants::SPARK_RELAY_KEY.as_bytes(), value)
        .await
        .expect("A provide spark:relay");

    // B（leaf）查询共享池：provider 记录复制有传播窗口，轮询几秒
    let mut providers: Vec<String> = Vec::new();
    for _ in 0..20 {
        providers = node_b
            .dht_get_providers(crate::p2p::constants::SPARK_RELAY_KEY.as_bytes())
            .await
            .unwrap_or_default();
        if !providers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        providers.iter().any(|p| p == node_a.peer_id()),
        "B(leaf) 应发现 A 为 spark:relay 候选，实际: {providers:?}"
    );
    node_a.stop().await;
    node_b.stop().await;
}

/// R3 梯队选择（relay-implementation §2）：leaf 与非 leaf 同纪律——无已连接
/// 候选时共享池候选（梯队②）入选；已预约/in-flight 排除。
#[tokio::test]
async fn leaf_selects_relay_pool_candidates() {
    let mut el = test_loop().await;
    el.leaf_mode = true;
    let provider = PeerId::random();
    el.relay_pool_candidates.push(provider);
    assert_eq!(
        el.select_relay_candidates(),
        vec![provider],
        "leaf 下共享池候选应入选（排序纪律与节点形态无关）"
    );
    // 已预约的候选排除
    el.relay_reservations_inflight.insert(provider);
    assert!(
        el.select_relay_candidates().is_empty(),
        "in-flight 候选排除"
    );
}

/// relay 预约请求地址构造回归（真机实测根修）：
/// - 电路监听地址必须携带 relay 完整传输地址（裸 /p2p/<relay>/p2p-circuit
///   被 libp2p-relay 0.21 client 以 MissingRelayAddr 拒绝，listen_on Err，
///   预约永远不成）；
/// - overlay 基地址自带 /p2p/<peer> 尾段时须剥掉（否则双重 /p2p 尾段）；
/// - 无可用传输地址时跳过（不发起必败的 listen_on）。
#[tokio::test]
async fn relay_reservation_request_builds_full_circuit_addr() {
    let mut el = test_loop().await;
    let relay = PeerId::random();
    // 无 overlay 地址：跳过（不进 in-flight）
    el.request_relay_reservation(relay);
    assert!(
        el.relay_reservations_inflight.is_empty(),
        "无传输地址应跳过，不发起必败的 listen_on"
    );

    // overlay 播种 relay 地址（带 /p2p 尾段，模拟 DHT/announce 来源）
    {
        let mut store = OverlayPeerStore::new(&mut el.storage);
        store
            .remember(
                &relay.to_base58(),
                &[
                    // 电路地址形态：必须被跳过（否则拼出双电路段，真机实测
                    // MultipleCircuitRelayProtocolsUnsupported）
                    format!("/ip4/203.0.113.9/tcp/4001/p2p/{relay}/p2p-circuit"),
                    // ws 形态：降权垫底（Android 无 ws 传输）
                    format!("/ip4/203.0.113.8/tcp/4001/ws/p2p/{relay}"),
                    format!("/ip4/203.0.113.7/tcp/4001/p2p/{relay}"),
                ],
                crate::p2p::overlay_store::OverlayPeerSource::Announce,
                false,
                0,
                None,
                &HashSet::new(),
            )
            .expect("remember relay addr");
    }
    el.request_relay_reservation(relay);
    assert!(
        el.relay_reservations_inflight.contains(&relay),
        "listen_on 成功应进 in-flight（裸地址 MissingRelayAddr 回归）"
    );
    // 电路监听地址：完整传输段 + 单 /p2p 尾段（双重 /p2p 回归）
    let expect = format!("/ip4/203.0.113.7/tcp/4001/p2p/{relay}/p2p-circuit");
    assert_eq!(
        el.build_circuit_address(relay).to_string(),
        expect,
        "基地址自带 /p2p 尾段时须剥掉再拼"
    );
}

/// U1 状态快照（relay-implementation §3）：autonat/角色/共享池大小/预约如实
/// 反映；到期与配额为近似值（字段注释口径）。
#[tokio::test]
async fn relay_status_snapshot_reflects_role_and_pool() {
    use libp2p::autonat::NatStatus;
    let mut el = test_loop().await;
    el.relay_pool_candidates = vec![PeerId::random(), PeerId::random()];
    // 未判定前：unknown + serving（显式 true 初始挂载）
    let st = el.local_relay_status();
    assert_eq!(st.autonat, "unknown");
    assert_eq!(st.relay_role, "serving");
    assert_eq!(st.pool_size, 2);
    assert!(st.reservations.is_empty());
    assert!(!st.stability_low);
    // Private 判定后：角色摘牌 → off
    el.handle_swarm_event(autonat_status_event(NatStatus::Unknown, NatStatus::Private));
    let st = el.local_relay_status();
    assert_eq!(st.autonat, "private");
    assert_eq!(st.relay_role, "off", "Private 摘牌后角色 off");
    // 预约条目：近似到期 = created_at + 2h（now=0 时钟下为正）
    let relay = PeerId::random();
    let mut circuit = Multiaddr::empty();
    circuit.push(libp2p::multiaddr::Protocol::P2p(relay.into()));
    circuit.push(libp2p::multiaddr::Protocol::P2pCircuit);
    el.relay_reservations
        .push(super::relay_manager::RelayReservation {
            relay_peer: relay,
            circuit_addr: circuit,
            created_at: 0,
        });
    let st = el.local_relay_status();
    assert_eq!(st.reservations.len(), 1);
    assert_eq!(st.reservations[0].peer, relay.to_base58());
    assert!(st.reservations[0].expires_in_ms > 0, "近似到期剩余为正");
    assert_eq!(st.reservations[0].used_bytes, None, "用量 libp2p 不暴露");
}

/// U2 向导三态（relay-implementation §3）：UPnP 事件驱动 mapped/failed/unknown
/// 状态翻转（NewExternalAddr → mapped；Expired/GatewayNotFound → failed）。
#[tokio::test]
async fn upnp_events_drive_three_state_status() {
    let mut el = test_loop().await;
    assert_eq!(
        el.local_relay_status().upnp,
        "unknown",
        "尚无事件为 unknown"
    );
    let mapped: Multiaddr = "/ip4/203.0.113.1/tcp/15002".parse().unwrap();
    el.handle_swarm_event(SwarmEvent::Behaviour(SparkBehaviourEvent::Upnp(
        libp2p::upnp::Event::NewExternalAddr(mapped.clone()),
    )));
    assert_eq!(el.local_relay_status().upnp, "mapped", "映射成功为 mapped");
    el.handle_swarm_event(SwarmEvent::Behaviour(SparkBehaviourEvent::Upnp(
        libp2p::upnp::Event::ExpiredExternalAddr(mapped),
    )));
    assert_eq!(el.local_relay_status().upnp, "failed", "映射过期为 failed");
    // 网关探测失败（含双层 NAT 迹象的 NonRoutableGateway）同为 failed
    let mut el2 = test_loop().await;
    el2.handle_swarm_event(SwarmEvent::Behaviour(SparkBehaviourEvent::Upnp(
        libp2p::upnp::Event::NonRoutableGateway,
    )));
    assert_eq!(el2.local_relay_status().upnp, "failed");
}

/// #2 stability 回填：providers 结果触发 get_record → 载荷 stability="low"
/// 回填映射 → 该候选在 sort_relay_tier 中垫底；无 stability 字段保持无降权。
#[tokio::test]
async fn relay_pool_record_backfills_stability() {
    use super::relay_manager::{RelayProviderHint, sort_relay_tier};
    let mut el = test_loop().await;
    let low_peer = PeerId::random();
    let normal_peer = PeerId::random();
    // 经 providers 结果触发回填查询（取走在途 qid）
    el.on_relay_pool_providers(Ok(libp2p::kad::GetProvidersOk::FoundProviders {
        key: libp2p::kad::RecordKey::new(&crate::p2p::constants::SPARK_RELAY_KEY),
        providers: [low_peer, normal_peer].into_iter().collect(),
    }));
    assert_eq!(
        el.relay_pool_record_queries.len(),
        1,
        "providers 后应发起回填查询"
    );
    let qid = *el.relay_pool_record_queries.iter().next().unwrap();
    // 构造带 stability:"low" 的提供记录回填
    let hint = RelayProviderHint {
        peer_id: low_peer.to_base58(),
        addresses: vec![],
        stability: Some("low".to_string()),
    };
    el.resolve_dht_get(
        qid,
        Ok(libp2p::kad::GetRecordOk::FoundRecord(
            libp2p::kad::PeerRecord {
                peer: None,
                record: libp2p::kad::Record {
                    key: libp2p::kad::RecordKey::new(&crate::p2p::constants::SPARK_RELAY_KEY),
                    value: hint.to_record_value(),
                    publisher: None,
                    expires: None,
                },
            },
        )),
    );
    assert_eq!(
        el.relay_pool_stability.get(&low_peer),
        Some(&true),
        "低稳候选应回填映射"
    );
    assert!(!el.relay_pool_stability.contains_key(&normal_peer));
    // 回填后排序：low 垫底（即便候选顺序原本在前）
    let last_seen = |_: &PeerId| 0i64;
    let is_low = |p: &PeerId| el.relay_pool_stability.get(p).copied().unwrap_or(false);
    let mut tier = vec![low_peer, normal_peer];
    sort_relay_tier(&mut tier, &last_seen, &is_low);
    assert_eq!(tier, vec![normal_peer, low_peer], "low 候选垫底");
}
