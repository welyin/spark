//! 事件循环主体：`EventLoop` 结构、pending 状态、主循环与命令分发。
//!
//! swarm 事件分发在 `swarm_events`，各协议处理在 `rr_protocols` / `org_direct`
//! / `dht` / `gossip`，keepalive tick 编排在 `tick`（均为 `EventLoop` 的
//! 分文件 impl 块）。结构与字段对本模块（`node`）可见，供分文件 impl 访问。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

use libp2p::multiaddr::Protocol;
use libp2p::swarm::Swarm;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId, gossipsub, kad, request_response};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::p2p::announce::{NodeAnnounce, NodeAnnounceValidator};
use crate::p2p::behaviour::SparkBehaviour;
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::direct::MinIntervalRateLimiter;
use crate::p2p::envelope::EnvelopeSigner;
use crate::p2p::host::P2pHost;
use crate::p2p::peer_activity::{NodeObservation, PeerActivityStore};
use crate::p2p::peer_targets::{
    PeerNodeInfo, build_dial_targets, extract_peer_id, is_public_external_addr,
};
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::api::Command;
use super::{LocalP2PNodeInfo, NowFn, P2pEvent};

/// 一批并发拨号中的单个在途目标（M9 分批并发）。
#[derive(Clone, Debug)]
pub(super) struct InFlightDial {
    /// 正在拨号的地址。
    pub(super) addr: String,
    /// 该次拨号的 [`ConnectionId`]（dial 前从 `DialOpts` 读取）——
    /// OutgoingConnectionError 按它精确归属：unknown_peer_id 拨号失败时
    /// 事件 peer_id=None，按 peer 匹配会让失败永久滞留（对端在线但首
    /// 候选撞 mdns/并发拨号竞争时，connect 等到超时、推送整体降级）
    pub(super) conn_id: libp2p::swarm::ConnectionId,
}

pub(super) struct PendingConnect {
    pub(super) node_info: PeerNodeInfo,
    /// 尚未分配进批次的目标地址（按 §2 记分卡/静态优先级排好序）。
    pub(super) targets: VecDeque<String>,
    /// 当前批次中已发起、尚未收场的在途拨号（M9 分批并发：批内任一连通
    /// 即收手，全批失败再开下一批）。
    pub(super) in_flight: Vec<InFlightDial>,
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
    /// 尚未分配进批次的目标地址（按 §2 记分卡/静态优先级排好序）。
    pub(super) targets: VecDeque<String>,
    /// 当前批次在途拨号（M9 分批并发：批内任一连通即收手，全批失败再开
    /// 下一批）。等待者（同地址去重）的 batch 为空、`waiting_base` 非空。
    pub(super) batch: Vec<InFlightDial>,
    pub(super) current_peer: Option<PeerId>,
    pub(super) request_json: String,
    /// 请求已发出（非空说明连接已建立、正在等应答，不再拨号）。
    pub(super) in_flight: Option<request_response::OutboundRequestId>,
    /// batch 是否由本 attempt 实际发起拨号（同地址去重等待者为 false）——
    /// 去重只认真实拨号方，否则拨号方失败重试时会被等待者误判「已在拨」
    /// 而全员僵持
    pub(super) dial_issued: bool,
    /// 等待者登记的等待地址（base 形式）——另一 attempt 正在拨该地址时，
    /// 本 attempt 不重复拨，登记等待；该连接建立时随路发请求，该拨号失败
    /// 时被唤醒自行走目标流程。
    pub(super) waiting_base: Option<String>,
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

    /// 是否仍有拨号/等待活动：批次在途拨号、已发请求、或作为同地址等待者。
    /// 三者皆空说明目标已耗尽，应回传终态。
    pub(super) fn has_dial_activity(&self) -> bool {
        !self.batch.is_empty() || self.in_flight.is_some() || self.waiting_base.is_some()
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
    /// 命令通道克隆（M9：防抖一次性定时器到点后回发 `NetworkChangeFired`）。
    pub(super) cmd_tx: mpsc::UnboundedSender<Command>,
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
    /// DHT 周期重发间隔（tick 计数；桌面默认 240≈4h，移动端 120≈2h）。
    pub(super) dht_republish_ticks: u64,
    /// 网络变化防抖闹钟在跑标记（M9 一次性定时器版）：武装时置 Some，闹钟
    /// 到点回发 `NetworkChangeFired` 时清 None——闹钟自己响、响完销毁，不借
    /// tick 当到期检查器（tick 内零拨号是结构保证）。
    pub(super) pending_network_change: Option<i64>,
    /// 上一 keepalive tick 的网络快照（M9 本地对比）：tick 内对比当前快照，
    /// 变化即武装防抖一次性定时器（零网络开销、不依赖壳层）。首 tick 只记录
    /// 基线不触发。
    pub(super) last_network_snapshot: Option<Vec<String>>,
    /// 优先类目 peer 的重新发现状态机（peer-rediscovery §4.3/§4.8）。
    pub(super) rediscovery_states: HashMap<PeerId, super::rediscovery::RediscoveryState>,
    /// 竞速 DHT 查询：QueryId → 目标 peer（区分于普通 pending_dht_get 查询）。
    pub(super) rediscovery_dht_queries: HashMap<kad::QueryId, PeerId>,
    /// 竞速场景：DHT 命中但 peer 未连接时暂存的 announce，待 ConnectionEstablished 后确认。
    pub(super) pending_rediscovery_confirm: HashMap<PeerId, crate::p2p::announce::NodeAnnounce>,
    /// 活跃 relay 预约（peer-rediscovery §4.6）：电路地址追加进 announce 列表。
    pub(super) relay_reservations: Vec<super::relay_manager::RelayReservation>,
    /// relay 预约请求 in-flight：已发起尚未收到 ReservationReqAccepted/失败
    /// 的 relay peer，用于避免重复请求（peer-rediscovery §4.6.2）。
    pub(super) relay_reservations_inflight: std::collections::HashSet<PeerId>,
    /// dm 入站异步处理完成通道：任务经 tx 送回结果，事件循环收到后
    /// 按任务 id 找回 ResponseChannel 并 send_response。
    pub(super) dm_completion_tx: mpsc::UnboundedSender<DmCompletion>,
    pub(super) dm_completion_rx: mpsc::UnboundedReceiver<DmCompletion>,
    /// org/dm 直连单目标拨号的应用层超时通道：每次实际拨号 spawn 一个
    /// 定时任务送回本次的 [`libp2p::swarm::ConnectionId`]，事件循环收到后
    /// 按 OutgoingConnectionError 同口径推进 attempt（黑洞地址不再烧光
    /// 外层总预算）；迟到的超时消息无 attempt 匹配即忽略。
    pub(super) dial_timeout_tx: mpsc::UnboundedSender<libp2p::swarm::ConnectionId>,
    pub(super) dial_timeout_rx: mpsc::UnboundedReceiver<libp2p::swarm::ConnectionId>,
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

/// 一块网卡的可拨号信息（自 `if_addrs::Interface` 抽取，便于单测构造）。
#[derive(Clone, Copy, Debug)]
pub(super) struct NetInterface {
    pub(super) ip: IpAddr,
    pub(super) is_loopback: bool,
    pub(super) is_up: bool,
}

/// 读取本机网卡清单；失败（如平台不支持）返回空，此时通配 listener 无法展开。
fn local_interfaces() -> Vec<NetInterface> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .iter()
        .map(|iface| NetInterface {
            ip: iface.ip(),
            is_loopback: iface.is_loopback(),
            is_up: iface.is_oper_up(),
        })
        .collect()
}

/// 通配 listener（0.0.0.0/::）展开为「每块非 loopback 且运行中的网卡一个
/// 具体地址」（同协议族同端口，如 `/ip4/192.168.1.2/tcp/4001`）；具体
/// listener 原样保留。返回顺序：具体地址在前、展开地址在后；整体去重。
pub(super) fn expand_wildcard_listeners(
    listeners: &[Multiaddr],
    interfaces: &[NetInterface],
) -> Vec<Multiaddr> {
    let mut concrete = Vec::new();
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for listener in listeners {
        match wildcard_ip(listener) {
            Some(wildcard) => {
                for iface in interfaces {
                    // loopback 与未运行网卡不参与展开；仅同协议族替换
                    // （v4 通配配 v4 网卡，v6 同理）。
                    if iface.is_loopback
                        || !iface.is_up
                        || iface.ip.is_ipv4() != wildcard.is_ipv4()
                    {
                        continue;
                    }
                    let addr = replace_ip(listener, iface.ip);
                    if seen.insert(addr.clone()) {
                        expanded.push(addr);
                    }
                }
            }
            None => {
                if seen.insert(listener.clone()) {
                    concrete.push(listener.clone());
                }
            }
        }
    }
    concrete.extend(expanded);
    concrete
}

/// 通配 listener 的首段 IP（0.0.0.0/::）；非通配返回 None。
fn wildcard_ip(addr: &Multiaddr) -> Option<IpAddr> {
    match addr.iter().next() {
        Some(Protocol::Ip4(ip)) if ip.is_unspecified() => Some(ip.into()),
        Some(Protocol::Ip6(ip)) if ip.is_unspecified() => Some(ip.into()),
        _ => None,
    }
}

/// 替换 multiaddr 首段 IP，其余段（端口/协议）原样保留。
fn replace_ip(addr: &Multiaddr, ip: IpAddr) -> Multiaddr {
    let mut out = Multiaddr::empty();
    for (index, protocol) in addr.iter().enumerate() {
        out.push(match index {
            0 => match ip {
                IpAddr::V4(v4) => Protocol::Ip4(v4),
                IpAddr::V6(v6) => Protocol::Ip6(v6),
            },
            _ => protocol,
        });
    }
    out
}

/// 地址首段是否为 IPv6 link-local（`fe80::/10`，即首段 0xfe80..=0xfebf）。
/// 链路本地地址仅在网段内有效，发布到 gossip/DHT 对端不可拨，应剔除
/// （peer-rediscovery §6 WP1 地址过滤）。
fn is_ipv6_link_local(address: &str) -> bool {
    let Ok(addr) = address.parse::<Multiaddr>() else {
        return false;
    };
    match addr.iter().next() {
        Some(Protocol::Ip6(ip)) => {
            let seg = ip.segments();
            seg[0] >= 0xfe80 && seg[0] <= 0xfebf
        }
        _ => false,
    }
}

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn now(&self) -> i64 {        (self.now_fn)()
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

    /// 主路径主动重拨（§4.1.2 ④）：切网后遍历优先类目 peer，对未连接的
    /// 用邻居池缓存地址发起重拨（缓存地址切网后仍大概率有效，无需等 DHT
    /// 兜底；无缓存地址则触发一次竞速以走 DHT 查询）。
    pub(super) fn redial_priority_peers(&mut self) {
        let now = self.now();
        let priority_peers: Vec<String> = {
            let mut store =
                crate::p2p::priority_peers::PriorityPeerStore::new(&mut self.storage);
            store.list().unwrap_or_default()
        };
        for pid in priority_peers {
            let Ok(peer) = pid.parse::<PeerId>() else {
                continue;
            };
            if self.swarm.is_connected(&peer) {
                continue;
            }
            // 已有一轮竞速在途的，让状态机继续推进，不重复竞速
            if matches!(
                self.rediscovery_states.get(&peer),
                Some(super::rediscovery::RediscoveryState::Racing { .. })
            ) {
                continue;
            }
            // 有缓存地址 → 直接重拨；否则交给 start_rediscovery 走 DHT 竞速
            let cached_addrs: Vec<Multiaddr> = {
                let mut store =
                    crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
                store
                    .get(&peer.to_base58())
                    .ok()
                    .flatten()
                    .map(|r| r.addresses)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .collect()
            };
            if !cached_addrs.is_empty() {
                // allocate_new_port：拨号源端口用 OS 临时端口，避免复用监听端口
                // [::]:15002 与多 listener 冲突 EADDRINUSE（PC 主动拨号瘫痪）。
                // 止血：dcutr 未接入（§7.1 阶段 B），relay 不依赖源端口；
                // 待 dcutr 接入时重新评估端口复用（wiki §4.6.3/§7.1）。
                let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                    .addresses(cached_addrs)
                    .allocate_new_port()
                    .build();
                let _ = self.swarm.dial(opts);
                // 标记竞速中，等待连接结果（成功则收敛，失败由 ConnectionClosed 兜底）
                self.rediscovery_states.insert(
                    peer,
                    super::rediscovery::RediscoveryState::Racing {
                        started_at: now,
                        dht_query_id: None,
                    },
                );
            } else {
                // 无缓存：直接触发竞速（本地拨号无地址 + DHT 查询并行）
                self.start_rediscovery(peer);
            }
        }
    }

    /// 覆盖网孤岛自举（connection-policy M8，**纯事件驱动**）：仅在 0 连接时
    /// 从邻居池按排序拨一轮（≤[`OVERLAY_TICK_DIAL_BUDGET`] 个），失败即沉默——
    /// 不做任何周期重试，等下一个事件（启动/网络变更确认/学到新邻居）再拨。
    ///
    /// 唯一合法目的是 DHT 自举：0 连接时 DHT 查询发不出去，必须先拨通任意
    /// Spark 节点；有 ≥1 连接 DHT 已能工作，直接跳过。候选排序由
    /// [`OverlayPeerStore::sample_dial_candidates`] 完成（最近成功/见过优先，
    /// 失败沉底）——去重靠「无周期触发」，不靠失败记忆。
    pub(super) fn bootstrap_overlay_dial(&mut self) {
        let connected = self.connected_peers();
        if !connected.is_empty() {
            return;
        }
        let now = self.now();
        let self_id = self.self_peer_id().to_base58();
        let mut exclude: HashSet<String> = HashSet::new();
        exclude.insert(self_id);
        let candidates = {
            let mut store = crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
            store
                .sample_dial_candidates(&exclude, now, crate::p2p::constants::OVERLAY_TICK_DIAL_BUDGET)
                .unwrap_or_default()
        };
        for candidate in candidates {
            let Ok(peer) = candidate.peer_id.parse::<PeerId>() else {
                continue;
            };
            let addrs: Vec<Multiaddr> = candidate
                .addresses
                .iter()
                .filter_map(|a| a.parse().ok())
                .collect();
            if addrs.is_empty() {
                continue;
            }
            // allocate_new_port：复用监听端口 [::]:15002 会与多 listener 冲突
            // EADDRINUSE，用 OS 临时端口恢复 PC 主动拨号。止血：dcutr 未接入
            // （§7.1 阶段 B），relay 不依赖源端口；待 dcutr 接入时重新评估端口
            // 复用（wiki §4.6.3/§7.1）。
            let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(addrs)
                .allocate_new_port()
                .build();
            if self.swarm.dial(opts).is_ok() {
                self.pending_overlay_dials.insert(peer, ());
            }
        }
    }

    pub(super) fn listen_addr_strings(&self) -> Vec<String> {
        let listeners: Vec<Multiaddr> = self.swarm.listeners().cloned().collect();
        // 通配 listener（0.0.0.0/::）对扫码名片不可拨，展开为本机可用网卡
        // 的具体地址；external_addresses 只保留公网可达段（S2，root fix）。
        let interfaces = local_interfaces();
        let mut addrs: Vec<String> = expand_wildcard_listeners(&listeners, &interfaces)
            .into_iter()
            .map(|addr| addr.to_string())
            .collect();
        // external 段只并入公网可达地址（S1 `is_public_external_addr`）：observe /
        // AutoNAT 会把本机/其它实例的私有 LAN IP 误认成 external，广播出去对端
        // remember 污染 → WrongPeerId。expand 出来的 LAN concrete 地址不动——同
        // LAN 互达必需，且已由本机网卡展开覆盖，external 里带私网地址是重复且错误。
        for ext in self.swarm.external_addresses() {
            if is_public_external_addr(ext) {
                addrs.push(ext.to_string());
            }
        }
        // 追加 relay 电路地址（格式：/p2p/<relayPeer>/p2p-circuit，peer-rediscovery §4.6）
        for reservation in &self.relay_reservations {
            addrs.push(reservation.circuit_addr.to_string());
        }
        // 过滤 IPv6 link-local（fe80::/10 仅链路本地有效，发布出去对端不可拨）；
        // 私有 IPv4 保留（同 LAN 场景有用）。
        addrs.retain(|a| !is_ipv6_link_local(a));
        addrs
    }

    /// 本机当前监听地址集合（M9 自过滤：不拨自己的监听地址，多实例同机开发的
    /// ::1 / 本机 LAN IP 污染源）。
    pub(super) fn self_listen_addr_set(&self) -> HashSet<String> {
        self.listen_addr_strings().into_iter().collect()
    }

    /// 发布侧兜底（S7）：剔除黑名单命中且在 TTL 内的地址，防止把自己学到的污染
    /// 地址广播出去（wrong-peer-id-address-pollution §2.9）。根治（S2）已止源头，
    /// 此过滤防旧污染地址在根治后短暂残留。返回剔除后的地址列表。
    pub(super) fn drop_blacklisted(&mut self, addrs: Vec<String>) -> Vec<String> {
        let now = self.now();
        let mut bl = crate::p2p::addr_blacklist::AddrBlacklistStore::new(&mut self.storage);
        addrs
            .into_iter()
            .filter(|a| !bl.is_blocked(a, now).unwrap_or(false))
            .collect()
    }

    /// 网络变化探测专用快照：只取**本机网卡展开的监听地址**（通配 listener
    /// 按网卡展开，过滤 link-local），排序去重。
    ///
    /// 刻意排除 `listen_addr_strings()` 里的两类易变成分：identify 观察到的
    /// external_addresses 与 relay 电路地址——它们是**连接的结果**（对端每次
    /// 连上/断开都会变），不是网络面变化；混入会把快照对比变成每 tick 误报，
    /// 防抖每轮武装、每轮触发 redial_priority_peers 全量重拨（真机实测）。
    pub(super) fn network_snapshot(&self) -> Vec<String> {
        let listeners: Vec<Multiaddr> = self.swarm.listeners().cloned().collect();
        let interfaces = local_interfaces();
        let mut addrs: Vec<String> = expand_wildcard_listeners(&listeners, &interfaces)
            .into_iter()
            .map(|addr| addr.to_string())
            .filter(|a| !is_ipv6_link_local(a))
            .collect();
        // 排序去重：Vec 比较对顺序敏感，接口枚举顺序不稳定会造成假变化
        addrs.sort();
        addrs.dedup();
        addrs
    }

    /// 武装网络变化防抖**一次性定时器**（M9）：基线快照随 `NetworkChangeFired`
    /// 命令携带，tokio sleep 到点回发——闹钟自己响，响完即销毁，无循环无重试。
    /// 已有闹钟在跑时不重复武装（抖动窗口内多个信号合并为一次）。
    pub(super) fn arm_network_change_timer(&mut self) {
        if self.pending_network_change.is_some() {
            return;
        }
        self.pending_network_change = Some(self.now());
        let base = self.network_snapshot();
        let tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                crate::p2p::constants::NETWORK_CHANGE_DEBOUNCE_MS as u64,
            ))
            .await;
            let _ = tx.send(Command::NetworkChangeFired { base });
        });
    }

    /// M9 本地地址变化探测（keepalive tick 调用，网络变化重连 A+B 的 B 兜底）：
    /// 对比 `network_snapshot()` 与上轮快照，变化则武装防抖一次性定时器。
    /// 首 tick 只记录基线不触发。零网络开销、不依赖壳层。
    pub(super) fn detect_local_network_change(&mut self) {
        let current = self.network_snapshot();
        if let Some(base) = &self.last_network_snapshot
            && base != &current
        {
            self.arm_network_change_timer();
        }
        self.last_network_snapshot = Some(current);
    }

    /// 读取某 peer 的地址记分卡（M9，供拨号目标排序）。
    pub(super) fn addr_meta_for(&mut self, peer_id: &str) -> HashMap<String, crate::p2p::overlay_store::AddrScore> {
        let mut store = crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
        store
            .get(peer_id)
            .ok()
            .flatten()
            .map(|r| r.addr_meta)
            .unwrap_or_default()
    }

    pub(super) async fn run(mut self, keepalive_interval: Option<Duration>) {
        use libp2p::futures::StreamExt;
        self.seed_kad_routing();
        // 启动即孤岛自举一轮（M8）：0 连接时 DHT 查询发不出去，先拨邻居池
        // 队首候选；失败沉默，等网络变更/学到新邻居等下个事件再拨。
        self.bootstrap_overlay_dial();
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
                Some(conn_id) = self.dial_timeout_rx.recv() => {
                    self.fail_org_dial(conn_id);
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
    pub(super) fn handle_command(&mut self, cmd: Command) -> bool {
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
            Command::DisconnectPeer { peer_id, tx } => {
                let result = match peer_id.parse::<PeerId>() {
                    Ok(p) => {
                        if self.swarm.is_connected(&p) {
                            let _ = self.swarm.disconnect_peer_id(p);
                        }
                        Ok(())
                    }
                    Err(e) => Err(P2pError::Dial(format!("invalid peer id: {e}"))),
                };
                let _ = tx.send(result);
            }
            Command::NetworkChanged => {
                // 武装一次性防抖定时器：到点回发 NetworkChangeFired 统一处理
                // （独立 tokio 定时，不借 keepalive tick 当到期检查器——tick
                // 内零拨号是结构保证）
                self.arm_network_change_timer();
            }
            Command::NetworkChangeFired { base } => {
                self.pending_network_change = None;
                let current = self.network_snapshot();
                if base != current {
                    // ③④⑤ 重发布 announce + DHT
                    let _ = self.publish_announce();
                    self.publish_node_presence_record();
                    // ⑥ 重建 relay 预约（旧预约随旧连接失效）
                    self.ensure_relay_reservations();
                    // 主路径（§4.1.2 ④）：主动重拨优先类目 peer（自设备/好友）——
                    // 缓存地址在切网后仍有较大概率有效，无需等被动 DHT 兜底。
                    self.redial_priority_peers();
                    // 覆盖网孤岛自举（M8 事件驱动）：切网后地址面刷新，若处于
                    // 孤岛（0 连接）按排序补拨一轮，失败沉默到下个事件。
                    self.bootstrap_overlay_dial();
                }
            }
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
        // 出站抑制：目标 peer 已被撤销时直接失败，不进入拨号。
        if let Some(peer_id) = extract_peer_id(&node_info) {
            if self.host.is_revoked_peer(&peer_id) {
                let _ = tx.send(Err(P2pError::Dial(format!(
                    "peer {peer_id} is revoked"
                ))));
                return;
            }
        }
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
        // M9：带地址记分卡排序 + 自过滤（不拨本机监听地址）
        let addr_meta = extract_peer_id(&node_info)
            .map(|pid| self.addr_meta_for(&pid))
            .unwrap_or_default();
        let self_addrs = self.self_listen_addr_set();
        let targets = match build_dial_targets(&node_info, Some(&addr_meta), &self_addrs) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        let mut pending = PendingConnect {
            node_info,
            targets,
            in_flight: Vec::new(),
            tx,
            last_error: None,
        };
        self.fill_connect_batch(&mut pending);
        if pending.in_flight.is_empty() {
            // 首批即无可拨目标（全部无效地址/被拒）→ 立即失败
            let err = pending
                .last_error
                .clone()
                .unwrap_or_else(|| "no dial targets".to_string());
            let info = pending.node_info.clone();
            self.remember_node_observation(&info, NodeObservation::Failure, Some(&err));
            let _ = pending.tx.send(Err(P2pError::Dial(format!(
                "Failed to connect peer by provided addresses: {err}"
            ))));
            return;
        }
        self.pending_connects.push(pending);
    }

    /// 从剩余目标填满当前批次（至多 [`DIAL_BATCH_SIZE`] 个在途拨号）。
    /// 每目标一拨（`DialOpts::unknown_peer_id().address(ma)`，含 /p2p 尾段原样
    /// 拨号），记录各自的 [`ConnectionId`] 供失败按它精确归属。地址无效/被
    /// 拒只记 last_error 继续填，不中止批次。
    pub(super) fn fill_connect_batch(&mut self, pending: &mut PendingConnect) {
        while pending.in_flight.len() < crate::p2p::constants::DIAL_BATCH_SIZE {
            let Some(target) = pending.targets.pop_front() else {
                break;
            };
            match target.parse::<Multiaddr>() {
                Ok(ma) => {
                    // allocate_new_port：复用监听端口 [::]:15002 会与多 listener
                    // 冲突 EADDRINUSE，用 OS 临时端口恢复 PC 主动拨号。
                    // 止血：dcutr 未接入（§7.1 阶段 B），relay 不依赖源端口；
                    // 待 dcutr 接入时重新评估端口复用（wiki §4.6.3/§7.1）。
                    let opts = DialOpts::unknown_peer_id()
                        .address(ma)
                        .allocate_new_port()
                        .build();
                    let conn_id = opts.connection_id();
                    if self.swarm.dial(opts).is_ok() {
                        pending.in_flight.push(InFlightDial {
                            addr: target,
                            conn_id,
                        });
                    } else {
                        pending.last_error = Some(format!("dial rejected: {target}"));
                    }
                }
                Err(e) => {
                    pending.last_error = Some(format!("invalid addr {target}: {e}"));
                }
            }
        }
    }

    /// connect 命令单目标拨号失败/超时归属：移除该 conn_id 对应的在途目标；
    /// 本批全败（in_flight 空）时填下一批；再无目标 → 返回错误文本（终态）。
    /// 返回 `Some(err)` 表示目标已耗尽，调用方回传终态。
    pub(super) fn fail_connect_dial(
        &mut self,
        pending: &mut PendingConnect,
        connection_id: libp2p::swarm::ConnectionId,
        error: &str,
    ) -> Option<String> {
        pending.last_error = Some(error.to_string());
        let before = pending.in_flight.len();
        pending.in_flight.retain(|d| d.conn_id != connection_id);
        if pending.in_flight.len() == before {
            // 该 conn_id 已不在批次（如连接已建立被收手时清理）：非本轮失败
            return None;
        }
        if pending.in_flight.is_empty() {
            // 本批全败 → 开下一批
            self.fill_connect_batch(pending);
            if pending.in_flight.is_empty() {
                return Some(
                    pending
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "no dial targets".to_string()),
                );
            }
        }
        None
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


#[cfg(test)]
mod wildcard_tests {
    //! 通配 listener 展开单测（纯函数，不依赖真实网卡）。
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    fn iface(ip: IpAddr, is_loopback: bool, is_up: bool) -> NetInterface {
        NetInterface {
            ip,
            is_loopback,
            is_up,
        }
    }

    fn addr(text: &str) -> Multiaddr {
        text.parse().expect("valid multiaddr")
    }

    fn strings(addrs: Vec<Multiaddr>) -> Vec<String> {
        addrs.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn wildcard_expands_to_each_usable_interface() {
        let listeners = vec![addr("/ip4/0.0.0.0/tcp/4001")];
        let interfaces = vec![
            iface(IpAddr::V4(Ipv4Addr::new(192, 168, 31, 134)), false, true),
            iface(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)), false, true),
        ];
        assert_eq!(
            strings(expand_wildcard_listeners(&listeners, &interfaces)),
            vec![
                "/ip4/192.168.31.134/tcp/4001".to_string(),
                "/ip4/10.0.0.8/tcp/4001".to_string(),
            ]
        );
    }

    #[test]
    fn concrete_listeners_kept_first_and_not_reexpanded() {
        let listeners = vec![
            addr("/ip4/1.2.3.4/tcp/4001"),
            addr("/ip4/0.0.0.0/tcp/4001"),
        ];
        let interfaces = vec![iface(
            IpAddr::V4(Ipv4Addr::new(192, 168, 31, 134)),
            false,
            true,
        )];
        assert_eq!(
            strings(expand_wildcard_listeners(&listeners, &interfaces)),
            vec![
                "/ip4/1.2.3.4/tcp/4001".to_string(),
                "/ip4/192.168.31.134/tcp/4001".to_string(),
            ]
        );
    }

    #[test]
    fn loopback_and_down_interfaces_not_used() {
        let listeners = vec![addr("/ip4/0.0.0.0/tcp/4001")];
        let interfaces = vec![
            iface(IpAddr::V4(Ipv4Addr::LOCALHOST), true, true),
            iface(IpAddr::V4(Ipv4Addr::new(192, 168, 31, 134)), false, false),
        ];
        assert!(expand_wildcard_listeners(&listeners, &interfaces).is_empty());
    }

    #[test]
    fn ipv6_wildcard_only_expands_to_ipv6_interfaces() {
        let listeners = vec![addr("/ip6/::/tcp/4001")];
        let interfaces = vec![
            iface(IpAddr::V4(Ipv4Addr::new(192, 168, 31, 134)), false, true),
            iface(IpAddr::V6(Ipv6Addr::LOCALHOST), true, true),
            iface(
                IpAddr::V6("2408:8207:1::1".parse().unwrap()),
                false,
                true,
            ),
        ];
        assert_eq!(
            strings(expand_wildcard_listeners(&listeners, &interfaces)),
            vec!["/ip6/2408:8207:1::1/tcp/4001".to_string()]
        );
    }

    #[test]
    fn duplicate_expansions_deduped() {
        let listeners = vec![
            addr("/ip4/0.0.0.0/tcp/4001"),
            addr("/ip4/0.0.0.0/tcp/4001"),
        ];
        let interfaces = vec![iface(
            IpAddr::V4(Ipv4Addr::new(192, 168, 31, 134)),
            false,
            true,
        )];
        assert_eq!(
            strings(expand_wildcard_listeners(&listeners, &interfaces)),
            vec!["/ip4/192.168.31.134/tcp/4001".to_string()]
        );
    }
}
