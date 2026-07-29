//! request-response 协议三件套（二）：org-share / org-pull 直连。
//!
//! 逐地址尝试的出站编排（`OrgAttempt` 状态机）与应答侧处理；拨号成功后的
//! 请求发出与失败重试挂钩在 `swarm_events` 的连接事件分支。

use std::collections::VecDeque;

use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId, request_response};
use serde_json::Value;

use crate::p2p::direct;
use crate::p2p::peer_targets::{PeerNodeInfo, build_dial_targets, extract_peer_id};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::{EventLoop, OrgAttempt, OrgAttemptKind, OrgTx};

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn begin_org_attempt(
        &mut self,
        node_info: PeerNodeInfo,
        payload: Value,
        tx: OrgTx,
        is_share: bool,
    ) {
        let targets = match build_dial_targets(&node_info) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                match tx {
                    OrgTx::Share(tx) => {
                        let _ = tx.send(Err(e));
                    }
                    OrgTx::Pull(tx) => {
                        let _ = tx.send(Err(e));
                    }
                    OrgTx::Dm(tx) => {
                        let _ = tx.send(Err(e));
                    }
                }
                return;
            }
        };
        let (kind, request_json) = if is_share {
            let sync_id = payload
                .get("syncId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (
                OrgAttemptKind::Share {
                    expected_sync_id: sync_id,
                },
                direct::build_org_share_request(payload),
            )
        } else {
            let text = match payload {
                Value::String(s) => s,
                _ => String::new(),
            };
            (OrgAttemptKind::Pull, text)
        };
        let mut attempt = OrgAttempt {
            kind,
            targets,
            current_target: None,
            current_peer: None,
            request_json,
            in_flight: None,
            tx,
        };
        // 已连接则直接在现有连接上发请求：重拨同一地址会因 TCP 端口复用的
        // 四元组冲突（EADDRINUSE）失败，也无必要。
        let connected_peer = extract_peer_id(&node_info)
            .and_then(|s| s.parse::<PeerId>().ok())
            .filter(|p| self.swarm.is_connected(p));
        if let Some(peer) = connected_peer {
            let request_id = self
                .swarm
                .behaviour_mut()
                .org_share_rr
                .send_request(&peer, attempt.request_json.clone());
            attempt.in_flight = Some(request_id);
            attempt.current_peer = Some(peer);
            self.pending_org_attempts.push(attempt);
            return;
        }
        self.dial_next_org_target(&mut attempt);
        if attempt.current_target.is_some() {
            self.pending_org_attempts.push(attempt);
        } else {
            attempt.finish_exhausted();
        }
    }

    pub(super) fn dial_next_org_target(&mut self, attempt: &mut OrgAttempt) {
        while let Some(target) = attempt.targets.pop_front() {
            match target.parse::<Multiaddr>() {
                Ok(ma) => {
                    let opts = if target.contains("/p2p/") {
                        DialOpts::from(ma)
                    } else {
                        DialOpts::unknown_peer_id().address(ma).build()
                    };
                    if self.swarm.dial(opts).is_ok() {
                        attempt.current_target = Some(target);
                        return;
                    }
                }
                Err(_) => continue,
            }
        }
    }

    pub(super) fn handle_org_share_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let response = match direct::parse_org_share_request(&request) {
            Err(_) => direct::build_org_share_error_response("empty or invalid json"),
            Ok(None) => direct::build_org_share_error_response("invalid type"),
            Ok(Some((direct::OrgShareRequestKind::OrgShare, payload))) => {
                match self
                    .host
                    .apply_incoming_org_share(payload.clone(), "direct")
                {
                    Ok(Some(ack)) => {
                        self.emit(P2pEvent::OrgShareAccepted {
                            org_id: ack.org_id.clone(),
                            sync_id: ack.sync_id.clone(),
                            source: "direct",
                        });
                        direct::build_org_share_ack_response(
                            ack.sync_id.as_deref(),
                            &ack.org_id,
                            &ack.receiver_root_id,
                        )
                    }
                    _ => direct::build_org_share_error_response("not accepted"),
                }
            }
            Ok(Some((direct::OrgShareRequestKind::OrgPullList, payload))) => {
                match self.host.handle_org_pull_list(payload, Some(peer.to_base58())) {
                    Ok(value) => value.to_string(),
                    Err(e) => serde_json::json!({"ok": false, "type": "org-pull-list-response", "reason": e}).to_string(),
                }
            }
            Ok(Some((direct::OrgShareRequestKind::OrgPullOrg, payload))) => {
                match self.host.handle_org_pull_org(payload, Some(peer.to_base58())) {
                    Ok(value) => value.to_string(),
                    Err(e) => serde_json::json!({"ok": false, "type": "org-pull-org-response", "orgId": "", "reason": e}).to_string(),
                }
            }
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .org_share_rr
            .send_response(channel, response);
    }

    // ------------------------------------------------------------------
    // org 直连 outbound 汇总
    // ------------------------------------------------------------------
    //
    // `from_dm` 标记事件来源协议：`org_share_rr` 与 `dm_rr` 的
    // OutboundRequestId 各自从 1 递增，同一 id 在两个 behaviour 上并存；
    // 两个分支共用 pending_org_attempts，必须按 kind 类别过滤——DmRr 分支
    // 只匹配 Dm attempt，OrgShareRr 分支只匹配 Share/Pull，否则「边同步边
    // 聊天」时 org ack 会被当成 dm 应答（反之亦然）。

    pub(super) fn resolve_org_response(
        &mut self,
        request_id: request_response::OutboundRequestId,
        response: String,
        from_dm: bool,
    ) {
        let mut i = 0;
        while i < self.pending_org_attempts.len() {
            let same_protocol =
                matches!(self.pending_org_attempts[i].kind, OrgAttemptKind::Dm) == from_dm;
            if self.pending_org_attempts[i].in_flight == Some(request_id) && same_protocol {
                let mut attempt = self.pending_org_attempts.remove(i);
                attempt.in_flight = None;
                let delivered = match &attempt.kind {
                    OrgAttemptKind::Share { expected_sync_id } => {
                        direct::parse_org_share_direct_response(&response, expected_sync_id)
                    }
                    OrgAttemptKind::Pull => {
                        matches!(serde_json::from_str::<Value>(&response), Ok(v) if v.is_object())
                    }
                    OrgAttemptKind::Dm => direct::parse_dm_response(&response).is_some(),
                };
                if delivered {
                    match (&attempt.kind, attempt.tx) {
                        (OrgAttemptKind::Share { .. }, OrgTx::Share(tx)) => {
                            let _ = tx.send(Ok(true));
                        }
                        (OrgAttemptKind::Pull, OrgTx::Pull(tx)) => {
                            let value = serde_json::from_str::<Value>(&response).ok();
                            let _ = tx.send(Ok(value));
                        }
                        (OrgAttemptKind::Dm, OrgTx::Dm(tx)) => {
                            let value = direct::parse_dm_response(&response);
                            let _ = tx.send(Ok(value));
                        }
                        // 类别与通道不匹配属内部错误，按耗尽处理
                        (kind, tx) => {
                            let _ = kind;
                            match tx {
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
                    return;
                }
                // 未送达/不可解析：下一个地址
                attempt.current_target = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.current_target.is_some() {
                    self.pending_org_attempts.push(attempt);
                } else {
                    attempt.finish_exhausted();
                }
                return;
            }
            i += 1;
        }
        // 未知 id（attempt 已完结/类别错配）：忽略，不 panic
        self.emit(P2pEvent::Warning(format!(
            "org/dm response for unknown request id {request_id:?} (from_dm={from_dm})"
        )));
    }

    pub(super) fn resolve_org_failure(
        &mut self,
        request_id: request_response::OutboundRequestId,
        from_dm: bool,
    ) {
        let mut i = 0;
        while i < self.pending_org_attempts.len() {
            let same_protocol =
                matches!(self.pending_org_attempts[i].kind, OrgAttemptKind::Dm) == from_dm;
            if self.pending_org_attempts[i].in_flight == Some(request_id) && same_protocol {
                let mut attempt = self.pending_org_attempts.remove(i);
                attempt.in_flight = None;
                attempt.current_target = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.current_target.is_some() {
                    self.pending_org_attempts.push(attempt);
                } else {
                    attempt.finish_exhausted();
                }
                return;
            }
            i += 1;
        }
        // 未知 id（attempt 已完结/类别错配）：忽略，不 panic
        self.emit(P2pEvent::Warning(format!(
            "org/dm failure for unknown request id {request_id:?} (from_dm={from_dm})"
        )));
    }
}
