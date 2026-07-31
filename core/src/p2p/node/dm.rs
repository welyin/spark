//! request-response 协议三件套（三）：dm（direct message）直连。
//!
//! 复用 org 直连的逐地址尝试编排（`OrgAttempt` 状态机，`OrgAttemptKind::Dm`
//! 走 `dm_rr` 协议）；应答侧只做透明搬运：信封 JSON 原样交给宿主
//! [`P2pHost::handle_dm`]，宿主返回值原样序列化为响应帧，验签/落库均在
//! kernel 层完成。
//!
//! 应答侧防护：同一对端按 [`crate::p2p::constants::DM_MIN_INTERVAL_MS`] 最小
//! 间隔限流（命中回 `rate-limited`；控制类 kind——read/recall/friend-accept——
//! 豁免，避免「发消息+已读回执」连发时第二条被误限流）；宿主错误对外一律回
//! `internal-error`（内部细节只记本地 Warning 事件，不外泄）。
//!
//! 宿主提供 [`crate::p2p::host::DmHandler`] 时，信封校验/落库等重 IO 在
//! 阻塞线程池执行（`tokio::task::spawn_blocking`），事件循环线程只负责
//! 收发帧与限流判定；任务完成后经 `dm_completion` 通道把结果送回事件循环，
//! 由 `finish_dm_inbound` 找回 ResponseChannel 回传响应。

use std::collections::{HashSet, VecDeque};

use libp2p::{PeerId, request_response};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::p2p::Result;
use crate::p2p::direct;
use crate::p2p::peer_targets::{PeerNodeInfo, build_dial_targets, extract_peer_id};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::{DmCompletion, EventLoop, OrgAttempt, OrgAttemptKind, OrgTx};

impl<S: StorageBackend> EventLoop<S> {
    /// dm 直连投递：已连接则直接发请求（无需地址），否则逐地址拨号
    /// （与 org 直连同编排）。
    pub(super) fn begin_dm_attempt(
        &mut self,
        node_info: PeerNodeInfo,
        payload: Value,
        tx: oneshot::Sender<Result<Option<Value>>>,
    ) {
        // 已连接短路在拨号目标构建之前：已连接的 peer 空地址也能直发
        // （重拨同一地址会因 TCP 四元组冲突失败，也无必要）。
        let connected_peer = extract_peer_id(&node_info)
            .and_then(|s| s.parse::<PeerId>().ok())
            .filter(|p| self.swarm.is_connected(p));
        if let Some(peer) = connected_peer {
            let request_json = direct::build_dm_request(&payload);
            let request_id = self
                .swarm
                .behaviour_mut()
                .dm_rr
                .send_request(&peer, request_json.clone());
            let attempt = OrgAttempt {
                kind: OrgAttemptKind::Dm,
                targets: VecDeque::new(),
                current_target: None,
                current_peer: Some(peer),
                request_json,
                in_flight: Some(request_id),
                dial_issued: false,
                dial_conn_id: None,
                tx: OrgTx::Dm(tx),
            };
            self.pending_org_attempts.push(attempt);
            return;
        }
        let targets = match build_dial_targets(&node_info) {
            Ok(t) => VecDeque::from(t),
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        let mut attempt = OrgAttempt {
            kind: OrgAttemptKind::Dm,
            targets,
            current_target: None,
            // 目标 peer 在构建时即记录：并发拨号冲突（EADDRINUSE）后的
            // 「已建连接复用」恢复与错误匹配都依赖它
            current_peer: extract_peer_id(&node_info).and_then(|s| s.parse::<PeerId>().ok()),
            request_json: direct::build_dm_request(&payload),
            in_flight: None,
            dial_issued: false,
            dial_conn_id: None,
            tx: OrgTx::Dm(tx),
        };
        self.dial_next_org_target(&mut attempt);
        if attempt.current_target.is_some() || attempt.in_flight.is_some() {
            self.pending_org_attempts.push(attempt);
        } else {
            attempt.finish_exhausted();
        }
    }

    /// 应答侧：限流 → 信封 JSON 原样交宿主 → 宿主应答序列化回传。
    /// 请求非合法 JSON 对象回 `invalid-request`；限流命中回 `rate-limited`
    /// （控制类 kind 豁免限流）；宿主拒绝/未实现回 `internal-error`（原始
    /// 错误仅本地 Warning，不外泄）。
    ///
    /// 宿主提供 [`crate::p2p::host::DmHandler`] 时处理被 spawn 出事件循环
    /// 线程（spawn_blocking），结果经 `dm_completion` 通道送回后由
    /// [`Self::finish_dm_inbound`] 回传响应；否则同步调用 `handle_dm`。
    pub(super) fn handle_dm_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        let Some(payload) = direct::parse_dm_request(&request) else {
            let response = direct::build_dm_error_response("invalid-request");
            let _ = self
                .swarm
                .behaviour_mut()
                .dm_rr
                .send_response(channel, response);
            return;
        };
        // 控制类 kind（read/recall/friend-accept）豁免限流：它们由「发消息」
        // 动作派生连发，与 chat 共享同一 1s 桶会被误限流并标 failed
        let kind = payload.get("kind").and_then(Value::as_str);
        if !direct::dm_kind_is_rate_limit_exempt(kind)
            && self.dm_limiter.is_rate_limited(&peer.to_base58(), now)
        {
            let response = direct::build_dm_error_response("rate-limited");
            let _ = self
                .swarm
                .behaviour_mut()
                .dm_rr
                .send_response(channel, response);
            return;
        }
        match self.host.dm_handler() {
            Some(handler) => {
                // 在线 peerId 快照随请求分发（事件语义为「收到那一刻」的在线集合）
                let online_peers: HashSet<String> = self
                    .connected_peers()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                let task_id = self.next_dm_task_id;
                self.next_dm_task_id += 1;
                self.pending_dm_inbound.insert(task_id, channel);
                let completion_tx = self.dm_completion_tx.clone();
                let peer_id = peer.to_base58();
                tokio::task::spawn_blocking(move || {
                    let result = handler.handle_dm(payload, &peer_id, &online_peers);
                    let completion: DmCompletion = (task_id, peer_id, result);
                    let _ = completion_tx.send(completion);
                });
            }
            None => {
                // 宿主未提供异步处理器：同步回退（NoopHost/轻量测试宿主）
                let response = match self.host.handle_dm(payload, &peer.to_base58()) {
                    Ok(value) => {
                        serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string())
                    }
                    Err(e) => {
                        self.emit(P2pEvent::Warning(format!(
                            "dm handle_dm failed (peer {}): {e}",
                            peer.to_base58()
                        )));
                        direct::build_dm_error_response("internal-error")
                    }
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .dm_rr
                    .send_response(channel, response);
            }
        }
    }

    /// dm 入站异步任务完成：找回 ResponseChannel，按同步路径同一口径
    /// 序列化应答（宿主错误 → Warning + `internal-error`）并回传。
    pub(super) fn finish_dm_inbound(&mut self, completion: DmCompletion) {
        let (task_id, peer_id, result) = completion;
        let Some(channel) = self.pending_dm_inbound.remove(&task_id) else {
            return;
        };
        let response = match result {
            Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
            Err(e) => {
                self.emit(P2pEvent::Warning(format!(
                    "dm handle_dm failed (peer {peer_id}): {e}"
                )));
                direct::build_dm_error_response("internal-error")
            }
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .dm_rr
            .send_response(channel, response);
    }
}
