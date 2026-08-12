//! swarm 事件分发：连接生命周期（拨号结果记账、connect/org 直连匹配、版本
//! 探测触发、端口写回）与 behaviour 事件（gossip 分流、mdns/identify 入池、
//! kad 查询汇总、各 request-response 协议的入站/出站钩子）。

use std::collections::HashSet;

use libp2p::swarm::{ConnectionId, SwarmEvent};
use libp2p::{PeerId, gossipsub, identify, kad, mdns, request_response};
use serde_json::Value;

use crate::p2p::P2pError;
use crate::p2p::behaviour::SparkBehaviourEvent;
use crate::p2p::constants::{OVERLAY_TOPIC, PLUGIN_ANNOUNCE_TOPIC, P2P_LISTEN_WS_PORT};
use crate::p2p::listen_port;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::peer_activity::{NodeObservation, PeerActivityStore};
use crate::p2p::peer_targets::extract_peer_id;
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::{EventLoop, OrgAttemptKind};

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn handle_swarm_event(&mut self, event: SwarmEvent<SparkBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { .. } => {
                if !self.port_persisted {
                    let addrs = self.listen_addr_strings();
                    if let Some(port) = listen_port::parse_ws_listen_port(&addrs)
                        && self
                            .storage
                            .put(P2P_LISTEN_WS_PORT, &port.to_string())
                            .is_ok()
                    {
                        self.port_persisted = true;
                        self.emit(P2pEvent::ListenPortPersisted { port });
                    }
                }
                if !self.started_emitted {
                    self.started_emitted = true;
                    // 打印编译时间戳：联调时确认跑的是否新代码（build.rs 注入）
                    eprintln!(
                        "[p2p] node build_time={} peer_id={}",
                        env!("SPARK_BUILD_TIME"),
                        self.self_peer_id().to_base58()
                    );
                    self.emit(P2pEvent::Started {
                        peer_id: self.self_peer_id().to_base58(),
                        listen_addresses: self.listen_addr_strings(),
                    });
                }
            }
            SwarmEvent::ExternalAddrConfirmed { .. } => {
                // 地址变化（UPnP 映射、relay 预约）→ 立即补发通告 + DHT 记录
                //（peer-rediscovery §4.2：外部地址确认同时触发 DHT 重发）
                let _ = self.publish_announce();
                self.publish_node_presence_record();
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                let now = self.now();
                {
                    let mut store = PeerActivityStore::new(&mut self.storage);
                    let _ = store.mark_connected(&peer_id.to_base58(), now);
                }
                // 连接沉淀进覆盖网邻居池：仅出站（dialer）方向的远端地址入池——
                // 它经我们成功拨号验证可达；入站（listener）方向的 remote 地址
                // 是对端 NAT 源 IP:临时端口，回拨必败，入池会占据 DM 拨号队首
                // 烧光外层预算（merge_neighbor_addresses 邻居池地址在前）。
                // 入站方向以空地址列表 remember：保留 peer 存在性与 lastSeen
                // 记账，不污染拨号候选。
                let remote_addr = endpoint.get_remote_address().to_string();
                // 自过滤上下文：在借用 storage 前取好（避免 mutable/immutable 冲突）
                let self_id = self.self_peer_id().to_base58();
                let self_addrs = self.self_listen_addr_set();
                {
                    let dialable_addrs: &[String] = if endpoint.is_dialer() {
                        std::slice::from_ref(&remote_addr)
                    } else {
                        &[]
                    };
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        dialable_addrs,
                        OverlayPeerSource::Connect,
                        false,
                        now,
                        Some(&self_id),
                        &self_addrs,
                    );
                    // M9 success 证据：dialer 方向真实连上的 remote addr 记成功分
                    //（listener 方向是 NAT 临时端口，沿用现口径只刷 last_seen）
                    if endpoint.is_dialer() {
                        let _ = store.mark_addr_success(&peer_id.to_base58(), &remote_addr, now);
                    }
                }
                // 覆盖网自举结果记账：成功标记 last_dial_result=success，
                // 候选排序提到队首（M8 排序制，无失败名单/退避状态）。
                if self.pending_overlay_dials.remove(&peer_id).is_some() {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.mark_dial_result(&peer_id.to_base58(), true);
                }
                // 已连接对端地址灌进 kad 路由表（identify 交换前的兜底）
                if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                    kad.add_address(&peer_id, endpoint.get_remote_address().clone());
                }
                // connect 命令匹配（M9 分批并发）：连接成功即收手（清空本批
                // 其余在途拨号），按 peerId 或批内任一地址匹配
                let remote = remote_addr.clone();
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        let expected =
                            extract_peer_id(&p.node_info).and_then(|s| s.parse::<PeerId>().ok());
                        expected == Some(peer_id)
                            || p.in_flight.iter().any(|d| {
                                remote == d.addr
                                    || remote.starts_with(&format!("{}/", d.addr))
                                    || d.addr.starts_with(&remote)
                            })
                    };
                    if matched {
                        let mut done = self.pending_connects.remove(i);
                        // 收手：取消本批其余在途拨号（清空 batch，其迟到失败
                        // 因 conn_id 已不在批次而不影响本 attempt 的成功结果）
                        done.in_flight.clear();
                        let info = done.node_info.clone();
                        self.remember_node_observation(&info, NodeObservation::Success, None);
                        let _ = done.tx.send(Ok(()));
                    } else {
                        i += 1;
                    }
                }
                // org/dm 直连尝试匹配：连接成功即发请求
                // （in_flight 非空说明请求已发出——双连接并存时不重复发）
                let mut j = 0;
                while j < self.pending_org_attempts.len() {
                    let matched = {
                        let a = &self.pending_org_attempts[j];
                        a.in_flight.is_none()
                            && (a.batch.iter().any(|d| {
                                remote == d.addr
                                    || remote.starts_with(&format!("{}/", d.addr))
                                    || d.addr.starts_with(&remote)
                            }) || a.current_peer == Some(peer_id))
                    };
                    if matched {
                        let attempt = &mut self.pending_org_attempts[j];
                        // dm 尝试走 /spark/dm/1.0.0，org-share/pull 走 /spark/org-share/1.0.0
                        let request_id = match attempt.kind {
                            OrgAttemptKind::Dm => self
                                .swarm
                                .behaviour_mut()
                                .dm_rr
                                .send_request(&peer_id, attempt.request_json.clone()),
                            _ => self
                                .swarm
                                .behaviour_mut()
                                .org_share_rr
                                .send_request(&peer_id, attempt.request_json.clone()),
                        };
                        attempt.in_flight = Some(request_id);
                        attempt.current_peer = Some(peer_id);
                        // 收手：连接已建立，清空本批其余在途拨号（避免迟到失败
                        // 误推进；等待者随路发请求，batch 本就为空）
                        attempt.batch.clear();
                        // 不 break：同地址去重的等待 attempt 排在本条之后，也要
                        // 随这路已建立连接发出请求（in_flight 非空检查已防
                        // 双连接重复发）
                    }
                    j += 1;
                }
                if num_established.get() == 1 {
                    // relay 资历制依据（plugin-dist §8.6）：记录接入时刻
                    self.peer_connected_since.insert(peer_id, now);
                    self.emit(P2pEvent::PeerConnected {
                        peer_id: peer_id.to_base58(),
                    });
                    self.host.on_peer_connected(&peer_id.to_base58());
                }
                // 竞速场景：连接建立后完成三层确认（peer-rediscovery §4.3）
                self.complete_rediscovery_confirm(peer_id);
                // 版本探测（in-flight 去重）
                self.begin_version_probe(peer_id);
            }
            SwarmEvent::ConnectionClosed {
                peer_id, num_established, ..
            } => {
                if num_established == 0 {
                    let now = self.now();
                    // 断连资历清零（§8.6：重接重新熬资历）
                    self.peer_connected_since.remove(&peer_id);
                    let mut store = PeerActivityStore::new(&mut self.storage);
                    let _ = store.mark_disconnected(&peer_id.to_base58(), now);
                    self.emit(P2pEvent::PeerDisconnected {
                        peer_id: peer_id.to_base58(),
                    });
                    // relay 连接断开 → 移除对应预约并尝试补充（peer-rediscovery §4.6.2）
                    self.on_relay_connection_lost(peer_id);
                    // M4 删断线自动竞速（connection-policy §5.3）：断线后不再
                    // 立即 fast-redial/并行竞速——等下次发送时懒拨号或对端拨过来。
                    // 本机网络恢复事件的重拨（redial_priority_peers）仍保留。
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, connection_id, error, .. } => {
                let peer = peer_id.map(|p| p.to_base58()).unwrap_or_else(|| "(unknown)".to_string());
                eprintln!(
                    "[p2p] OutgoingConnectionError: peer={peer} conn_id={connection_id:?} error={error:?}"
                );
                // M9 确定性错误硬删：WrongPeerId（尤其 obtained=本机）或目标地址
                // 命中本机监听地址 → 从目标 peer 记录删除此地址，防死地址累积
                self.hard_delete_on_deterministic_failure(connection_id, &error);
                // connect 命令：失败则试下一目标。按 ConnectionId 精确归属
                // （同 org attempt 口径）——候选 1 的 unknown_peer_id 拨号失败
                // 时 peer_id=None，按 peer 匹配会失配滞留：对端在线但首候选
                // 撞 mdns/并发拨号竞争时，connect 空等超时、推送整体降级
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        p.in_flight.iter().any(|d| d.conn_id == connection_id)
                    };
                    if matched {
                        let mut p = self.pending_connects.remove(i);
                        match self.fail_connect_dial(&mut p, connection_id, &error.to_string()) {
                            None => self.pending_connects.push(p),
                            Some(err) => {
                                let info = p.node_info.clone();
                                self.remember_node_observation(
                                    &info,
                                    NodeObservation::Failure,
                                    Some(&err),
                                );
                                let _ = p.tx.send(Err(P2pError::Dial(format!(
                                    "Failed to connect peer by provided addresses: {err}"
                                ))));
                            }
                        }
                    } else {
                        i += 1;
                    }
                }
                // org 尝试：失败试下一目标。按 ConnectionId 精确归属——
                // 候选 1 用 unknown_peer_id 拨原始地址，失败事件 peer_id=None，
                // 若按 peer/地址模糊匹配，一个 attempt 的失败会误推进同 peer
                // 的所有 attempt（含未拨号的等待者），级联耗尽目标
                self.fail_org_dial(connection_id);
                // 覆盖网自举失败：记账 last_dial_result=failure，候选排序自然
                // 沉底（M8）——不记退避、不进名单；无周期触发即不会重拨，等
                // 下个事件（网络变更/新邻居/发送懒拨号）再按排序拨一轮。
                if let Some(peer) = peer_id
                    && self.pending_overlay_dials.remove(&peer).is_some()
                {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.mark_dial_result(&peer.to_base58(), false);
                }
                // 竞速拨号失败（peer-rediscovery §4.3/N2）：DHT 命中后拨号待确认
                // 阶段全部失败时计一次失败进退避并清理暂存——否则状态永卡
                // Racing、暂存 announce 泄漏（函数内部按 Racing+stash 归属，
                // 普通拨号失败不受影响）
                if let Some(peer) = peer_id {
                    self.on_rediscovery_dial_failed(peer);
                }
            }
            SwarmEvent::ListenerClosed {
                listener_id,
                addresses,
                reason,
            } => {
                eprintln!(
                    "[p2p] ListenerClosed: listener_id={listener_id:?} addresses={addresses:?} reason={reason:?}"
                );
                // 电路监听关闭 = relay 预约失败/过期/被拒（libp2p-relay 0.21
                // client 无 ReservationReqFailed 事件，N3）：清理预约与
                // in-flight 标记，重选由周期 tick 自然进行（普通 TCP 监听
                // 地址关闭不含 /p2p-circuit 段，函数内部忽略）
                self.on_circuit_listener_closed(&addresses);
            }
            SwarmEvent::Behaviour(behaviour_event) => self.handle_behaviour_event(behaviour_event),
            SwarmEvent::IncomingConnection { .. } => {}
            SwarmEvent::IncomingConnectionError {
                local_addr, send_back_addr, error, ..
            } => {
                eprintln!(
                    "[p2p] IncomingConnectionError: local_addr={local_addr} send_back_addr={send_back_addr} error={error:?}"
                );
            }
            SwarmEvent::ExpiredListenAddr { .. } => {}
            SwarmEvent::ListenerError { listener_id, error } => {
                eprintln!("[p2p] ListenerError: listener_id={listener_id:?} error={error:?}");
            }
            _ => {}
        }
    }

    /// M9 确定性错误硬删：`WrongPeerId`（尤其 obtained=本机）或拨号目标地址命中
    /// 本机监听地址时，从目标 peer 的覆盖网记录删除该地址——这类错误是确定性
    /// 死地址（对端 peerId 不匹配 / 多实例同机污染），删除后不再反复白拨。
    fn hard_delete_on_deterministic_failure(
        &mut self,
        connection_id: ConnectionId,
        error: &libp2p::swarm::DialError,
    ) {
        // 目标地址：WrongPeerId 的 endpoint 即拨号地址；其余错误无法定位地址
        let (failing_addr, obtained_is_self) = match error {
            libp2p::swarm::DialError::WrongPeerId { obtained, address } => {
                (address.to_string(), Some(*obtained) == Some(self.self_peer_id()))
            }
            _ => return,
        };
        // 命中本机监听地址也是确定性死地址（自过滤漏网兜底）
        let hits_self_addr = self.self_listen_addr_set().contains(&failing_addr);
        if !obtained_is_self && !hits_self_addr {
            return;
        }
        // 按 conn_id 定位目标 peer：connect 命令或 org/dm 直连 attempt
        let target_peer = self
            .pending_connects
            .iter()
            .find(|p| p.in_flight.iter().any(|d| d.conn_id == connection_id))
            .and_then(|p| extract_peer_id(&p.node_info).and_then(|s| s.parse::<PeerId>().ok()))
            .or_else(|| {
                self.pending_org_attempts
                    .iter()
                    .find(|a| a.batch.iter().any(|d| d.conn_id == connection_id))
                    .and_then(|a| a.current_peer)
            });
        let Some(peer) = target_peer else {
            return;
        };
        // 拨号可能走 raw 或 /p2p/<id> 变体：两形态都删（base_addr 剥 /p2p 段）
        let peer_str = peer.to_base58();
        let mut store = OverlayPeerStore::new(&mut self.storage);
        let _ = store.remove_addr(&peer_str, &failing_addr);
        let base = super::org_direct::base_addr(&failing_addr);
        if base != failing_addr.as_str() {
            let _ = store.remove_addr(&peer_str, base);
        }
    }

    fn handle_behaviour_event(&mut self, event: SparkBehaviourEvent) {
        match event {
            SparkBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            }) => {
                if message.topic == gossipsub::IdentTopic::new(PLUGIN_ANNOUNCE_TOPIC).hash() {
                    // plugin-announce：字节校验在校验链内做（失败 Reject 扣分），
                    // 非 UTF-8 直接按结构非法上报
                    match String::from_utf8(message.data) {
                        Ok(text) => {
                            self.handle_inbound_plugin_announce(&text, propagation_source, message_id)
                        }
                        Err(_) => {
                            let _ = self
                                .swarm
                                .behaviour_mut()
                                .gossipsub
                                .report_message_validation_result(
                                    &message_id,
                                    &propagation_source,
                                    gossipsub::MessageAcceptance::Reject,
                                );
                        }
                    }
                    return;
                }
                let Ok(text) = String::from_utf8(message.data) else {
                    // 非 UTF-8：保持开启 validate_messages 前的语义（照常转发），
                    // 无条件回报 Accept（overlay/sync 不在本波收紧评分）
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .gossipsub
                        .report_message_validation_result(
                            &message_id,
                            &propagation_source,
                            gossipsub::MessageAcceptance::Accept,
                        );
                    return;
                };
                if message.topic == gossipsub::IdentTopic::new(OVERLAY_TOPIC).hash() {
                    // spark-overlay 分流：§3 信封 type='org-address' 走组织地址记录
                    // 校验链（p2p-messages.md §16），其余按 node-announce 处理
                    let is_org_address = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.get("type")?.as_str().map(ToString::to_string))
                        .as_deref()
                        == Some(crate::org::ORG_ADDRESS_GOSSIP_TYPE);
                    if is_org_address {
                        self.handle_inbound_org_address(&text);
                    } else {
                        self.handle_inbound_announce(&text);
                    }
                } else {
                    self.handle_sync_message(&text);
                }
                // overlay/sync 保持历史语义（validate_messages 开启前一律转发）：
                // 无条件回报 Accept，不引入新的评分行为
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .report_message_validation_result(
                        &message_id,
                        &propagation_source,
                        gossipsub::MessageAcceptance::Accept,
                    );
            }
            SparkBehaviourEvent::Mdns(mdns::Event::Discovered(peers)) => {
                let now = self.now();
                let self_id = self.self_peer_id().to_base58();
                let self_addrs = self.self_listen_addr_set();
                let mut store = OverlayPeerStore::new(&mut self.storage);
                for (peer_id, addr) in peers {
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        &[addr.to_string()],
                        OverlayPeerSource::Mdns,
                        false,
                        now,
                        Some(&self_id),
                        &self_addrs,
                    );
                }
            }
            SparkBehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
                // 记录对端协议清单（三层确认第②层：/spark/ 前缀判定 Spark 节点）
                let protocols: HashSet<String> =
                    info.protocols.iter().map(ToString::to_string).collect();
                self.peer_protocols.insert(peer_id, protocols);
                // 对端监听地址灌进 kad 路由表
                if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                    for addr in info.listen_addrs {
                        kad.add_address(&peer_id, addr);
                    }
                }
            }
            SparkBehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id, result, ..
            }) => match result {
                kad::QueryResult::GetRecord(res) => self.resolve_dht_get(id, res),
                kad::QueryResult::PutRecord(res) => self.resolve_dht_put(id, res),
                kad::QueryResult::GetProviders(res) => self.resolve_dht_providers(id, res),
                kad::QueryResult::GetClosestPeers(res) => match res {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[p2p] Kad GetClosestPeers: error={e:?}");
                    }
                },
                _ => {}
            },
            SparkBehaviourEvent::NodeChallengeRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    self.handle_challenge_inbound(peer, request, channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.resolve_challenge(request_id, Some(response));
                }
            },
            SparkBehaviourEvent::NodeChallengeRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_challenge(request_id, None);
            }
            SparkBehaviourEvent::VersionRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request { channel, .. } => {
                    self.handle_version_inbound(channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.resolve_version_response(request_id, peer, response);
                }
            },
            SparkBehaviourEvent::VersionRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_version_failure(request_id);
            }
            SparkBehaviourEvent::ExchangeRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    self.handle_exchange_inbound_request(peer, request, channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.handle_exchange_response(request_id, response);
                }
            },
            SparkBehaviourEvent::ExchangeRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_exchange_failure(request_id);
            }
            SparkBehaviourEvent::RecoveryRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    self.answer_recovery(peer, request, channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.resolve_recovery_outbound(request_id, Some(response));
                }
            },
            SparkBehaviourEvent::RecoveryRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_recovery_outbound(request_id, None);
            }
            SparkBehaviourEvent::OrgShareRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    self.handle_org_share_inbound(peer, request, channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.resolve_org_response(request_id, response, false);
                }
            },
            SparkBehaviourEvent::OrgShareRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_org_failure(request_id, false);
            }
            SparkBehaviourEvent::DmRr(request_response::Event::Message {
                peer,
                message,
                ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    // 诊断日志（debug 级）：最低层捕获所有 request-response 请求，
                    // 默认不输出避免污染日志；排查入站消息时可开 debug 观察
                    let preview = if request.len() <= 200 { &request[..] } else { &request[..200] };
                    log::debug!(
                        "[P2P_SWARM_MSG] DmRr Request from_peer={} preview={}",
                        &peer.to_base58()[..std::cmp::min(16, peer.to_base58().len())],
                        preview
                    );
                    self.handle_dm_inbound(peer, request, channel);
                }
                request_response::Message::Response {
                    request_id,
                    response,
                    ..
                } => {
                    self.resolve_org_response(request_id, response, true);
                }
            },
            SparkBehaviourEvent::DmRr(request_response::Event::OutboundFailure {
                request_id,
                ..
            }) => {
                self.resolve_org_failure(request_id, true);
            }
            SparkBehaviourEvent::RelayClient(libp2p::relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                ..
            }) => {
                // 预约成功：记录电路地址，触发 announce + DHT 重发（peer-rediscovery §4.6）。
                // 只认 ReservationReqAccepted——OutboundCircuitEstablished 是"我们经 relay
                // 拨出"，与预约无关，不能据此登记自己对外可达的电路地址。
                self.on_reservation_accepted(relay_peer_id);
            }
            SparkBehaviourEvent::RelayClient(
                libp2p::relay::client::Event::OutboundCircuitEstablished { .. }
                | libp2p::relay::client::Event::InboundCircuitEstablished { .. },
            ) => {
                // 电路建立（出站经 relay 拨出 / 入站对端经我们预约连入）均不改变对外
                // 可达的电路地址集合；对外发布只以 ReservationReqAccepted 为准（§4.6）。
            }
            _ => {}
        }
    }
}
