//! 优先类目 peer 的重新发现（peer-rediscovery §4.3/§4.8）：本地缓存拨号
//! ∥ DHT 查询 并行竞速。
//!
//! 优先类目（自设备 + 好友）没有任何组织级冗余，断开后的 DHT 竞速是唯一
//! 恢复手段。竞速目标集有界、只在断开时触发，DHT 开销可忽略。
//!
//! 状态机已收敛为 `Idle`/`Racing` 两态（connection-policy M10 删退避状态机）：
//! 失败即回 Idle 静默，等下一个真实事件（网络恢复 / 发送懒触发 / 对端拨入）
//! 再触发，tick 内零主动拨号、彻底事件驱动。

use libp2p::{PeerId, kad};

use crate::p2p::announce::{node_presence_record_key, verify_announce_text};
use crate::storage::StorageBackend;

use super::event_loop::EventLoop;

/// 优先类目 peer 的重新发现状态（peer-rediscovery §4.8，Idle/Racing 两态）。
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
}

impl<S: StorageBackend> EventLoop<S> {
    /// 触发优先类目 peer 的重新发现（peer 断开且属于优先类目时调用）。
    /// 并行竞速：A) 本地缓存地址拨号  B) DHT 查询。
    pub(super) fn start_rediscovery(&mut self, peer: PeerId) {
        let now = self.now();
        if matches!(
            self.rediscovery_states.get(&peer),
            Some(RediscoveryState::Racing { .. })
        ) {
            // 已有一轮竞速在途，不重复竞速（防重复 DHT 查询 + 重复拨号）
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
            // allocate_new_port：复用监听端口 [::]:15002 会与多 listener 冲突
            // EADDRINUSE，用 OS 临时端口恢复 PC 主动拨号。止血：dcutr 未接入
            // （§7.1 阶段 B），relay 不依赖源端口；待 dcutr 接入时重新评估端口
            // 复用（wiki §4.6.3/§7.1）。
            let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .addresses(cached_addrs)
                .allocate_new_port()
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
    /// 注意：本函数所有命中但确认失败的路径都必须推进状态机（回到 Idle），
    /// 不能停留在 Racing——否则该 peer 的竞速永不结束（§4.8）。
    fn confirm_dht_node_record_for_rediscovery(
        &mut self,
        peer: PeerId,
        value: &[u8],
        dht_query_id: Option<kad::QueryId>,
    ) {
        let _ = dht_query_id;
        // 校验失败的公共出口：DHT 命中但确认失败 → 回 Idle 静默（竞速收尾）
        let fail = |this: &mut Self| {
            this.abort_rediscovery_attempt(peer);
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
                // allocate_new_port：复用监听端口 [::]:15002 会与多 listener 冲突
                // EADDRINUSE，用 OS 临时端口恢复 PC 主动拨号。止血：dcutr 未接入
                // （§7.1 阶段 B），relay 不依赖源端口；待 dcutr 接入时重新评估端口
                // 复用（wiki §4.6.3/§7.1）。
                let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                    .addresses(addrs)
                    .allocate_new_port()
                    .build();
                let _ = self.swarm.dial(opts);
                self.pending_rediscovery_confirm.insert(peer, announce);
                // 拨号发起后保持 Racing，等待连接建立完成确认
                return;
            }
            // announce 无可用地址 → 无法拨号，本轮视为失败回 Idle 静默
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
        // 竞速成功，回到 Idle
        self.rediscovery_states.insert(peer, RediscoveryState::Idle);
    }

    /// 竞速拨号失败归属（OutgoingConnectionError 按 peer 调用，N2）：仅当该
    /// peer 处于「DHT 命中后拨号待确认」阶段（Racing 且 stash 指向它）才清理
    /// 暂存的 announce 并回 Idle 静默——普通 connect/org 拨号失败、以及
    /// start_rediscovery 并行 A 的缓存拨号失败（DHT 查询仍在途，stash 为空）
    /// 都不受影响，避免误伤正常连接管理。
    pub(super) fn on_rediscovery_dial_failed(&mut self, peer: PeerId) {
        let racing = matches!(
            self.rediscovery_states.get(&peer),
            Some(RediscoveryState::Racing { .. })
        );
        if racing && self.pending_rediscovery_confirm.remove(&peer).is_some() {
            self.abort_rediscovery_attempt(peer);
        }
    }

    /// DHT 竞速未命中：回 Idle 静默，等下一个真实事件再触发。
    pub(super) fn on_rediscovery_dht_miss(&mut self, peer: PeerId, dht_query_id: Option<kad::QueryId>) {
        let _ = dht_query_id;
        self.abort_rediscovery_attempt(peer);
    }

    /// 竞速收尾：把一次失败的竞速尝试迁出 Racing 回 Idle 静默（DHT 未命中 /
    /// 命中但确认失败 / 无地址可拨 / 竞速拨号失败时调用），清理暂存的 announce。
    /// 竞速必须有明确结果（成功或失败）离开 Racing，否则后续事件会被
    /// `start_rediscovery` 的 Racing 守卫永久拒绝（§4.8 收尾铁律）。失败即静默，
    /// 不做任何退避调度，由下一个真实事件（网络恢复 / 发送懒触发 / 对端拨入）
    /// 重新触发。
    fn abort_rediscovery_attempt(&mut self, peer: PeerId) {
        self.pending_rediscovery_confirm.remove(&peer);
        self.rediscovery_states.insert(peer, RediscoveryState::Idle);
    }
}
