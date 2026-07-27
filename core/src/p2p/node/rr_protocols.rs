//! request-response 协议三件套（一）：version 探测、peer-exchange、org-recovery。
//!
//! 每个协议按 begin_xxx（发起）/ handle_xxx_inbound（应答侧）/ resolve_xxx
//! （出站汇总）组织；事件入口在 `swarm_events` 的 behaviour 事件分支。

use libp2p::{PeerId, request_response};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::p2p::direct;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::{EventLoop, ForwardCtx, RecoverySession};

impl<S: StorageBackend> EventLoop<S> {
    // ------------------------------------------------------------------
    // version 探测
    // ------------------------------------------------------------------

    /// 版本探测（in-flight 去重）：连接建立时发起 version 请求。
    pub(super) fn begin_version_probe(&mut self, peer_id: PeerId) {
        if !self.version_probe_in_flight.contains(&peer_id) {
            self.version_probe_in_flight.insert(peer_id);
            let request_id = self
                .swarm
                .behaviour_mut()
                .version_rr
                .send_request(&peer_id, String::new());
            self.pending_version.insert(request_id, peer_id);
        }
    }

    /// 响应侧：连接打开即写版本帧（请求体为空）。
    pub(super) fn handle_version_inbound(
        &mut self,
        channel: request_response::ResponseChannel<String>,
    ) {
        let frame = direct::build_peer_version_response(
            &self.app_version,
            &self.self_peer_id().to_base58(),
            self.now(),
        );
        let text = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string());
        let _ = self
            .swarm
            .behaviour_mut()
            .version_rr
            .send_response(channel, text);
    }

    pub(super) fn resolve_version_response(
        &mut self,
        request_id: request_response::OutboundRequestId,
        peer: PeerId,
        response: String,
    ) {
        if let Some(probed) = self.pending_version.remove(&request_id) {
            self.version_probe_in_flight.remove(&probed);
            if let Some(version) = direct::parse_peer_version_response(&response) {
                self.host.on_peer_version(&version, &peer.to_base58());
                self.emit(P2pEvent::PeerVersion {
                    peer_id: peer.to_base58(),
                    app_version: version,
                });
            }
        }
    }

    pub(super) fn resolve_version_failure(
        &mut self,
        request_id: request_response::OutboundRequestId,
    ) {
        if let Some(probed) = self.pending_version.remove(&request_id) {
            self.version_probe_in_flight.remove(&probed);
        }
    }

    // ------------------------------------------------------------------
    // peer-exchange
    // ------------------------------------------------------------------

    pub(super) fn begin_exchange(&mut self, peer_id: &str, tx: oneshot::Sender<Result<usize>>) {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            let _ = tx.send(Err(P2pError::Malformed("invalid peer id".to_string())));
            return;
        };
        if !self.connected_peers().contains(&peer) {
            let _ = tx.send(Ok(0));
            return;
        }
        let request_id = self.swarm.behaviour_mut().exchange_rr.send_request(
            &peer,
            direct::build_exchange_request(crate::p2p::constants::PEER_EXCHANGE_MAX),
        );
        self.pending_exchange.insert(request_id, (peer, tx));
    }

    pub(super) fn handle_exchange_inbound_request(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        let respond = |behaviour: &mut crate::p2p::behaviour::SparkBehaviour, text: String| {
            let _ = behaviour.exchange_rr.send_response(channel, text);
        };
        let parsed: Option<Value> = serde_json::from_str(&request).ok();
        if parsed
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            != Some("peer-exchange-request")
        {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_exchange_response(false, &[], None),
            );
            return;
        }
        if self
            .exchange_limiter
            .is_rate_limited(&peer.to_base58(), now)
        {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_exchange_response(false, &[], Some("rate-limited")),
            );
            return;
        }
        let want = direct::normalize_exchange_want(parsed.as_ref().and_then(|v| v.get("want")));
        let samples = {
            let mut store = OverlayPeerStore::new(&mut self.storage);
            store
                .sample_for_exchange(
                    Some(&peer.to_base58()),
                    want,
                    now,
                    crate::p2p::constants::PEER_EXCHANGE_MAX_AGE_MS,
                )
                .unwrap_or_default()
        };
        let samples: Vec<direct::PeerExchangeSample> = samples
            .into_iter()
            .map(|r| direct::PeerExchangeSample {
                peer_id: r.peer_id,
                addresses: r.addresses,
                last_seen_at: r.last_seen_at,
            })
            .collect();
        respond(
            self.swarm.behaviour_mut(),
            direct::build_exchange_response(true, &samples, None),
        );
    }

    pub(super) fn handle_exchange_response(
        &mut self,
        request_id: request_response::OutboundRequestId,
        response: String,
    ) {
        let Some((responder, tx)) = self.pending_exchange.remove(&request_id) else {
            return;
        };
        let Some(samples) = direct::parse_exchange_response(&response) else {
            let _ = tx.send(Ok(0));
            return;
        };
        let now = self.now();
        let self_id = self.self_peer_id().to_base58();
        let responder_id = responder.to_base58();
        let mut merged = 0usize;
        {
            let mut store = OverlayPeerStore::new(&mut self.storage);
            for sample in samples
                .iter()
                .take(crate::p2p::constants::PEER_EXCHANGE_MAX)
            {
                if let Some((pid, addrs)) =
                    direct::filter_incoming_sample(sample, &self_id, &responder_id)
                {
                    let _ = store.remember(&pid, &addrs, OverlayPeerSource::Exchange, false, now);
                    merged += 1;
                }
            }
        }
        self.emit(P2pEvent::PeerExchangeCompleted {
            responder: responder_id,
            merged,
        });
        let _ = tx.send(Ok(merged));
    }

    pub(super) fn resolve_exchange_failure(
        &mut self,
        request_id: request_response::OutboundRequestId,
    ) {
        if let Some((responder, tx)) = self.pending_exchange.remove(&request_id) {
            self.emit(P2pEvent::PeerExchangeCompleted {
                responder: responder.to_base58(),
                merged: 0,
            });
            let _ = tx.send(Ok(0));
        }
    }

    // ------------------------------------------------------------------
    // org-recovery
    // ------------------------------------------------------------------

    pub(super) fn begin_recovery_query(
        &mut self,
        token: &str,
        neighbors: &[String],
        want: usize,
        tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
    ) {
        let connected = self.connected_peers();
        let mut request_ids = Vec::new();
        for neighbor in neighbors.iter().take(3) {
            let Ok(peer) = neighbor.parse::<PeerId>() else {
                continue;
            };
            if !connected.contains(&peer) {
                continue;
            }
            let request_id = self.swarm.behaviour_mut().recovery_rr.send_request(
                &peer,
                direct::build_recovery_request(token, crate::p2p::constants::RECOVERY_TTL, want),
            );
            request_ids.push(request_id);
        }
        if request_ids.is_empty() {
            let _ = tx.send(Ok(Vec::new()));
            return;
        }
        // 首个请求挂 session，其余请求经 extra 映射指向它；最后完成者汇总
        let first = request_ids[0];
        self.pending_recovery.insert(
            first,
            RecoverySession {
                remaining: request_ids.len(),
                collected: Vec::new(),
                tx,
            },
        );
        for id in request_ids.iter().skip(1) {
            self.pending_recovery_extra.insert(*id, first);
        }
    }

    pub(super) fn answer_recovery(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        let Some(query) = direct::parse_recovery_request(&request) else {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(false, &[], None));
            return;
        };
        if self
            .recovery_limiter
            .is_rate_limited(&peer.to_base58(), now)
        {
            let _ = self.swarm.behaviour_mut().recovery_rr.send_response(
                channel,
                direct::build_recovery_response(false, &[], Some("rate-limited")),
            );
            return;
        }
        // 本地命中
        let view = self.host.recovery_view();
        if let Some(peers) = direct::match_recovery_view(&view, &query.token, query.want, now) {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(true, &peers, None));
            return;
        }
        // 转发：ttl>0 时向除请求方外的已连接邻居取前 2 个
        let ttl = direct::normalize_recovery_ttl(query.ttl);
        let connected: Vec<PeerId> = self
            .connected_peers()
            .into_iter()
            .filter(|p| *p != peer)
            .take(2)
            .collect();
        if ttl == 0 || connected.is_empty() {
            let _ = self
                .swarm
                .behaviour_mut()
                .recovery_rr
                .send_response(channel, direct::build_recovery_response(true, &[], None));
            return;
        }
        let mut ids = Vec::new();
        for neighbor in &connected {
            let request_id = self.swarm.behaviour_mut().recovery_rr.send_request(
                neighbor,
                direct::build_recovery_request(&query.token, ttl - 1, query.want),
            );
            ids.push(request_id);
        }
        let ctx = ForwardCtx {
            channel,
            remaining: ids.len(),
            collected: Vec::new(),
            want: query.want,
        };
        let first = ids[0];
        self.pending_forward.insert(first, ctx);
        for id in ids.iter().skip(1) {
            self.pending_forward_extra.insert(*id, first);
        }
    }

    // ------------------------------------------------------------------
    // recovery outbound 汇总
    // ------------------------------------------------------------------

    pub(super) fn resolve_recovery_outbound(
        &mut self,
        request_id: request_response::OutboundRequestId,
        response: Option<String>,
    ) {
        // 转发上下文
        if let Some(first) = self.pending_forward_extra.remove(&request_id) {
            let mut respond_now = None;
            if let Some(ctx) = self.pending_forward.get_mut(&first) {
                if let Some(text) = response
                    && let Some(peers) = direct::parse_recovery_response(&text)
                {
                    ctx.collected.extend(peers);
                }
                ctx.remaining = ctx.remaining.saturating_sub(1);
                if ctx.remaining == 0
                    && let Some(ctx) = self.pending_forward.remove(&first)
                {
                    let merged = direct::dedupe_recovery_peers(ctx.collected, ctx.want);
                    respond_now = Some((ctx.channel, merged));
                }
            }
            if let Some((channel, merged)) = respond_now {
                let _ = self.swarm.behaviour_mut().recovery_rr.send_response(
                    channel,
                    direct::build_recovery_response(true, &merged, None),
                );
            }
            return;
        }
        // 转发批次的首个请求 id（其余 id 经 pending_forward_extra 归并到它）
        if let Some(mut ctx) = self.pending_forward.remove(&request_id) {
            if let Some(text) = response
                && let Some(peers) = direct::parse_recovery_response(&text)
            {
                ctx.collected.extend(peers);
            }
            ctx.remaining = ctx.remaining.saturating_sub(1);
            if ctx.remaining == 0 {
                let merged = direct::dedupe_recovery_peers(ctx.collected, ctx.want);
                let _ = self.swarm.behaviour_mut().recovery_rr.send_response(
                    ctx.channel,
                    direct::build_recovery_response(true, &merged, None),
                );
            } else {
                self.pending_forward.insert(request_id, ctx);
            }
            return;
        }
        // 主查询 session
        if let Some(first) = self.pending_recovery_extra.remove(&request_id) {
            let mut finish = None;
            if let Some(session) = self.pending_recovery.get_mut(&first) {
                if let Some(text) = response
                    && let Some(peers) = direct::parse_recovery_response(&text)
                {
                    session.collected.extend(peers);
                }
                session.remaining = session.remaining.saturating_sub(1);
                if session.remaining == 0 {
                    finish = self.pending_recovery.remove(&first);
                }
            }
            if let Some(session) = finish {
                let merged = direct::dedupe_recovery_peers(
                    session.collected,
                    crate::p2p::constants::RECOVERY_QUERY_WANT * 2,
                );
                let _ = session.tx.send(Ok(merged));
            }
            return;
        }
        if let Some(mut session) = self.pending_recovery.remove(&request_id) {
            if let Some(text) = response
                && let Some(peers) = direct::parse_recovery_response(&text)
            {
                session.collected.extend(peers);
            }
            session.remaining = session.remaining.saturating_sub(1);
            if session.remaining == 0 {
                let merged = direct::dedupe_recovery_peers(
                    session.collected,
                    crate::p2p::constants::RECOVERY_QUERY_WANT * 2,
                );
                let _ = session.tx.send(Ok(merged));
            } else {
                self.pending_recovery.insert(request_id, session);
            }
        }
    }
}
