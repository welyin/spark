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
    let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (dm_completion_tx, dm_completion_rx) = mpsc::unbounded_channel();
    EventLoop {
        swarm,
        storage: MemoryStorage::new(),
        host: Box::new(NoopHost),
        keypair,
        signer: EnvelopeSigner::generate(),
        now_fn: Arc::new(|| 0),
        app_version: "test".to_string(),
        cmd_rx,
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
        pending_network_change_base: None,
        rediscovery_states: HashMap::new(),
        rediscovery_dht_queries: HashMap::new(),
        rediscovery_failures: HashMap::new(),
        pending_rediscovery_confirm: HashMap::new(),
        relay_reservations: Vec::new(),
        relay_reservations_inflight: std::collections::HashSet::new(),
        dm_completion_tx,
        dm_completion_rx,
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
        current_target: None,
        current_peer: Some(peer),
        request_json: "{}".to_string(),
        in_flight: None,
        dial_issued: false,
        dial_conn_id: None,
        tx: OrgTx::Dm(tx),
    };
    (attempt, rx)
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
    assert!(el.pending_org_attempts[1].current_target.is_some());

    el.handle_swarm_event(conn_established(peer, addr));
    assert!(
        el.pending_org_attempts.iter().all(|a| a.in_flight.is_some()),
        "等待者也要随已建立连接发出请求"
    );
}

/// OutgoingConnectionError 按 ConnectionId 精确归属：无关连接的失败不
/// 推进 attempt（防止级联耗尽），本 attempt 拨号的失败才推进下一目标。
#[tokio::test]
async fn conn_error_advances_only_matching_attempt() {
    let mut el = test_loop().await;
    let peer = PeerId::random();
    let addr = "/ip4/127.0.0.1/tcp/4001";
    let addr2 = "/ip4/127.0.0.1/tcp/4002";
    let (a, _rx) = dm_attempt(&[addr, addr2], peer);
    push_dialed(&mut el, a);
    let my_conn = el.pending_org_attempts[0]
        .dial_conn_id
        .expect("dial issued records conn id");

    // 无关连接的失败（如 mdns/其他 attempt 的拨号）：不推进
    el.handle_swarm_event(conn_error(ConnectionId::new_unchecked(999), None));
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(
        el.pending_org_attempts[0].current_target.as_deref(),
        Some(addr),
        "无关失败不得推进 attempt"
    );

    // 本 attempt 拨号的失败（unknown_peer_id 拨原始地址，peer_id=None）：
    // 推进下一目标
    el.handle_swarm_event(conn_error(my_conn, None));
    assert_eq!(el.pending_org_attempts.len(), 1);
    assert_eq!(
        el.pending_org_attempts[0].current_target.as_deref(),
        Some(addr2),
        "本 attempt 的失败应推进到下一目标"
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
    let conn_a = el.pending_org_attempts[0].dial_conn_id.expect("A dialed");

    el.handle_swarm_event(conn_error(conn_a, None));

    assert_eq!(el.pending_org_attempts.len(), 1, "A 已终结，只剩被唤醒的 B");
    assert!(el.pending_org_attempts[0].dial_issued, "B 被唤醒后自行拨号");
    assert_eq!(
        el.pending_org_attempts[0].current_target.as_deref(),
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
    let conn = el.pending_connects[0].dial_conn_id.expect("dial issued");
    let remaining = el.pending_connects[0].targets.len();

    // 无关连接的失败：不推进
    el.handle_swarm_event(conn_error(ConnectionId::new_unchecked(999), None));
    assert_eq!(el.pending_connects.len(), 1);
    assert_eq!(el.pending_connects[0].targets.len(), remaining);

    // 本连接的失败：推进下一目标
    el.handle_swarm_event(conn_error(conn, None));
    assert_eq!(el.pending_connects.len(), 1);
    assert_eq!(el.pending_connects[0].targets.len(), remaining - 1);
    assert_ne!(
        el.pending_connects[0].dial_conn_id,
        Some(conn),
        "推进后记录新一轮拨号的 conn id"
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
