//! keepalive tick 编排：peer-exchange 轮选、node-announce 周期发布、
//! DHT 节点存在记录与网关职责记录周期重发。
//!
//! tick 内**无任何拨号**（connection-policy M8）：覆盖网孤岛自举改事件驱动
//! （节点启动 / 网络变更确认时由 [`EventLoop::bootstrap_overlay_dial`] 触发
//! 一轮，失败沉默到下个事件），不再周期补拨。
//!
//! 周期 tick 由事件循环内 interval 驱动（`event_loop` 的 run），手动 tick 经
//! `Command::Tick`；组织层保活由宿主在 `P2pEvent::KeepaliveTick` 后执行。

use std::collections::HashSet;

use libp2p::PeerId;
use tokio::sync::oneshot;

use crate::p2p::constants::NODE_ANNOUNCE_INTERVAL_MS;
use crate::p2p::direct;
use crate::p2p::keepalive;
use crate::storage::StorageBackend;

use super::event_loop::EventLoop;
use super::{KeepaliveStats, P2pEvent};

impl<S: StorageBackend> EventLoop<S> {
    pub(super) fn run_keepalive_tick(&mut self) -> KeepaliveStats {
        let mut stats = KeepaliveStats::default();
        let now = self.now();

        // 0) 本地地址变化探测（M9，网络变化重连 A+B 的 B 兜底）：纯本地对比
        // 监听地址与上轮快照，变化即武装防抖**一次性定时器**——到点回发
        // `Command::NetworkChangeFired`，由事件循环执行重连动作。tick 内零拨号
        // 是结构保证（函数体无任何拨号路径），不靠逻辑门控。首 tick 只记录
        // 基线，不触发。
        self.detect_local_network_change();

        // 1) peer-exchange：游标轮选一个已连接邻居
        let connected = self.connected_peers();
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
        let republish_interval = self.dht_republish_ticks;
        if self.dht_tick_counter == 1 || self.dht_tick_counter % republish_interval == 0 {
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

        // 7) relay 预约不足时补充（peer-rediscovery §4.6.2）
        self.ensure_relay_reservations();

        stats
    }
}
