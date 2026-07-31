//! 事件循环主体：`EventLoop` 结构、pending 状态、主循环与命令分发。
//!
//! swarm 事件分发在 `swarm_events`，各协议处理在 `rr_protocols` / `org_direct`
//! / `dht` / `gossip`，keepalive tick 编排在 `tick`（均为 `EventLoop` 的
//! 分文件 impl 块）。结构与字段对本模块（`node`）可见，供分文件 impl 访问。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use libp2p::swarm::Swarm;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId, gossipsub, request_response};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::p2p::announce::{NodeAnnounce, NodeAnnounceValidator};
use crate::p2p::behaviour::SparkBehaviour;
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::direct::MinIntervalRateLimiter;
use crate::p2p::envelope::EnvelopeSigner;
use crate::p2p::host::P2pHost;
use crate::p2p::peer_activity::{NodeObservation, PeerActivityStore};
use crate::p2p::peer_targets::{PeerNodeInfo, build_dial_targets, extract_peer_id};
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::api::Command;
use super::{LocalP2PNodeInfo, NowFn, P2pEvent};

pub(super) struct PendingConnect {
    pub(super) node_info: PeerNodeInfo,
    pub(super) targets: VecDeque<String>,
    pub(super) current: Option<String>,
    /// 本次拨号的 [`ConnectionId`]（dial 前从 `DialOpts` 读取）——
    /// OutgoingConnectionError 按它精确归属：unknown_peer_id 拨号失败时
    /// 事件 peer_id=None，按 peer 匹配会让失败永久滞留（对端在线但首
    /// 候选撞 mdns/并发拨号竞争时，connect 等到超时、推送整体降级）
    pub(super) dial_conn_id: Option<libp2p::swarm::ConnectionId>,
    pub(super) tx: oneshot::Sender<Result<()>>,
    pub(super) last_error: Option<String>,
}

pub(super) struct RecoverySession {
    pub(super) remaining: usize,
    pub(super) collected: Vec<PeerNodeInfo>,
    pub(super) tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
}

pub(super) struct ForwardCtx {
    pub(super) channel: request_response::ResponseChannel<String>,
    pub(super) remaining: usize,
    pub(super) collected: Vec<PeerNodeInfo>,
    pub(super) want: usize,
}

pub(super) enum OrgAttemptKind {
    /// org-share 直连推送：ok && syncId 匹配即 true。
    Share { expected_sync_id: String },
    /// org-pull：返回首个可解析响应 JSON。
    Pull,
    /// dm 直连投递（`/spark/dm/1.0.0`）：返回对方应用层应答 JSON。
    Dm,
}

/// org/dm 直连尝试的最终结果通道（按类别直接回传给调用方）。
pub(super) enum OrgTx {
    Share(oneshot::Sender<Result<bool>>),
    Pull(oneshot::Sender<Result<Option<Value>>>),
    Dm(oneshot::Sender<Result<Option<Value>>>),
}

impl OrgTx {
    /// 调用方已放弃等待（投递层超时后 rx 被 drop）——新 attempt 入队时
    /// 据此惰性回收滞留 attempt（拨号无响应等无事件路径下 vec 才有界）
    pub(super) fn is_closed(&self) -> bool {
        match self {
            OrgTx::Share(tx) => tx.is_closed(),
            OrgTx::Pull(tx) => tx.is_closed(),
            OrgTx::Dm(tx) => tx.is_closed(),
        }
    }
}

pub(super) struct OrgAttempt {
    pub(super) kind: OrgAttemptKind,
    pub(super) targets: VecDeque<String>,
    pub(super) current_target: Option<String>,
    pub(super) current_peer: Option<PeerId>,
    pub(super) request_json: String,
    pub(super) in_flight: Option<request_response::OutboundRequestId>,
    /// current_target 的地址是否由本 attempt 实际发起拨号（去重等待者为
    /// false）——dial_next_org_target 的同地址去重只认真实拨号方，
    /// 否则拨号方失败重试时会被等待者误判「已在拨」而全员僵持
    pub(super) dial_issued: bool,
    /// 本次拨号的 [`ConnectionId`]（`DialOpts::connection_id()` 在 dial 前
    /// 读取）——OutgoingConnectionError 按它精确归属到发起拨号的 attempt：
    /// `unknown_peer_id` 拨号失败时事件 peer_id=None，按 peer 匹配会让
    /// 无关失败级联推进所有 attempt 至目标耗尽
    pub(super) dial_conn_id: Option<libp2p::swarm::ConnectionId>,
    pub(super) tx: OrgTx,
}

/// dm 入站异步任务完成消息：(任务 id, 对端 peerId base58, 宿主处理结果)。
pub(super) type DmCompletion = (u64, String, std::result::Result<Value, String>);

impl OrgAttempt {
    /// 地址/重试耗尽：按类别回传终态。
    pub(super) fn finish_exhausted(self) {
        match self.tx {
            OrgTx::Share(tx) => {
                let _ = tx.send(Ok(false));
            }
            OrgTx::Pull(tx) => {
                let _ = tx.send(Ok(None));
            }
            OrgTx::Dm(tx) => {
                let _ = tx.send(Ok(None));
            }
        }
    }
}

pub(super) struct EventLoop<S: StorageBackend> {
    pub(super) swarm: Swarm<SparkBehaviour>,
    pub(super) storage: S,
    pub(super) host: Box<dyn P2pHost>,
    pub(super) keypair: libp2p::identity::Keypair,
    pub(super) signer: EnvelopeSigner,
    pub(super) now_fn: NowFn,
    pub(super) app_version: String,
    pub(super) cmd_rx: mpsc::UnboundedReceiver<Command>,
    pub(super) event_tx: mpsc::UnboundedSender<P2pEvent>,
    pub(super) announce_validator: NodeAnnounceValidator,
    pub(super) exchange_limiter: MinIntervalRateLimiter,
    pub(super) recovery_limiter: MinIntervalRateLimiter,
    pub(super) last_announced_at: i64,
    pub(super) overlay_exchange_cursor: u64,
    pub(super) started_emitted: bool,
    pub(super) port_persisted: bool,
    pub(super) pending_connects: Vec<PendingConnect>,
    pub(super) pending_overlay_dials: HashMap<PeerId, ()>,
    pub(super) version_probe_in_flight: HashSet<PeerId>,
    pub(super) pending_version: HashMap<request_response::OutboundRequestId, PeerId>,
    pub(super) pending_exchange:
        HashMap<request_response::OutboundRequestId, (PeerId, oneshot::Sender<Result<usize>>)>,
    pub(super) pending_recovery: HashMap<request_response::OutboundRequestId, RecoverySession>,
    /// 同一恢复 session 的其余请求 → 首个请求 id。
    pub(super) pending_recovery_extra:
        HashMap<request_response::OutboundRequestId, request_response::OutboundRequestId>,
    pub(super) pending_forward: HashMap<request_response::OutboundRequestId, ForwardCtx>,
    /// 同一转发批次的其余请求 → 首个请求 id。
    pub(super) pending_forward_extra:
        HashMap<request_response::OutboundRequestId, request_response::OutboundRequestId>,
    pub(super) pending_org_attempts: Vec<OrgAttempt>,
    /// node-challenge 应答侧限流。
    pub(super) challenge_limiter: MinIntervalRateLimiter,
    /// dm 应答侧限流（同一对端最小间隔）。
    pub(super) dm_limiter: MinIntervalRateLimiter,
    /// identify 上报的对端协议清单（三层确认第②层）。
    pub(super) peer_protocols: HashMap<PeerId, HashSet<String>>,
    /// 显式 challenge 命令：请求 id →（对端、nonce、调用方等待器）。
    pub(super) pending_challenge: HashMap<
        request_response::OutboundRequestId,
        (PeerId, String, oneshot::Sender<Result<bool>>),
    >,
    /// DHT 三层确认链路发起的 challenge：请求 id →（对端、nonce、待入池 announce）。
    pub(super) pending_challenge_confirm:
        HashMap<request_response::OutboundRequestId, (PeerId, String, NodeAnnounce)>,
    pub(super) pending_dht_put: HashMap<libp2p::kad::QueryId, oneshot::Sender<Result<()>>>,
    pub(super) pending_dht_get:
        HashMap<libp2p::kad::QueryId, oneshot::Sender<Result<Option<Vec<u8>>>>>,
    pub(super) pending_dht_providers:
        HashMap<libp2p::kad::QueryId, oneshot::Sender<Result<Vec<String>>>>,
    /// 本网关职责内提供的 (key → value)，挂 keepalive tick 周期重发（§15）。
    pub(super) provided_records: HashMap<Vec<u8>, Vec<u8>>,
    /// keepalive tick 计数（DHT 节点存在记录按间隔重发）。
    pub(super) dht_tick_counter: u64,
    /// dm 入站异步处理完成通道：任务经 tx 送回结果，事件循环收到后
    /// 按任务 id 找回 ResponseChannel 并 send_response。
    pub(super) dm_completion_tx: mpsc::UnboundedSender<DmCompletion>,
    pub(super) dm_completion_rx: mpsc::UnboundedReceiver<DmCompletion>,
    /// dm 入站进行中：任务 id → ResponseChannel。
    pub(super) pending_dm_inbound: HashMap<u64, request_response::ResponseChannel<String>>,
    pub(super) next_dm_task_id: u64,
    /// plugin-announce 接收侧校验链 + 逐 peer 限流（plugin-dist §8.6）。
    pub(super) plugin_announce_validator: crate::p2p::plugin_announce::PluginAnnounceValidator,
    /// relay 资历阈值（§8.6：传播源连续接入时长下限，默认 72h）。
    pub(super) plugin_announce_tenure_ms: i64,
    /// 各 peer 当前连接建立时刻（资历制依据；断连清零重计）。
    pub(super) peer_connected_since: HashMap<PeerId, i64>,
    /// gossipsub topic → IdentTopic 缓存（构造含字符串哈希；topic 为协议常量集合）。
    pub(super) topic_cache: HashMap<String, gossipsub::IdentTopic>,
}

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn now(&self) -> i64 {
        (self.now_fn)()
    }

    pub(super) fn emit(&self, event: P2pEvent) {
        let _ = self.event_tx.send(event);
    }

    pub(super) fn self_peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }

    pub(super) fn connected_peers(&self) -> HashSet<PeerId> {
        self.swarm.connected_peers().copied().collect()
    }

    pub(super) fn listen_addr_strings(&self) -> Vec<String> {
        self.swarm
            .listeners()
            .chain(self.swarm.external_addresses())
            .map(ToString::to_string)
            .collect()
    }

    pub(super) async fn run(mut self, keepalive_interval: Option<Duration>) {
        use libp2p::futures::StreamExt;
        self.seed_kad_routing();
        let mut keepalive = keepalive_interval.map(tokio::time::interval);
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    if self.handle_command(cmd) {
                        break;
                    }
                }
                Some(completion) = self.dm_completion_rx.recv() => {
                    self.finish_dm_inbound(completion);
                }
                _ = async {
                    match keepalive.as_mut() {
                        Some(interval) => interval.tick().await,
                        None => std::future::pending::<tokio::time::Instant>().await,
                    }
                } => {
                    let stats = self.run_keepalive_tick();
                    self.emit(P2pEvent::KeepaliveTick(stats));
                }
            }
        }
        self.emit(P2pEvent::Stopped);
    }

    /// 返回 true 表示收到 Shutdown。
    fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Broadcast { topic, body, tx } => {
                let _ = tx.send(self.publish_envelope(&topic, body));
            }
            Command::PublishPluginAnnounce { json, tx } => {
                let _ = tx.send(self.publish_plugin_announce_raw(&json));
            }
            Command::AnnounceNow { tx } => {
                let _ = tx.send(self.publish_announce());
            }
            Command::ConnectPeer { node_info, tx } => self.begin_connect(node_info, tx),
            Command::ExchangeWithPeer { peer_id, tx } => self.begin_exchange(&peer_id, tx),
            Command::QueryRecovery {
                token,
                neighbors,
                want,
                tx,
            } => {
                self.begin_recovery_query(&token, &neighbors, want, tx);
            }
            Command::OrgShareDirect {
                node_info,
                payload,
                tx,
            } => {
                self.begin_org_attempt(node_info, payload, OrgTx::Share(tx), true);
            }
            Command::OrgPullRequest {
                node_info,
                request_json,
                tx,
            } => {
                self.begin_org_attempt(
                    node_info,
                    Value::String(request_json),
                    OrgTx::Pull(tx),
                    false,
                );
            }
            Command::DmDirect {
                node_info,
                payload,
                tx,
            } => {
                self.begin_dm_attempt(node_info, payload, tx);
            }
            Command::LocalNodeInfo { tx } => {
                let _ = tx.send(self.local_node_info());
            }
            Command::DhtPutRecord { key, value, tx } => self.begin_dht_put(key, value, tx),
            Command::DhtGetRecord { key, tx } => self.begin_dht_get(key, tx),
            Command::DhtProvide { key, value, tx } => self.begin_dht_provide(key, value, tx),
            Command::DhtGetProviders { key, tx } => self.begin_dht_get_providers(key, tx),
            Command::ChallengePeer { peer_id, tx } => self.begin_challenge(&peer_id, tx),
            Command::Tick { tx } => {
                let _ = tx.send(self.run_keepalive_tick());
            }
            Command::Shutdown => return true,
        }
        false
    }

    fn local_node_info(&mut self) -> LocalP2PNodeInfo {
        let topic = gossipsub::IdentTopic::new(SYNC_TOPIC).hash();
        let subscribers: Vec<String> = self
            .swarm
            .behaviour()
            .gossipsub
            .all_peers()
            .filter(|(_, topics)| topics.contains(&&topic))
            .map(|(peer, _)| peer.to_base58())
            .collect();
        LocalP2PNodeInfo {
            started: true,
            peer_id: Some(self.self_peer_id().to_base58()),
            addresses: self.listen_addr_strings(),
            connected_peers: self
                .connected_peers()
                .iter()
                .map(ToString::to_string)
                .collect(),
            spark_sync_subscribers: subscribers,
        }
    }

    // ------------------------------------------------------------------
    // 连接管理
    // ------------------------------------------------------------------

    pub(super) fn begin_connect(&mut self, node_info: PeerNodeInfo, tx: oneshot::Sender<Result<()>>) {
        // 惰性回收调用方已放弃的滞留项（connect_peer 10s 超时后 rx 被
        // drop；拨号无响应时无事件触发清理，vec 只在新 connect 时有界）
        self.pending_connects.retain(|p| !p.tx.is_closed());
        // 已连接即成功（重拨同一地址会因 TCP 四元组冲突失败，也无必要；
        // TS 侧 libp2p dial 已连接 peer 同样为 no-op 成功）
        if extract_peer_id(&node_info)
            .and_then(|s| s.parse::<PeerId>().ok())
            .is_some_and(|p| self.swarm.is_connected(&p))
        {
            let _ = tx.send(Ok(()));
            return;
        }
        let targets = match build_dial_targets(&node_info) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        let mut pending = PendingConnect {
            node_info,
            targets,
            current: None,
            dial_conn_id: None,
            tx,
            last_error: None,
        };
        if let Some(err) = self.dial_next_connect_target(&mut pending) {
            let info = pending.node_info.clone();
            self.remember_node_observation(&info, NodeObservation::Failure, Some(&err));
            let _ = pending.tx.send(Err(P2pError::Dial(format!(
                "Failed to connect peer by provided addresses: {err}"
            ))));
            return;
        }
        self.pending_connects.push(pending);
    }

    /// 尝试下一个拨号目标；全部耗尽时返回错误文本（由调用方回传终态）。
    pub(super) fn dial_next_connect_target(
        &mut self,
        pending: &mut PendingConnect,
    ) -> Option<String> {
        // 新一轮目标尝试：上一目标（如有）的拨号归属失效
        pending.dial_conn_id = None;
        while let Some(target) = pending.targets.pop_front() {
            match target.parse::<Multiaddr>() {
                Ok(ma) => {
                    let opts = if target.contains("/p2p/") {
                        DialOpts::from(ma)
                    } else {
                        DialOpts::unknown_peer_id().address(ma).build()
                    };
                    let conn_id = opts.connection_id();
                    if self.swarm.dial(opts).is_ok() {
                        pending.current = Some(target);
                        pending.dial_conn_id = Some(conn_id);
                        return None;
                    }
                    pending.last_error = Some(format!("dial rejected: {target}"));
                }
                Err(e) => {
                    pending.last_error = Some(format!("invalid addr {target}: {e}"));
                }
            }
        }
        Some(
            pending
                .last_error
                .clone()
                .unwrap_or_else(|| "no dial targets".to_string()),
        )
    }

    pub(super) fn remember_node_observation(
        &mut self,
        info: &PeerNodeInfo,
        obs: NodeObservation,
        error: Option<&str>,
    ) {
        let now = self.now();
        let mut store = PeerActivityStore::new(&mut self.storage);
        let _ = store.remember_node_info(info, obs, error, now);
    }
}
