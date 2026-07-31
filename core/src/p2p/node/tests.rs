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
use crate::p2p::direct::MinIntervalRateLimiter;
use crate::p2p::envelope::EnvelopeSigner;
use crate::p2p::host::NoopHost;
use crate::p2p::peer_targets::PeerNodeInfo;
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
        dm_completion_tx,
        dm_completion_rx,
        pending_dm_inbound: HashMap::new(),
        next_dm_task_id: 0,
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
