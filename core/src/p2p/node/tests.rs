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
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let swarm = build_swarm(&keypair, &BehaviourOptions::default())
        .await
        .expect("build swarm");
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
        last_network_snapshot: None,
        rediscovery_states: HashMap::new(),
        rediscovery_dht_queries: HashMap::new(),
        rediscovery_failures: HashMap::new(),
        pending_rediscovery_confirm: HashMap::new(),
        relay_reservations: Vec::new(),
        relay_reservations_inflight: std::collections::HashSet::new(),
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
    }
}

/// dm 类 attempt（目标地址列表 + 应答通道）。
fn dm_attempt(
    targets: &[&str],
    peer: PeerId,
) -> (OrgAttempt, oneshot::Receiver<crate::p2p::Result<Option<Value>>>) {
    let (tx, rx) = oneshot::channel();
    let attempt = OrgAttempt {
        kind: OrgAttemptKind::Dm,
        targets: targets.iter().map(|s| s.to_string()).collect::<VecDeque<_>>(),
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

fn conn_error(connection_id: ConnectionId, peer_id: Option<PeerId>) -> SwarmEvent<SparkBehaviourEvent> {
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
    assert!(!el.pending_org_attempts[1].dial_issued, "同地址后来者登记等待");
    assert!(
        el.pending_org_attempts[1].waiting_base.is_some(),
        "同地址后来者登记等待"
    );

    el.handle_swarm_event(conn_established(peer, addr));
    assert!(
        el.pending_org_attempts.iter().all(|a| a.in_flight.is_some()),
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
        !el.pending_connects[0].in_flight.iter().any(|d| d.conn_id == conn),
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

/// 回归测试（peer-rediscovery §4.8 严重缺陷）：退避到期后必须能重新竞速，
/// 而不是被 `start_rediscovery` 的入口守卫拒绝卡死在 Backoff。
#[tokio::test]
async fn rediscovery_backoff_can_retry_after_deadline() {
    use super::rediscovery::RediscoveryState;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    // 已到期的 Backoff（now() 恒为 0，next_retry_at=0 即已到期）
    el.rediscovery_states.insert(
        peer,
        RediscoveryState::Backoff { next_retry_at: 0 },
    );
    el.poll_rediscovery_retries();
    let state = el.rediscovery_states.get(&peer).expect("state present");
    assert!(
        matches!(state, RediscoveryState::Racing { .. }),
        "退避到期后应重新进入竞速，实际: {state:?}"
    );
}

/// 连续失败达到上限 → Offline（§4.8 5 次封顶），且再次退避到期不复活。
/// 走真实路径循环 miss → Backoff 到期 poll 重试 → Racing → miss——连调
/// miss 不经过 poll 的测法不代表真实路径（失败计数须跨重试轮次保留）。
#[tokio::test]
async fn rediscovery_exhaustion_reaches_offline() {
    use super::rediscovery::RediscoveryState;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    for round in 1..5u32 {
        el.on_rediscovery_dht_miss(peer, None);
        let state = el.rediscovery_states.get(&peer).expect("state present");
        assert!(
            matches!(state, RediscoveryState::Backoff { .. }),
            "第 {round} 次失败后应为 Backoff，实际: {state:?}"
        );
        assert_eq!(
            el.rediscovery_failures.get(&peer).copied(),
            Some(round),
            "第 {round} 次失败后连续失败计数应为 {round}"
        );
        // 模拟退避到期（now() 恒为 0，把 next_retry_at 拨回 0）后重新竞速
        if let Some(s) = el.rediscovery_states.get_mut(&peer) {
            *s = RediscoveryState::Backoff { next_retry_at: 0 };
        }
        el.poll_rediscovery_retries();
        let state = el.rediscovery_states.get(&peer).expect("state present");
        assert!(
            matches!(state, RediscoveryState::Racing { .. }),
            "第 {round} 轮退避到期后应重新竞速，实际: {state:?}"
        );
    }
    // 第 5 次失败 → Offline，连续失败计数随之清除
    el.on_rediscovery_dht_miss(peer, None);
    assert!(
        matches!(
            el.rediscovery_states.get(&peer),
            Some(RediscoveryState::Offline)
        ),
        "5 次失败后应为 Offline，实际: {:?}",
        el.rediscovery_states.get(&peer)
    );
    assert!(
        el.rediscovery_failures.get(&peer).is_none(),
        "Offline 后失败计数应清除"
    );
    // Offline 状态下再 miss 不复活
    el.on_rediscovery_dht_miss(peer, None);
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Offline)
    ));
}

/// 竞速拨号失败归属（N2）：仅「DHT 命中后拨号待确认」阶段（Racing + 暂存）
/// 计失败进退避并清暂存；无状态 peer 与并行 A 阶段（Racing 但无暂存，
/// DHT 查询仍在途）不受影响。
#[tokio::test]
async fn rediscovery_dial_failure_attribution() {
    use super::rediscovery::RediscoveryState;
    let mut el = test_loop().await;
    let peer = PeerId::random();
    // 无状态 peer（普通拨号失败）：不产生任何竞速状态
    el.on_rediscovery_dial_failed(peer);
    assert!(el.rediscovery_states.get(&peer).is_none());
    // 并行 A 阶段（Racing、无暂存）：不影响，等 DHT 查询结果收尾
    el.rediscovery_states.insert(
        peer,
        RediscoveryState::Racing {
            started_at: 0,
            dht_query_id: None,
        },
    );
    el.on_rediscovery_dial_failed(peer);
    assert!(matches!(
        el.rediscovery_states.get(&peer),
        Some(RediscoveryState::Racing { .. })
    ));
    // 拨号待确认阶段（Racing + 暂存）：计一次失败进 Backoff、清暂存
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
            Some(RediscoveryState::Backoff { .. })
        ) && el.rediscovery_failures.get(&peer).copied() == Some(1),
        "竞速拨号失败应计一次失败进退避，实际: {:?} / failures={:?}",
        el.rediscovery_states.get(&peer),
        el.rediscovery_failures.get(&peer)
    );
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
        el.pending_org_attempts[0].batch[0].addr,
        addr2,
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
    assert!(
        el.pending_connects.is_empty(),
        "连通即收手，connect 终结"
    );
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
    let addresses: Vec<String> = (4201..=4205).map(|p| format!("/ip4/127.0.0.1/tcp/{p}")).collect();
    // 为控制批大小，手动构造 attempt 而非走 begin_connect（begin_connect 首批
    // 即填满 DIAL_BATCH_SIZE）
    let (a, _rx) = dm_attempt(&addresses.iter().map(String::as_str).collect::<Vec<_>>().as_slice(), peer);
    // 先拨第一批 4 个（DIAL_BATCH_SIZE）
    push_dialed(&mut el, a);
    assert_eq!(el.pending_org_attempts[0].batch.len(), 4, "首批 4 个在途");
    assert_eq!(el.pending_org_attempts[0].targets.len(), 1, "剩 1 个待拨");
    // 逐个失败直至本批全败 → 开下一批
    let batch1: Vec<_> = el.pending_org_attempts[0].batch.iter().map(|d| d.conn_id).collect();
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
