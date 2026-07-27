//! keepalive tick 编排：覆盖网补拨、peer-exchange 轮选、node-announce 周期
//! 发布、DHT 节点存在记录与网关职责记录周期重发。
//!
//! 周期 tick 由事件循环内 interval 驱动（`event_loop` 的 run），手动 tick 经
//! `Command::Tick`；组织层保活由宿主在 `P2pEvent::KeepaliveTick` 后执行。

use std::collections::HashSet;

use libp2p::swarm::dial_opts::DialOpts;
use libp2p::{Multiaddr, PeerId};
use tokio::sync::oneshot;

use crate::p2p::constants::{DHT_REPUBLISH_TICKS, NODE_ANNOUNCE_INTERVAL_MS};
use crate::p2p::direct;
use crate::p2p::keepalive;
use crate::p2p::overlay_store::OverlayPeerStore;
use crate::storage::StorageBackend;

use super::event_loop::EventLoop;
use super::{KeepaliveStats, P2pEvent};

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn run_keepalive_tick(&mut self) -> KeepaliveStats {
        let mut stats = KeepaliveStats::default();
        let now = self.now();

        // 1) 覆盖网拨号：活跃连接不足时从邻居池补拨
        let connected = self.connected_peers();
        let budget = keepalive::overlay_dial_budget(connected.len());
        if budget > 0 {
            let self_id = self.self_peer_id().to_base58();
            let mut exclude: HashSet<String> = connected.iter().map(ToString::to_string).collect();
            exclude.insert(self_id);
            let candidates = {
                let mut store = OverlayPeerStore::new(&mut self.storage);
                store
                    .sample_dial_candidates(&exclude, budget)
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
                let opts = DialOpts::peer_id(peer).addresses(addrs).build();
                if self.swarm.dial(opts).is_ok() {
                    self.pending_overlay_dials.insert(peer, ());
                    stats.overlay_dialed += 1;
                }
            }
        }

        // 2) peer-exchange：游标轮选一个已连接邻居
        let connected_strs: HashSet<String> = connected.iter().map(ToString::to_string).collect();
        if let Some(target) = keepalive::pick_exchange_target(
            &connected_strs,
            &self.self_peer_id().to_base58(),
            self.overlay_exchange_cursor,
        ) {
            self.overlay_exchange_cursor += 1;
            if let Ok(peer) = target.parse::<PeerId>() {
                let request_id = self.swarm.behaviour_mut().exchange_rr.send_request(
                    &peer,
                    direct::build_exchange_request(crate::p2p::constants::PEER_EXCHANGE_MAX),
                );
                // tick 内发起的交换不带调用方等待器：完成后经事件上报
                let (tx, _rx) = oneshot::channel();
                self.pending_exchange.insert(request_id, (peer, tx));
                stats.exchanged = 1;
            }
        }

        // 3) node-announce 周期发布
        if now - self.last_announced_at >= NODE_ANNOUNCE_INTERVAL_MS
            && let Ok(true) = self.publish_announce()
        {
            stats.announced = true;
        }

        // 4) DHT 节点存在记录周期重发（挂 tick 计数：首个 tick 发一次，此后按间隔）
        self.dht_tick_counter += 1;
        if self.dht_tick_counter == 1 || self.dht_tick_counter % DHT_REPUBLISH_TICKS == 0 {
            self.publish_node_presence_record();
            // 5) 网关职责记录（组织私有 DHT）周期重发（§15 同节奏）
            let provided: Vec<(Vec<u8>, Vec<u8>)> = self
                .provided_records
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (key, value) in provided {
                if let Err(e) = self.republish_provided(&key, &value) {
                    self.emit(P2pEvent::Warning(format!(
                        "dht republish provided failed: {e}"
                    )));
                }
            }
        }

        stats
    }
}
