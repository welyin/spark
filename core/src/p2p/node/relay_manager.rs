//! relay 预约管理（peer-rediscovery §4.6）：relay 候选选择、预约请求、
//! 预约状态管理与 `relay::client::Event` 处理。
//!
//! 蜂窝 CGNAT 下入站不可达，DHT 记录中只有直连地址等于没有地址。relay
//! 电路地址是 DHT 记录的一等字段。本模块从零实现 relay client 预约：
//! 移动端（Client 模式）也维护 1 主 1 备共 2 个预约——预约是出站操作，
//! 不受 CGNAT 限制。
//!
//! 注：rust-libp2p relay client 在 circuit listener 存活期间会自动续期
//! 预约，无需实现续期逻辑；只需在 `ReservationReqFailed`/连接断开时移除
//! 并重选候选（§6 WP1.4）。

use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};

use crate::storage::StorageBackend;

use super::event_loop::EventLoop;
use super::relay_manager;

/// relay 预约状态。
#[derive(Clone, Debug)]
pub struct RelayReservation {
    /// relay 节点的 peerId。
    pub relay_peer: PeerId,
    /// 获得的电路地址（含 /p2p-circuit 后缀）。
    pub circuit_addr: Multiaddr,
    /// 预约建立时刻（now_ms；保留供续期阈值判断，当前由 libp2p 自动续期）。
    #[allow(dead_code)]
    pub created_at: i64,
}

impl<S: StorageBackend> EventLoop<S> {
    /// 选择 relay 候选：从邻居池挑选活跃度高且已连接的 peer，优先具备 relay
    /// server 能力（identify 协议清单含 hop 协议）的。
    ///
    /// 简化实现：从已连接 peer 中筛选具备 relay hop 能力的；若无则回退到
    /// 已连接 peer 全集（活跃度靠后由拨号结果纠偏）。
    pub(super) fn select_relay_candidates(&mut self) -> Vec<PeerId> {
        let mut candidates = Vec::new();
        // 已连接且 identify 报告了 /libp2p/circuit/relay/0.2.0/hop 协议的 peer
        for peer in self.connected_peers() {
            let is_hop = self
                .peer_protocols
                .get(&peer)
                .is_some_and(|ps| ps.iter().any(|p| p.contains("/circuit/relay/") && p.ends_with("/hop")));
            if is_hop {
                candidates.push(peer);
            }
        }
        if candidates.is_empty() {
            // 回退：已连接 peer 全集（relay 资历校验在协议层完成）
            candidates = self.connected_peers().into_iter().collect();
        }
        // 排除已 in-flight 或已预约的
        candidates.retain(|p| {
            !self.relay_reservations_inflight.contains(p)
                && !self.relay_reservations.iter().any(|r| r.relay_peer == *p)
        });
        // 限制到目标数
        let target = crate::p2p::constants::RELAY_RESERVATION_TARGET;
        candidates.truncate(target);
        candidates
    }

    /// 尝试向指定 relay peer 建立预约：在 relay 地址上追加 /p2p-circuit 并监听。
    /// 成功后 libp2p 会向 relay 发预约请求，`ReservationReqAccepted` 事件回传。
    pub(super) fn request_relay_reservation(&mut self, relay_peer: PeerId) {
        // 已 in-flight 或已预约，不重复发起
        if self.relay_reservations_inflight.contains(&relay_peer)
            || self
                .relay_reservations
                .iter()
                .any(|r| r.relay_peer == relay_peer)
        {
            return;
        }
        // 构造 /p2p/<relayPeer>/p2p-circuit 监听地址
        let mut circuit_addr = Multiaddr::empty();
        circuit_addr.push(Protocol::P2p(relay_peer.into()));
        circuit_addr.push(Protocol::P2pCircuit);
        if self.swarm.listen_on(circuit_addr).is_err() {
            self.emit(super::P2pEvent::Warning(format!(
                "relay listen failed for {relay_peer}"
            )));
            return;
        }
        self.relay_reservations_inflight.insert(relay_peer);
    }

    /// 预约成功：记录预约并触发 announce + DHT 重发布（地址列表变化了）。
    /// `relay_peer` 为发出预约的 relay 节点 peerId。
    ///
    /// 对外发布的电路地址必须采用完整形式 `/ip4/<relayIP>/tcp/<port>/p2p/<relayPeer>/p2p-circuit`
    /// （§4.6.1：对端可据其中 relay 的完整地址直接拨号走中继）；裸 `/p2p/<relayPeer>/p2p-circuit`
    /// 缺 relay 可达地址，冷启动拨号方拨不动。
    pub(super) fn on_reservation_accepted(&mut self, relay_peer: PeerId) {
        // 预约确认：结束 in-flight
        self.relay_reservations_inflight.remove(&relay_peer);
        let circuit_addr = self.build_circuit_address(relay_peer);
        let now = self.now();
        self.relay_reservations.retain(|r| r.relay_peer != relay_peer);
        self.relay_reservations.push(relay_manager::RelayReservation {
            relay_peer,
            circuit_addr,
            created_at: now,
        });
        // 地址列表变化 → 重发布 announce + DHT
        let _ = self.publish_announce();
        self.publish_node_presence_record();
    }

    /// 构造对外可达的完整电路地址：取 relay peer 的邻居池已知地址（过滤不可路由/
    /// link-local），追加 `/p2p/<relayPeer>/p2p-circuit`。若拿不到完整地址则回退为
    /// `/p2p/<relayPeer>/p2p-circuit`（对端已有该 relay 连接时仍可用）。
    fn build_circuit_address(&mut self, relay_peer: PeerId) -> Multiaddr {
        let base_addrs: Vec<String> = {
            let mut store = crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
            store
                .get(&relay_peer.to_base58())
                .ok()
                .flatten()
                .map(|r| r.addresses)
                .unwrap_or_default()
        };
        // 优先 IPv4/IPv6 直连地址（去掉已含 /p2p-circuit 或 /p2p/<peer> 的）
        let mut best: Option<Multiaddr> = None;
        for raw in base_addrs {
            let Ok(ma) = raw.parse::<Multiaddr>() else {
                continue;
            };
            let mut has_transport = false;
            for p in ma.iter() {
                match p {
                    Protocol::Ip4(ip) if !ip.is_unspecified() => has_transport = true,
                    Protocol::Ip6(ip) if !ip.is_unspecified() && !ip.is_loopback() => {
                        has_transport = true
                    }
                    _ => {}
                }
            }
            if has_transport {
                best = Some(ma);
                break;
            }
        }
        if let Some(mut ma) = best {
            ma.push(Protocol::P2p(relay_peer.into()));
            ma.push(Protocol::P2pCircuit);
            return ma;
        }
        // 回退：裸电路地址
        let mut fallback = Multiaddr::empty();
        fallback.push(Protocol::P2p(relay_peer.into()));
        fallback.push(Protocol::P2pCircuit);
        fallback
    }

    /// relay 连接断开或预约失败：移除该 relay 的预约与 in-flight，若仍有名额则重选候选。
    /// 预约请求 in-flight 期间连接断开（未收到 ReservationReqAccepted）也计入名额，
    /// 故补选条件看「实际预约 + in-flight」是否低于目标。
    pub(super) fn on_relay_connection_lost(&mut self, relay_peer: PeerId) {
        self.relay_reservations.retain(|r| r.relay_peer != relay_peer);
        self.relay_reservations_inflight.remove(&relay_peer);
        let target = crate::p2p::constants::RELAY_RESERVATION_TARGET;
        let occupied = self.relay_reservations.len() + self.relay_reservations_inflight.len();
        if occupied >= target {
            return;
        }
        // 尝试补充一个预约
        for candidate in self.select_relay_candidates() {
            if candidate != relay_peer
                && !self.relay_reservations.iter().any(|r| r.relay_peer == candidate)
                && !self.relay_reservations_inflight.contains(&candidate)
            {
                self.request_relay_reservation(candidate);
                break;
            }
        }
    }

    /// 确保预约数达到目标：不足时从候选补齐（幂等，供启动/切网/周期 tick 调用）。
    /// 候选需排除 in-flight，避免对同一 relay 重复发预约请求。
    pub(super) fn ensure_relay_reservations(&mut self) {
        let target = crate::p2p::constants::RELAY_RESERVATION_TARGET;
        if self.relay_reservations.len() >= target {
            return;
        }
        let candidates = self.select_relay_candidates();
        for candidate in candidates {
            if self.relay_reservations.len() >= target {
                break;
            }
            let already = self
                .relay_reservations
                .iter()
                .any(|r| r.relay_peer == candidate);
            if !already {
                self.request_relay_reservation(candidate);
            }
        }
    }

    /// 电路监听关闭（SwarmEvent::ListenerClosed，N3）：libp2p-relay 0.21 client
    /// 没有 ReservationReqFailed/Denied 事件，预约被拒/失败/过期由 transport
    /// 关闭对应电路监听上行。此处清理该 relay 的预约与 in-flight 标记使其可
    /// 被重选；重选交给周期 tick 的 ensure_relay_reservations（不引入新定时器）。
    pub(super) fn on_circuit_listener_closed(&mut self, addresses: &[Multiaddr]) {
        for addr in addresses {
            let Some(relay_peer) = circuit_addr_relay_peer(addr) else {
                continue;
            };
            let was_inflight = self.relay_reservations_inflight.remove(&relay_peer);
            let before = self.relay_reservations.len();
            self.relay_reservations.retain(|r| r.relay_peer != relay_peer);
            if was_inflight || self.relay_reservations.len() != before {
                self.emit(super::P2pEvent::Warning(format!(
                    "relay circuit listener closed for {relay_peer}"
                )));
            }
        }
    }
}

/// 从电路监听地址（…/p2p/<relayPeer>/p2p-circuit）提取 relay peer。
/// 仅当地址含 /p2p-circuit 段时返回——普通监听地址关闭不涉及预约状态。
fn circuit_addr_relay_peer(addr: &Multiaddr) -> Option<PeerId> {
    let mut relay = None;
    let mut has_circuit = false;
    for p in addr.iter() {
        match p {
            Protocol::P2p(peer_id) => relay = Some(peer_id),
            Protocol::P2pCircuit => has_circuit = true,
            _ => {}
        }
    }
    if has_circuit { relay } else { None }
}
