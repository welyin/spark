//! 优先类目 peer 的重新发现（peer-rediscovery §4.3/§4.8）：本地缓存拨号
//! ∥ DHT 查询 并行竞速，含退避状态机。
//!
//! 优先类目（自设备 + 好友）没有任何组织级冗余，断开后的 DHT 竞速是唯一
//! 恢复手段。竞速目标集有界、只在断开时触发，DHT 开销可忽略。

use libp2p::{PeerId, kad};

use crate::p2p::announce::{node_presence_record_key, verify_announce_text};
use crate::p2p::constants::{
    REDISCOVERY_BACKOFF_MAX_MS, REDISCOVERY_MAX_FAILURES,
};
use crate::storage::StorageBackend;

use super::event_loop::EventLoop;

/// 优先类目 peer 的重新发现状态（peer-rediscovery §4.8）。
#[derive(Clone, Debug)]
pub enum RediscoveryState {
    /// 未在重新发现（连接正常或未触发）。
    Idle,
    /// 竞速中：本地拨号 + DHT 查询并行。
    Racing {
        started_at: i64,
        /// DHT 查询的 QueryId（结果经 `rediscovery_dht_queries` 映射匹配，
        /// 字段保留以对齐设计文档 §4.8 状态定义）。
        #[allow(dead_code)]
        dht_query_id: Option<kad::QueryId>,
    },
    /// DHT 未命中，退避等待下一次重试（连续失败计数独立存于
    /// `EventLoop::rediscovery_failures`，跨重试轮次保留）。
    Backoff {
        next_retry_at: i64,
    },
    /// 已标记为 offline（连续失败超过上限）。
    Offline,
}

impl<S: StorageBackend> EventLoop<S> {
    /// 触发优先类目 peer 的重新发现（peer 断开且属于优先类目时调用）。
    /// 并行竞速：A) 本地缓存地址拨号  B) DHT 查询。
    pub(super) fn start_rediscovery(&mut self, peer: PeerId) {
        let now = self.now();
        if self
            .rediscovery_states
            .get(&peer)
            .is_some_and(|s| !matches!(s, RediscoveryState::Idle | RediscoveryState::Offline))
        {
            // 已在竞速或退避中，不重复触发
            return;
        }
        // A) 本地缓存地址拨号：从邻居池读取该 peer 的地址
        let cached_addrs: Vec<libp2p::Multiaddr> = {
            let mut store = crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
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
            let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(cached_addrs)
                .build();
            let _ = self.swarm.dial(opts);
        }
        // B) DHT 查询
        let dht_query_id = self.query_peer_dht_record(peer);
        self.rediscovery_states.insert(
            peer,
            RediscoveryState::Racing {
                started_at: now,
                dht_query_id,
            },
        );
    }

    /// 发起一次对该 peer 的 DHT 存在记录查询，返回 QueryId（未挂 kad 时 None）。
    fn query_peer_dht_record(&mut self, peer: PeerId) -> Option<kad::QueryId> {
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            return None;
        };
        let key = kad::RecordKey::new(&node_presence_record_key(&peer.to_base58()));
        let query_id = kad.get_record(key);
        // 登记竞速查询映射（resolve_dht_get 据此区分竞速命中）
        self.rediscovery_dht_queries.insert(query_id, peer);
        Some(query_id)
    }

    /// DHT 竞速查询命中（resolve_dht_get 回调）：走三层确认，若 peer 未连接
    /// 则先拨号，连接建立后再确认。
    pub(super) fn on_rediscovery_dht_hit(
        &mut self,
        peer: PeerId,
        record_value: &[u8],
        dht_query_id: Option<kad::QueryId>,
    ) {
        // 清除竞速状态中的 dht_query_id 标记（本轮查询已结束）
        if let Some(state) = self.rediscovery_states.get_mut(&peer)
            && let RediscoveryState::Racing { started_at, .. } = state
        {
            *state = RediscoveryState::Racing {
                started_at: *started_at,
                dht_query_id: None,
            };
        }
        // 走三层确认（confirm_dht_node_record 内部：未连接时先拨号暂存）
        self.confirm_dht_node_record_for_rediscovery(peer, record_value, dht_query_id);
    }

    /// 竞速场景的 DHT 记录确认：复用三层确认，但 peer 未连接时先拨号、
    /// 暂存 announce 待连接建立后完成确认（§4.3 新增分支）。
    ///
    /// 注意：本函数所有命中但确认失败的路径都必须推进状态机（回到 Backoff），
    /// 不能停留在 Racing——否则该 peer 的竞速永不结束（§4.8）。
    fn confirm_dht_node_record_for_rediscovery(
        &mut self,
        peer: PeerId,
        value: &[u8],
        dht_query_id: Option<kad::QueryId>,
    ) {
        let _ = dht_query_id;
        // 校验失败的公共出口：DHT 命中但确认失败 → 计一次失败进入退避重试
        let fail = |this: &mut Self| {
            this.fail_rediscovery_attempt(peer);
        };
        let Ok(text) = std::str::from_utf8(value) else {
            fail(self);
            return;
        };
        let Some(announce) = verify_announce_text(text) else {
            fail(self);
            return;
        };
        if announce.peer_id != peer.to_base58() {
            fail(self);
            return;
        }
        // ①签名与 PeerId 匹配 → ②identify 协议清单 → ③challenge
        let is_spark = self
            .peer_protocols
            .get(&peer)
            .is_some_and(|ps| ps.iter().any(|p| p.starts_with("/spark/")));
        if !is_spark {
            fail(self);
            return;
        }
        if !self.swarm.is_connected(&peer) {
            // 竞速场景：先拨号，连接建立后在 ConnectionEstablished 中完成确认
            let addrs: Vec<libp2p::Multiaddr> = announce
                .addresses
                .iter()
                .filter_map(|a| a.parse().ok())
                .collect();
            if !addrs.is_empty() {
                let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                    .addresses(addrs)
                    .build();
                let _ = self.swarm.dial(opts);
                self.pending_rediscovery_confirm.insert(peer, announce);
                // 拨号发起后保持 Racing，等待连接建立完成确认
                return;
            }
            // announce 无可用地址 → 无法拨号，本轮视为失败进入退避
            fail(self);
            return;
        }
        // 已连接：直接完成 ②③ 层确认
        self.finish_rediscovery_confirm(peer, announce);
    }

    /// 连接建立后完成竞速确认（ConnectionEstablished 分支调用）。
    pub(super) fn complete_rediscovery_confirm(&mut self, peer: PeerId) {
        if let Some(announce) = self.pending_rediscovery_confirm.remove(&peer) {
            self.finish_rediscovery_confirm(peer, announce);
        }
    }

    fn finish_rediscovery_confirm(&mut self, peer: PeerId, announce: crate::p2p::announce::NodeAnnounce) {
        let now = self.now();
        let nonce = crate::p2p::challenge::generate_nonce();
        let request = crate::p2p::challenge::build_challenge_request(&nonce, now);
        let request_id = self
            .swarm
            .behaviour_mut()
            .node_challenge_rr
            .send_request(&peer, request);
        self.pending_challenge_confirm
            .insert(request_id, (peer, nonce, announce));
        // 竞速成功，回到 Idle 并清除连续失败计数
        self.rediscovery_states.insert(peer, RediscoveryState::Idle);
        self.rediscovery_failures.remove(&peer);
    }

    /// 竞速拨号失败归属（OutgoingConnectionError 按 peer 调用，N2）：仅当该
    /// peer 处于「DHT 命中后拨号待确认」阶段（Racing 且 stash 指向它）才计
    /// 一次失败并清理暂存的 announce——普通 connect/org 拨号失败、以及
    /// start_rediscovery 并行 A 的缓存拨号失败（DHT 查询仍在途，stash 为空）
    /// 都不受影响，避免误伤正常连接管理。
    pub(super) fn on_rediscovery_dial_failed(&mut self, peer: PeerId) {
        let racing = matches!(
            self.rediscovery_states.get(&peer),
            Some(RediscoveryState::Racing { .. })
        );
        if racing && self.pending_rediscovery_confirm.remove(&peer).is_some() {
            self.fail_rediscovery_attempt(peer);
        }
    }

    /// DHT 竞速未命中：进入退避等待。
    pub(super) fn on_rediscovery_dht_miss(&mut self, peer: PeerId, dht_query_id: Option<kad::QueryId>) {
        let _ = dht_query_id;
        self.fail_rediscovery_attempt(peer);
    }

    /// 把一次竞速尝试计为失败并进入退避（DHT 命中但确认失败/无地址可拨/
    /// 竞速拨号失败时调用），避免状态卡在 Racing。与 on_rediscovery_dht_miss
    /// 共用退避节奏。
    ///
    /// 连续失败计数存于独立的 `rediscovery_failures` 映射，跨 Backoff→Racing
    /// 重试轮次保留（状态机在重试时会经 Idle/Racing 迁移，计数挂在状态上
    /// 会被重置为 1，Offline 永不可达）。
    ///
    /// 已 Offline 的 peer 保持 Offline（不因再次 miss 复活为 Backoff）。
    fn fail_rediscovery_attempt(&mut self, peer: PeerId) {
        if matches!(
            self.rediscovery_states.get(&peer),
            Some(RediscoveryState::Offline)
        ) {
            return;
        }
        let now = self.now();
        let failures = self.rediscovery_failures.get(&peer).copied().unwrap_or(0) + 1;
        if failures >= REDISCOVERY_MAX_FAILURES {
            self.rediscovery_states.insert(peer, RediscoveryState::Offline);
            self.rediscovery_failures.remove(&peer);
            return;
        }
        self.rediscovery_failures.insert(peer, failures);
        let delay = rediscovery_backoff_ms(failures);
        self.rediscovery_states.insert(
            peer,
            RediscoveryState::Backoff {
                next_retry_at: now + delay,
            },
        );
    }

    /// 退避到期检查（keepalive tick 调用）：到期则重新竞速。
    pub(super) fn poll_rediscovery_retries(&mut self) {
        let now = self.now();
        let mut due: Vec<PeerId> = Vec::new();
        for (peer, state) in &self.rediscovery_states {
            if let RediscoveryState::Backoff { next_retry_at, .. } = state
                && *next_retry_at <= now
            {
                due.push(*peer);
            }
        }
        for peer in due {
            // 若已重新连上则清除状态与连续失败计数
            if self.swarm.is_connected(&peer) {
                self.rediscovery_states.insert(peer, RediscoveryState::Idle);
                self.rediscovery_failures.remove(&peer);
                continue;
            }
            // 退避到期 → 重新竞速。必须先重置为 Idle 再触发，
            // 否则 start_rediscovery 的入口守卫会拒绝 Backoff 状态（§4.8 退避重试）
            self.rediscovery_states.insert(peer, RediscoveryState::Idle);
            self.start_rediscovery(peer);
        }
    }
}

/// 退避间隔：min(2^(failures-1), 10) 分钟（failures=1→1min, 2→2min, 3→4min,
/// 4→8min, 5+→10min 封顶；对应设计 §4.8 的 1/2/4/8/10/10...）。
pub fn rediscovery_backoff_ms(failures: u32) -> i64 {
    if failures == 0 {
        return 60_000;
    }
    let exp = failures - 1;
    let mins = 1u64.checked_shl(exp).unwrap_or(u64::MAX);
    let mins = std::cmp::min(mins as i64, REDISCOVERY_BACKOFF_MAX_MS / 60_000);
    mins * 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_backoff_schedule() {
        // 1/2/4/8/10/10... 分钟
        assert_eq!(rediscovery_backoff_ms(1), 60_000);
        assert_eq!(rediscovery_backoff_ms(2), 2 * 60_000);
        assert_eq!(rediscovery_backoff_ms(3), 4 * 60_000);
        assert_eq!(rediscovery_backoff_ms(4), 8 * 60_000);
        // 第 5 次起封顶 10 分钟
        assert_eq!(rediscovery_backoff_ms(5), 10 * 60_000);
        assert_eq!(rediscovery_backoff_ms(20), 10 * 60_000);
    }
}
