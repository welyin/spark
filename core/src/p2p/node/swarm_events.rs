//! swarm 事件分发：连接生命周期（拨号结果记账、connect/org 直连匹配、版本
//! 探测触发、端口写回）与 behaviour 事件（gossip 分流、mdns/identify 入池、
//! kad 查询汇总、各 request-response 协议的入站/出站钩子）。

use std::collections::HashSet;

use libp2p::swarm::SwarmEvent;
use libp2p::{PeerId, gossipsub, identify, kad, mdns, request_response};
use serde_json::Value;

use crate::p2p::P2pError;
use crate::p2p::behaviour::SparkBehaviourEvent;
use crate::p2p::constants::{OVERLAY_TOPIC, P2P_LISTEN_WS_PORT};
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
                // 地址变化（UPnP 映射、relay 预约）→ 立即补发通告
                let _ = self.publish_announce();
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
                        break;
                    }
                    j += 1;
                }
                if num_established.get() == 1 {
                    self.emit(P2pEvent::PeerConnected {
                        peer_id: peer_id.to_base58(),
                    });
                }
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
                    let mut store = PeerActivityStore::new(&mut self.storage);
                    let _ = store.mark_disconnected(&peer_id.to_base58(), now);
                    self.emit(P2pEvent::PeerDisconnected {
                        peer_id: peer_id.to_base58(),
                    });
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                // connect 命令：失败则试下一目标
                let mut i = 0;
                while i < self.pending_connects.len() {
                    let matched = {
                        let p = &self.pending_connects[i];
                        match (
                            peer_id,
                            extract_peer_id(&p.node_info).and_then(|s| s.parse::<PeerId>().ok()),
                        ) {
                            (Some(actual), Some(expected)) => actual == expected,
                            (None, None) => true,
                            _ => false,
                        }
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
                // org 尝试：失败试下一目标
                let mut j = 0;
                while j < self.pending_org_attempts.len() {
                    let should_retry = {
                        let a = &self.pending_org_attempts[j];
                        a.in_flight.is_none()
                            && a.current_target.is_some()
                            && match (peer_id, a.current_peer) {
                                (Some(actual), Some(expected)) => actual == expected,
                                (None, None) => true,
                                (Some(_), None) => true,
                                _ => false,
                            }
                    };
                    if should_retry {
                        let mut a = self.pending_org_attempts.remove(j);
                        a.current_target = None;
                        self.dial_next_org_target(&mut a);
                        if a.current_target.is_some() {
                            self.pending_org_attempts.push(a);
                        } else {
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
            }
            SwarmEvent::Behaviour(behaviour_event) => self.handle_behaviour_event(behaviour_event),
            _ => {}
        }
    }

    fn handle_behaviour_event(&mut self, event: SparkBehaviourEvent) {
        match event {
            SparkBehaviourEvent::Gossipsub(gossipsub::Event::Message { message, .. }) => {
                let Ok(text) = String::from_utf8(message.data) else {
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
            _ => {}
        }
    }
}
