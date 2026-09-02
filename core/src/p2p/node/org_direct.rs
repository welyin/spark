//! request-response 协议三件套（二）：org-share / org-pull 直连。
//!
//! 逐地址尝试的出站编排（`OrgAttempt` 状态机）与应答侧处理；拨号成功后的
//! 请求发出与失败重试挂钩在 `swarm_events` 的连接事件分支。

use std::collections::VecDeque;
use std::time::Duration;

use libp2p::swarm::ConnectionId;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId, request_response};
use serde_json::Value;

use crate::p2p::constants::DIRECT_DIAL_TARGET_TIMEOUT_MS;
use crate::p2p::direct;
use crate::p2p::peer_targets::{PeerNodeInfo, build_dial_targets, extract_peer_id};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::{EventLoop, OrgAttempt, OrgAttemptKind, OrgTx};

/// 拨号去重的规范化地址：剥掉尾部 `/p2p/{peerId}` 段——同一地址的原始形式
/// （unknown_peer_id 拨）与带 peer 段形式（`DialOpts::from` 拨）必须互认，
/// 否则两个 attempt 会各拨一种形式，对同一端点建立双连接
pub(super) fn base_addr(addr: &str) -> &str {
    addr.split("/p2p/").next().unwrap_or(addr)
}

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn begin_org_attempt(
        &mut self,
        node_info: PeerNodeInfo,
        payload: Value,
        tx: OrgTx,
        is_share: bool,
    ) {
        // 惰性回收调用方已放弃的滞留 attempt（同 begin_connect 口径）
        self.pending_org_attempts.retain(|a| !a.tx.is_closed());
        // M9：带地址记分卡排序 + 自过滤（不拨本机监听地址）
        let addr_meta = extract_peer_id(&node_info)
            .map(|pid| self.addr_meta_for(&pid))
            .unwrap_or_default();
        let self_addrs = self.self_listen_addr_set();
        let targets = match build_dial_targets(&node_info, Some(&addr_meta), &self_addrs) {
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
            batch: Vec::new(),
            // 目标 peer 在构建时即记录（同 begin_dm_attempt 的并发恢复口径）
            current_peer: extract_peer_id(&node_info).and_then(|s| s.parse::<PeerId>().ok()),
            request_json,
            in_flight: None,
            dial_issued: false,
            waiting_base: None,
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
        if attempt.has_dial_activity() {
            self.pending_org_attempts.push(attempt);
        } else {
            attempt.finish_exhausted();
        }
    }

    pub(super) fn dial_next_org_target(&mut self, attempt: &mut OrgAttempt) {
        // 进入新一轮目标尝试：上一目标（如有）的拨号归属失效
        attempt.dial_issued = false;
        // 同地址并发拨号恢复：并行尝试（如 org-share 推送与 dm 邀请同时
        // 拨同一 peer）已建好连接时，本 attempt 的拨号会同步报错/异步
        // DialFailure——此时直接复用已建连接发请求，而不是误走下一目标
        // 或耗尽放弃（放弃侧无任何重试，推送丢失只能等下次变更触发）。
        if let Some(peer) = attempt
            .current_peer
            .filter(|p| self.swarm.is_connected(p))
        {
            let request_id = match attempt.kind {
                OrgAttemptKind::Dm => self
                    .swarm
                    .behaviour_mut()
                    .dm_rr
                    .send_request(&peer, attempt.request_json.clone()),
                _ => self
                    .swarm
                    .behaviour_mut()
                    .org_share_rr
                    .send_request(&peer, attempt.request_json.clone()),
            };
            attempt.in_flight = Some(request_id);
            attempt.current_peer = Some(peer);
            attempt.batch.clear();
            attempt.waiting_base = None;
            return;
        }
        attempt.dial_issued = true;
        // 填满当前批次：至多 DIAL_BATCH_SIZE 个在途拨号；同地址去重/无效地址
        // 不影响批次继续填充。
        while attempt.batch.len() < crate::p2p::constants::DIAL_BATCH_SIZE {
            let Some(target) = attempt.targets.pop_front() else {
                break;
            };
            // 同地址拨号去重：另一 attempt **正在实际拨**同一地址时不重复拨
            // （并发同地址拨号在 loopback 上确定性 EADDRINUSE），登记为等待者
            // ——ConnectionEstablished 按地址匹配时随那路连接发请求；若那路
            // 失败，OutgoingConnectionError 的重试路径唤醒本 attempt 自行拨号。
            let already_dialing = self.pending_org_attempts.iter().any(|a| {
                a.dial_issued
                    && a.in_flight.is_none()
                    && a.batch.iter().any(|d| base_addr(&d.addr) == base_addr(&target))
            });
            if already_dialing {
                attempt.waiting_base = Some(base_addr(&target).to_string());
                attempt.dial_issued = false;
                return;
            }
            // 电路地址补目的段（MissingDstPeerId 根修，peer_targets helper）
            let target = match attempt.current_peer {
                Some(cp) => crate::p2p::peer_targets::ensure_circuit_dst_peer(
                    &target,
                    &cp.to_base58(),
                ),
                None => target,
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
                    // dial 前取出本次拨号的 ConnectionId：OutgoingConnectionError
                    // 按它精确归属（unknown_peer_id 拨号失败时事件 peer_id=None，
                    // 不能按 peer 匹配，否则无关失败会误推进本 attempt）
                    let conn_id = opts.connection_id();
                    if self.swarm.dial(opts).is_ok() {
                        attempt.batch.push(super::event_loop::InFlightDial {
                            addr: target,
                            conn_id,
                        });
                        // 单目标应用层超时：黑洞地址（无 RST）会挂到 OS TCP
                        // 超时（移动端数十秒），一个黑洞目标烧光外层 15s 总
                        // 预算；到期经通道按 OutgoingConnectionError 同口径
                        // 推进下一目标。拨号提前成败时迟到的超时消息无
                        // attempt 匹配（conn_id 已不在批次），自然忽略。
                        let timeout_tx = self.dial_timeout_tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(
                                DIRECT_DIAL_TARGET_TIMEOUT_MS,
                            ))
                            .await;
                            let _ = timeout_tx.send(conn_id);
                        });
                    }
                }
                Err(_) => continue,
            }
        }
    }

    /// 拨号失败/单目标超时按 [`ConnectionId`] 精确归属推进 attempt：
    /// OutgoingConnectionError 与应用层拨号超时（dial_timeout 通道）共用
    /// 本路径。按 ConnectionId 而非 peer/地址匹配——`unknown_peer_id` 拨号
    /// 失败时事件 peer_id=None，模糊匹配会让一个 attempt 的失败级联推进
    /// 同 peer 的所有 attempt（含未拨号的等待者）至目标耗尽。命中则试
    /// 下一目标；耗尽时唤醒同地址等待者并回传终态。
    pub(super) fn fail_org_dial(&mut self, connection_id: ConnectionId) {
        let mut j = 0;
        while j < self.pending_org_attempts.len() {
            let should_retry = {
                let a = &self.pending_org_attempts[j];
                a.in_flight.is_none()
                    && a.dial_issued
                    && a.batch.iter().any(|d| d.conn_id == connection_id)
            };
            if should_retry {
                let mut a = self.pending_org_attempts.remove(j);
                let failed_base = a
                    .batch
                    .iter()
                    .find(|d| d.conn_id == connection_id)
                    .map(|d| base_addr(&d.addr).to_string());
                // 仅移除本次失败的在途目标；批内其余目标继续并发竞速
                a.batch.retain(|d| d.conn_id != connection_id);
                if a.batch.is_empty() {
                    // 本批全败 → 开下一批（或成为同地址等待者 / 耗尽）
                    self.dial_next_org_target(&mut a);
                    if !a.has_dial_activity() {
                        // 拨号方耗尽：同地址的去重等待者所等的事件已不会
                        // 发生，唤醒其自行走目标流程
                        if let Some(base) = failed_base {
                            self.wake_addr_waiters(&base);
                        }
                        a.finish_exhausted();
                        continue; // a 已 remove，不重复 push
                    }
                }
                self.pending_org_attempts.push(a);
            } else {
                j += 1;
            }
        }
    }

    /// 拨号方 attempt 耗尽后唤醒同地址等待者：去重等待者（登记等待、未实际
    /// 拨号）所等的连接事件已不会发生，移交其自行走目标流程（dial_next 内
    /// 有已连接短路，拨号方曾成功建连时直接复用；否则等待者自己拨号）。
    /// 只在 OutgoingConnectionError 的耗尽分支需要：应答类失败时等待者早已
    /// 在 ConnectionEstablished 被服务（不再是等待者）。
    pub(super) fn wake_addr_waiters(&mut self, failed_base: &str) {
        let mut i = 0;
        while i < self.pending_org_attempts.len() {
            let is_waiter = {
                let a = &self.pending_org_attempts[i];
                a.in_flight.is_none() && a.waiting_base.as_deref() == Some(failed_base)
            };
            if is_waiter {
                let mut w = self.pending_org_attempts.remove(i);
                w.waiting_base = None;
                self.dial_next_org_target(&mut w);
                if w.has_dial_activity() {
                    self.pending_org_attempts.push(w);
                } else {
                    w.finish_exhausted();
                }
            } else {
                i += 1;
            }
        }
    }

    pub(super) fn handle_org_share_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        // 故障注入（e2e 专用，org-sync-stall-fix §5）：黑洞开启时 org-pull
        // 入站扣住应答通道不响应——请求方走协议读超时，复现「对端半连接
        // 长超时」；org-share 正常应答（应答面不瘫痪，仅 pull 链路挂起）。
        if self.org_pull_blackhole
            && let Ok(Some((kind, _))) = direct::parse_org_share_request(&request)
            && matches!(
                kind,
                direct::OrgShareRequestKind::OrgPullList | direct::OrgShareRequestKind::OrgPullOrg
            )
        {
            log::info!("[p2p] fault injection: org-pull request blackholed (no response)");
            self.stalled_pull_channels.push(channel);
            return;
        }
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
                // 未送达/不可解析：开下一批地址
                attempt.batch.clear();
                attempt.waiting_base = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.has_dial_activity() {
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
                attempt.batch.clear();
                attempt.waiting_base = None;
                self.dial_next_org_target(&mut attempt);
                if attempt.has_dial_activity() {
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

#[cfg(test)]
mod tests {
    use super::base_addr;

    #[test]
    fn base_addr_strips_p2p_suffix() {
        let raw = "/ip4/127.0.0.1/tcp/9100";
        let with_peer = "/ip4/127.0.0.1/tcp/9100/p2p/12D3KooWExample";
        assert_eq!(base_addr(raw), raw);
        assert_eq!(base_addr(with_peer), raw);
        // 两种形式互认（去重键规范化的目的）
        assert_eq!(base_addr(raw), base_addr(with_peer));
    }
}
