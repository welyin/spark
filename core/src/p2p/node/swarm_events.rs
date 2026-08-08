//! swarm 事件分发：连接生命周期（拨号结果记账、connect/org 直连匹配、版本
//! 探测触发、端口写回）与 behaviour 事件（gossip 分流、mdns/identify 入池、
//! kad 查询汇总、各 request-response 协议的入站/出站钩子）。

use std::collections::HashSet;

use libp2p::swarm::SwarmEvent;
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
                // 连接沉淀进覆盖网邻居池
                let remote_addr = endpoint.get_remote_address().to_string();
                {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        std::slice::from_ref(&remote_addr),
                        OverlayPeerSource::Connect,
                        false,
                        now,
                    );
                }
                // 覆盖网补拨结果记账
                if self.pending_overlay_dials.remove(&peer_id).is_some() {
                    let mut store = OverlayPeerStore::new(&mut self.storage);
                    let _ = store.mark_dial_result(&peer_id.to_base58(), true);
                }
                // 已连接对端地址灌进 kad 路由表（identify 交换前的兜底）
                if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                    kad.add_address(&peer_id, endpoint.get_remote_address().clone());
                }
                // connect 命令匹配
                let remote = remote_addr.clone();
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        let expected =
                            extract_peer_id(&p.node_info).and_then(|s| s.parse::<PeerId>().ok());
                        expected == Some(peer_id)
                            || p.current.as_deref().is_some_and(|t| {
                                remote == t
                                    || remote.starts_with(&format!("{t}/"))
                                    || t.starts_with(&remote)
                            })
                    };
                    if matched {
                        let done = self.pending_connects.remove(i);
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
                            && (a.current_target.as_deref().is_some_and(|t| {
                                remote == t
                                    || remote.starts_with(&format!("{t}/"))
                                    || t.starts_with(&remote)
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
                        // 不 break：同地址去重的等待 attempt 排在本条之后，也要
                        // 随这路已建立连接发出请求（in_flight 非空检查已防
                        // 双连接重复发）
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
                peer_id,
                num_established,
                ..
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
                    // 优先类目 peer（自设备/好友）断开 → 立即并行竞速（peer-rediscovery §4.3）
                    if self.host.is_priority_peer(&peer_id.to_base58()) {
                        self.start_rediscovery(peer_id);
                    }
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, connection_id, error, .. } => {
                // connect 命令：失败则试下一目标。按 ConnectionId 精确归属
                // （同 org attempt 口径）——候选 1 的 unknown_peer_id 拨号失败
                // 时 peer_id=None，按 peer 匹配会失配滞留：对端在线但首候选
                // 撞 mdns/并发拨号竞争时，connect 空等超时、推送整体降级
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        p.dial_conn_id == Some(connection_id)
                    };
                    if matched {
                        let mut p = self.pending_connects.remove(i);
                        p.last_error = Some(error.to_string());
                        p.current = None;
                        match self.dial_next_connect_target(&mut p) {
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
                let mut j = 0;
                while j < self.pending_org_attempts.len() {
                    let should_retry = {
                        let a = &self.pending_org_attempts[j];
                        a.in_flight.is_none()
                            && a.current_target.is_some()
                            && a.dial_issued
                            && a.dial_conn_id == Some(connection_id)
                    };
                    if should_retry {
                        let mut a = self.pending_org_attempts.remove(j);
                        let failed_base = a
                            .current_target
                            .as_deref()
                            .map(super::org_direct::base_addr)
                            .map(str::to_string);
                        a.current_target = None;
                        self.dial_next_org_target(&mut a);
                        if a.current_target.is_some() || a.in_flight.is_some() {
                            self.pending_org_attempts.push(a);
                        } else {
                            // 拨号方耗尽：同地址的去重等待者所等的事件已不会
                            // 发生，唤醒其自行走目标流程
                            if let Some(base) = failed_base {
                                self.wake_addr_waiters(&base);
                            }
                            a.finish_exhausted();
                        }
                    } else {
                        j += 1;
                    }
                }
                // 覆盖网补拨失败记账
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
            SwarmEvent::ListenerClosed { addresses, .. } => {
                // 电路监听关闭 = relay 预约失败/过期/被拒（libp2p-relay 0.21
                // client 无 ReservationReqFailed 事件，N3）：清理预约与
                // in-flight 标记，重选由周期 tick 自然进行（普通 TCP 监听
                // 地址关闭不含 /p2p-circuit 段，函数内部忽略）
                self.on_circuit_listener_closed(&addresses);
            }
            SwarmEvent::Behaviour(behaviour_event) => self.handle_behaviour_event(behaviour_event),
            _ => {}
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
                let mut store = OverlayPeerStore::new(&mut self.storage);
                for (peer_id, addr) in peers {
                    let _ = store.remember(
                        &peer_id.to_base58(),
                        &[addr.to_string()],
                        OverlayPeerSource::Mdns,
                        false,
                        now,
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
