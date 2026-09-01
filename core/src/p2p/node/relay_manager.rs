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
use libp2p::{Multiaddr, PeerId, autonat, kad};

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

/// `spark:relay` 共享池记录载荷（relay-implementation §2 R2，org 网关成员
/// 提示同型：`{peerId, addresses}` 紧凑 JSON）。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProviderHint {
    /// relay 节点 libp2p peerId。
    #[serde(rename = "peerId")]
    pub peer_id: String,
    /// relay 节点公网可达 multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
    /// relay 稳定性标记（R1 动态 IP 降权：`Some("low")` = stability=low，
    /// 消费方排序降权垫底；旧记录无此字段视为正常）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<String>,
}

impl RelayProviderHint {
    /// 序列化为 DHT 记录值（紧凑 JSON，与 OrgMemberHint 同口径）。
    pub fn to_record_value(&self) -> Vec<u8> {
        serde_json::to_string(self)
            .unwrap_or_else(|_| "{}".to_string())
            .into_bytes()
    }

    /// 从 DHT 记录值解析：形状不符返回 `None`（静默丢弃口径）。
    /// 当前仅测试/R3 候选解析消费（get_providers 结果地址段的复核）。
    #[allow(dead_code)]
    pub fn from_record_value(value: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(value).ok()?;
        let hint: Self = serde_json::from_str(text).ok()?;
        if hint.peer_id.trim().is_empty() {
            return None;
        }
        Some(hint)
    }
}

impl<S: StorageBackend> EventLoop<S> {
    /// 选择 relay 候选（R3 排序纪律，relay-implementation §2；直连失败后的
    /// relay 候选序）：
    /// ① 自设备/本组织成员的公网 relay（已连接 + hop 能力 ∩ 宿主谓词
    ///   `is_self_device_or_org_member`——设备清单/成员表由 kernel 宿主注入，
    ///   p2p 层不直接依赖业务表）；
    /// ② `spark:relay` 共享池候选（R2 `relay_pool_candidates` 落点）；
    /// ③④ 既有兜底（保留原行为）：其他已连接 + hop，再已连接全集。
    /// 跨层不混排；层内按 peer_activity last_seen 降序、`stability_low` 垫底；
    /// 目标数沿用 [`RELAY_RESERVATION_TARGET`]（1 主 1 备）。
    pub(super) fn select_relay_candidates(&mut self) -> Vec<PeerId> {
        let connected = self.connected_peers();
        let is_hop = |p: &PeerId| {
            self.peer_protocols.get(p).is_some_and(|ps| {
                ps.iter()
                    .any(|p| p.contains("/circuit/relay/") && p.ends_with("/hop"))
            })
        };
        // last_seen 一次取齐（peer_activity；pool 候选可能未连接，一并查）
        let mut last_seen_map: std::collections::HashMap<PeerId, i64> =
            std::collections::HashMap::new();
        {
            let mut store =
                crate::p2p::peer_activity::PeerActivityStore::new(&mut self.storage);
            for p in connected.iter().chain(self.relay_pool_candidates.iter()) {
                if let Ok(Some(rec)) = store.get(&p.to_base58()) {
                    last_seen_map.insert(*p, rec.last_seen_at);
                }
            }
        }
        let last_seen = |p: &PeerId| last_seen_map.get(p).copied().unwrap_or(0);
        let is_low = |p: &PeerId| {
            self.relay_pool_stability.get(p).copied().unwrap_or(false)
        };

        // ① 自设备/本组织成员 relay（已连接 + hop ∩ 宿主谓词）
        let tier1: Vec<PeerId> = connected
            .iter()
            .filter(|p| is_hop(p) && self.host.is_self_device_or_org_member(&p.to_base58()))
            .copied()
            .collect();
        // ② 共享池候选（可未连接——R2 查询结果侧已发起拨号，此处直接入选）
        let tier2 = self.relay_pool_candidates.clone();
        // ③ 其他已连接 + hop（非梯队①成员）
        let tier3: Vec<PeerId> = connected
            .iter()
            .filter(|p| is_hop(p) && !tier1.contains(p))
            .copied()
            .collect();
        // ④ 已连接全集其余（原「无 hop 回退全集」语义保留为末位兜底）
        let tier4: Vec<PeerId> = connected.iter().filter(|p| !is_hop(p)).copied().collect();

        let mut out = assemble_relay_tiers([tier1, tier2, tier3, tier4], &last_seen, &is_low);
        // 排除已 in-flight 或已预约的
        out.retain(|p| {
            !self.relay_reservations_inflight.contains(p)
                && !self.relay_reservations.iter().any(|r| r.relay_peer == *p)
        });
        // 限制到目标数
        out.truncate(crate::p2p::constants::RELAY_RESERVATION_TARGET);
        out
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
        // 电路监听地址必须携带 relay 的完整传输地址
        // （<relay-addr>/p2p/<relayPeer>/p2p-circuit）：libp2p-relay 0.21
        // client transport 对裸 /p2p/<relayPeer>/p2p-circuit 返回
        // MissingRelayAddr（listen_on Err，预约永远无法形成）。
        let circuit_addr = self.build_circuit_address(relay_peer);
        if !multiaddr_has_transport(&circuit_addr) {
            // overlay 尚无该 relay 的可用传输地址，listen_on 必失败——
            // 本轮跳过，等地址到位后由周期 tick 重试。
            return;
        }
        eprintln!("[p2p] relay reservation request -> {relay_peer} addr={circuit_addr}");
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
        eprintln!("[p2p] relay reservation accepted from {relay_peer}");
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
    pub(super) fn build_circuit_address(&mut self, relay_peer: PeerId) -> Multiaddr {
        let base_addrs: Vec<String> = {
            let mut store = crate::p2p::overlay_store::OverlayPeerStore::new(&mut self.storage);
            store
                .get(&relay_peer.to_base58())
                .ok()
                .flatten()
                .map(|r| r.addresses)
                .unwrap_or_default()
        };
        // 基地址选取：IPv4/IPv6 直连地址，跳过已含 /p2p-circuit 的电路地址
        // （真机实测：relay 的电路地址被误作基地址再拼 /p2p/<relay>/p2p-circuit
        // 产生双电路段，对端拨号 MultipleCircuitRelayProtocolsUnsupported）；
        // 有多个候选时偏好非 ws 形态（Android 无 ws 传输，ws 电路地址对手机
        // 不可拨）。
        let mut best: Option<Multiaddr> = None;
        let mut best_ws: Option<Multiaddr> = None;
        for raw in base_addrs {
            let Ok(ma) = raw.parse::<Multiaddr>() else {
                continue;
            };
            let mut has_transport = false;
            let mut is_circuit = false;
            let mut is_ws = false;
            for p in ma.iter() {
                match p {
                    Protocol::Ip4(ip) if !ip.is_unspecified() => has_transport = true,
                    Protocol::Ip6(ip) if !ip.is_unspecified() && !ip.is_loopback() => {
                        has_transport = true
                    }
                    Protocol::P2pCircuit => is_circuit = true,
                    Protocol::Ws(_) | Protocol::Wss(_) => is_ws = true,
                    _ => {}
                }
            }
            if !has_transport || is_circuit {
                continue;
            }
            if is_ws {
                if best_ws.is_none() {
                    best_ws = Some(ma);
                }
                continue;
            }
            best = Some(ma);
            break;
        }
        let best = best.or(best_ws);
        if let Some(mut ma) = best {
            // 基地址可能自带 /p2p/<peer> 尾段（overlay 来源不一）：剥掉再拼，
            // 否则得到 .../p2p/<relay>/p2p/<relay>/p2p-circuit 双重尾段，
            // 电路监听/拨号无法成立（真机实测）。
            if matches!(ma.iter().last(), Some(Protocol::P2p(_))) {
                ma.pop();
            }
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
        if candidates.is_empty() {
            // R2（relay-implementation §2）：无已连接候选 → 查 spark:relay
            // 共享池补充（节流 60s；结果经 resolve_dht_providers 分流拨号）
            self.maybe_query_relay_pool();
            return;
        }
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

    // ------------------------------------------------------------------
    // R1：AutoNAT 公网判定驱动 relay server 自动启停（relay-implementation §2）
    // ------------------------------------------------------------------

    /// AutoNAT `NatStatus` 变化（swarm_events 接线）：
    /// - Public → 启用 relay server 角色（hop 可服务）并对 `spark:relay` 共享池
    ///   provide（R2，含已挂载时的首次 Public 确认——刷新地址载荷）；
    /// - Private → 摘牌：新连接不再接受预约（既有预约由各自连接 handler 服务
    ///   到期），并撤下共享池 provide；
    /// - Unknown → 不变更现状。
    /// 显式 `enable_relay_server: false`（用户手动关/移动端）优先于自动判定，
    /// 角色永不开；leaf 双保险（kad client 不服务 + begin_dht_provide 守卫）。
    pub(super) fn on_autonat_status_changed(&mut self, status: autonat::NatStatus) {
        // U1 状态页快照：无论角色配置如何都记录最新判定
        self.nat_status = match &status {
            autonat::NatStatus::Public(_) => super::NatStatusLabel::Public,
            autonat::NatStatus::Private => super::NatStatusLabel::Private,
            autonat::NatStatus::Unknown => super::NatStatusLabel::Unknown,
        };
        if !self.enable_relay_server {
            return;
        }
        match status {
            autonat::NatStatus::Public(ref addr) => {
                if self.swarm.behaviour().relay_server.as_ref().is_none() {
                    let local = self.self_peer_id();
                    self.swarm.behaviour_mut().relay_server =
                        libp2p::swarm::behaviour::toggle::Toggle::from(Some(
                            crate::p2p::behaviour::build_relay_server(local),
                        ));
                }
                // 预约响应地址来源：登记 external address（含判定地址本身）
                self.register_relay_external_addrs(Some(addr.clone()));
                // R2：就绪（Public + 角色开启）即对共享池 provide/刷新
                self.provide_relay_pool_record();
            }
            autonat::NatStatus::Private => {
                if self.swarm.behaviour().relay_server.as_ref().is_some() {
                    self.swarm.behaviour_mut().relay_server =
                        libp2p::swarm::behaviour::toggle::Toggle::from(None);
                }
                // 撤下共享池 provide：停止 provider 声明 + 取消周期重发登记
                if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                    kad.stop_providing(&kad::RecordKey::new(
                        &crate::p2p::constants::SPARK_RELAY_KEY,
                    ));
                }
                self.provided_records
                    .remove(crate::p2p::constants::SPARK_RELAY_KEY.as_bytes());
            }
            autonat::NatStatus::Unknown => {}
        }
    }

    /// relay 角色服务期间登记 external address：libp2p relay server 的预约
    /// 响应只携带 swarm external addresses——空集时客户端以
    /// NoAddressesInReservation 失败，预约永远无法成立（真机实测）。登记范围
    /// = 公网段监听地址（与 spark:relay 共享池发布同口径
    /// is_public_external_addr）+ 可选显式地址（AutoNAT Public 判定地址、
    /// UPnP 映射地址）。角色未服务时（Toggle None）不登记。
    pub(super) fn register_relay_external_addrs(&mut self, extra: Option<Multiaddr>) {
        if self.swarm.behaviour().relay_server.as_ref().is_none() {
            return;
        }
        if let Some(addr) = extra {
            self.swarm.add_external_address(addr);
        }
        let candidates: Vec<Multiaddr> = self
            .listen_addr_strings()
            .into_iter()
            .filter_map(|a| a.parse::<Multiaddr>().ok())
            .filter(|ma| crate::p2p::peer_targets::is_public_external_addr(ma))
            .collect();
        for ma in candidates {
            self.swarm.add_external_address(ma);
        }
    }

    /// R2 提供侧：对约定键 `spark:relay` 做 provide（复用 `begin_dht_provide`
    /// 既有路径——start_providing + put_record + 登记 tick 周期重发）。载荷 =
    /// 本机 peerId + listen_addr_strings 的**公网段**地址集；公网段为空
    /// （无 external 确认地址）不发布——非公网地址对拨号方无用。
    pub(super) fn provide_relay_pool_record(&mut self) {
        let public_addrs: Vec<String> = self
            .listen_addr_strings()
            .into_iter()
            .filter(|a| {
                a.parse::<Multiaddr>()
                    .map(|ma| crate::p2p::peer_targets::is_public_external_addr(&ma))
                    .unwrap_or(false)
            })
            .collect();
        if public_addrs.is_empty() {
            return;
        }
        let hint = RelayProviderHint {
            peer_id: self.self_peer_id().to_base58(),
            addresses: public_addrs,
            // R1 动态 IP 降权：本机 stability=low 时随载荷宣告，消费方（R3
            // 排序）据此降权垫底
            stability: self
                .relay_stability_low
                .then(|| "low".to_string()),
        };
        let (tx, _rx) = tokio::sync::oneshot::channel();
        self.begin_dht_provide(
            crate::p2p::constants::SPARK_RELAY_KEY.as_bytes().to_vec(),
            hint.to_record_value(),
            tx,
        );
    }

    /// U1 状态快照（relay-implementation §3，只读 facade）：AutoNAT 判定、
    /// relay 角色、活跃预约（到期/配额为近似值，见字段注释）、共享池候选数、
    /// stability 标记。
    pub(super) fn local_relay_status(&self) -> super::LocalRelayStatus {
        let now = self.now();
        let reservations = self
            .relay_reservations
            .iter()
            .map(|r| super::RelayReservationInfo {
                peer: r.relay_peer.to_base58(),
                expires_in_ms: (r.created_at
                    + (crate::p2p::constants::RELAY_DEFAULT_DURATION_LIMIT_SECS * 1000) as i64
                    - now)
                    .max(0),
                limit_bytes: crate::p2p::constants::RELAY_DEFAULT_DATA_LIMIT_BYTES,
                used_bytes: None,
            })
            .collect();
        super::LocalRelayStatus {
            autonat: self.nat_status.as_str().to_string(),
            relay_role: if self.swarm.behaviour().relay_server.as_ref().is_some() {
                "serving".to_string()
            } else {
                "off".to_string()
            },
            reservations,
            pool_size: self.relay_pool_candidates.len(),
            stability_low: self.relay_stability_low,
            upnp: if self.upnp_mapping.is_some() {
                "mapped"
            } else if self.upnp_failed {
                "failed"
            } else {
                "unknown"
            }
            .to_string(),
        }
    }

    // ------------------------------------------------------------------
    // R2 客户端：spark:relay 共享池查询 → 候选源（relay-implementation §2）
    // ------------------------------------------------------------------

    /// 无已连接候选时对 `spark:relay` 做 get_providers（kad client 一次性查询
    /// 兼容，leaf 同路径；节流 60s，懒连接纪律失败沉默）。
    pub(super) fn maybe_query_relay_pool(&mut self) {
        if self.swarm.behaviour().kad.as_ref().is_none() {
            return;
        }
        // 已有查询在途不重复发起（主守卫，与时间节流互补）
        if !self.relay_pool_queries.is_empty() {
            return;
        }
        let now = self.now();
        if self.last_relay_pool_query_at != 0
            && now - self.last_relay_pool_query_at
                < crate::p2p::constants::RELAY_POOL_QUERY_MIN_INTERVAL_MS
        {
            return;
        }
        self.last_relay_pool_query_at = now;
        let query_id = self
            .swarm
            .behaviour_mut()
            .kad
            .as_mut()
            .expect("kad checked above")
            .get_providers(kad::RecordKey::new(
                &crate::p2p::constants::SPARK_RELAY_KEY,
            ));
        self.relay_pool_queries.insert(query_id);
    }

    /// 共享池查询结果（resolve_dht_providers 分流）：providers 落为候选源
    /// （R3 排序消费），并逐个拨号——kad 在 get_providers 过程中经
    /// ADD_PROVIDER 已把 provider 地址灌进路由表，DialOpts 仅带 peerId 即可；
    /// 连上后由 ensure_relay_reservations 走既有候选选择预约。
    pub(super) fn on_relay_pool_providers(&mut self, result: kad::GetProvidersResult) {
        let Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) = result else {
            return;
        };
        let self_id = self.self_peer_id();
        for peer in providers {
            if peer == self_id
                || self.swarm.is_connected(&peer)
                || self.relay_reservations_inflight.contains(&peer)
                || self.relay_reservations.iter().any(|r| r.relay_peer == peer)
            {
                continue;
            }
            // 候选源登记（去重，上限 8 防膨胀；R3 将按纪律排序消费）
            if !self.relay_pool_candidates.contains(&peer) {
                self.relay_pool_candidates.push(peer);
                self.relay_pool_candidates.truncate(8);
            }
            let opts = libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                .allocate_new_port()
                .build();
            let _ = self.swarm.dial(opts);
        }
        // #2 stability 回填：get_providers 只回 PeerId——对 spark:relay 补一次
        // get_record 取载荷解析 stability（last-writer 单记录，多 provider 的
        // stability 以现存记录为准；无载荷/失败静默，保持「无降权」现状）
        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            let query_id = kad.get_record(kad::RecordKey::new(
                &crate::p2p::constants::SPARK_RELAY_KEY,
            ));
            self.relay_pool_record_queries.insert(query_id);
        }
    }

    /// #2：spark:relay 载荷回填（resolve_dht_get 分流）——解析
    /// [`RelayProviderHint`] 的 stability，低稳候选记入 `relay_pool_stability`
    /// （R3 `sort_relay_tier` 据此垫底）；非 low/缺字段清除标记（覆盖旧值）。
    pub(super) fn on_relay_pool_record(&mut self, result: kad::GetRecordResult) {
        let Ok(kad::GetRecordOk::FoundRecord(peer_record)) = result else {
            return;
        };
        let Some(hint) = RelayProviderHint::from_record_value(&peer_record.record.value)
        else {
            return;
        };
        let Ok(peer) = hint.peer_id.parse::<PeerId>() else {
            return;
        };
        if hint.stability.as_deref() == Some("low") {
            self.relay_pool_stability.insert(peer, true);
        } else {
            self.relay_pool_stability.remove(&peer);
        }
    }
}

/// 从电路监听地址（…/p2p/<relayPeer>/p2p-circuit）提取 relay peer。
/// 仅当地址含 /p2p-circuit 段时返回——普通监听地址关闭不涉及预约状态。
/// multiaddr 是否含可用传输段（非 unspecified 的 Ip4/Ip6）——电路监听地址
/// 的 relay 段完整性检查（build_circuit_address 回退裸形式时为 false）。
fn multiaddr_has_transport(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| match p {
        Protocol::Ip4(ip) => !ip.is_unspecified(),
        Protocol::Ip6(ip) => !ip.is_unspecified() && !ip.is_loopback(),
        _ => false,
    })
}

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

/// R3 层内排序（relay-implementation §2）：last_seen 降序；`stability_low`
/// 的节点层内降权垫底（低稳组内仍按 last_seen 降序）。稳定排序：同分时
/// 保持调用方给的原始顺序。
pub fn sort_relay_tier(
    peers: &mut [PeerId],
    last_seen: &dyn Fn(&PeerId) -> i64,
    is_low_stability: &dyn Fn(&PeerId) -> bool,
) {
    peers.sort_by_key(|p| (is_low_stability(p), std::cmp::Reverse(last_seen(p))));
}

/// R3 梯队拼接（relay-implementation §2）：梯队顺序即入参顺序（① 自设备/
/// 本组织成员 relay → ② spark:relay 共享池 → ③④ 既有兜底）；每层先按
/// [`sort_relay_tier`] 排序，跨层不混排、跨层去重保序。
pub fn assemble_relay_tiers(
    tiers: [Vec<PeerId>; 4],
    last_seen: &dyn Fn(&PeerId) -> i64,
    is_low_stability: &dyn Fn(&PeerId) -> bool,
) -> Vec<PeerId> {
    let mut out: Vec<PeerId> = Vec::new();
    for mut tier in tiers {
        sort_relay_tier(&mut tier, last_seen, is_low_stability);
        for p in tier {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(n: u8) -> Vec<PeerId> {
        (0..n).map(|_| PeerId::random()).collect()
    }

    /// R3 排序纪律（relay-implementation §2）：三梯队混合——① 成员 relay 最前；
    /// ② 共享池层内 last_seen 降序、stability_low 垫底（即便 last_seen 最新）；
    /// ③④ 兜底在后；跨层不混排、跨层去重。
    #[test]
    fn assemble_relay_tiers_orders_by_tier_last_seen_stability() {
        let [m1] = peers(1).try_into().unwrap();
        let [pool_hi, pool_lo, pool_low] = peers(3).try_into().unwrap();
        let [h1] = peers(1).try_into().unwrap();
        let [c1] = peers(1).try_into().unwrap();
        let last_seen = |p: &PeerId| -> i64 {
            if *p == pool_hi {
                200
            } else if *p == pool_lo {
                100
            } else if *p == pool_low {
                999 // stability_low：last_seen 最新也垫底
            } else if *p == m1 {
                10
            } else {
                0
            }
        };
        let is_low = |p: &PeerId| *p == pool_low;
        // tier2 乱序输入 + 与 tier1 重复元素（去重验证）
        let out = assemble_relay_tiers(
            [
                vec![m1],
                vec![pool_low, pool_lo, pool_hi, m1],
                vec![h1],
                vec![c1, h1],
            ],
            &last_seen,
            &is_low,
        );
        assert_eq!(
            out,
            vec![m1, pool_hi, pool_lo, pool_low, h1, c1],
            "梯队序 + 层内 last_seen 降序 + low 垫底 + 跨层去重"
        );
    }

    /// R3 层内排序：last_seen 降序；low 垫底且组内仍按 last_seen。
    #[test]
    fn sort_relay_tier_last_seen_desc_low_last() {
        let [a, b, c] = peers(3).try_into().unwrap();
        let last_seen = |p: &PeerId| -> i64 {
            if *p == a {
                300
            } else if *p == b {
                100
            } else {
                900
            }
        };
        let is_low = |p: &PeerId| *p == c;
        let mut tier = vec![b, c, a];
        sort_relay_tier(&mut tier, &last_seen, &is_low);
        assert_eq!(tier, vec![a, b, c]);
    }
}
