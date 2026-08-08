//! Kad（公共 DHT）与 node-challenge：路由表播种、记录读写与 provider 职责、
//! 节点存在记录周期重发、DHT 命中记录的三层身份确认（①announce 验签 →
//! ②identify 协议清单 → ③challenge-response），以及显式 challenge 命令。

use std::time::Duration;

use libp2p::{Multiaddr, PeerId, kad, request_response};
use tokio::sync::oneshot;

use crate::org::gateway::OrgMemberHint;
use crate::p2p::announce::{
    announce_to_json, node_presence_record_key, prepare_publish_addresses, sign_node_announce,
    verify_announce_text,
};
use crate::p2p::challenge;
use crate::p2p::constants::DHT_RECORD_TTL_SECS;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::event_loop::EventLoop;

impl<S: StorageBackend> EventLoop<S> {
    /// 启动时把邻居池现有条目灌进 kad 路由表并 bootstrap（dht_mode = Off 时不挂 kad，跳过）。
    pub(super) fn seed_kad_routing(&mut self) {
        if self.swarm.behaviour().kad.as_ref().is_none() {
            return;
        }
        let records = {
            let mut store = OverlayPeerStore::new(&mut self.storage);
            store.list_all().unwrap_or_default()
        };
        let self_id = self.self_peer_id();
        let mut added = false;
        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            for record in records {
                let Ok(peer) = record.peer_id.parse::<PeerId>() else {
                    continue;
                };
                if peer == self_id {
                    continue;
                }
                for addr in record
                    .addresses
                    .iter()
                    .filter_map(|a| a.parse::<Multiaddr>().ok())
                {
                    kad.add_address(&peer, addr);
                    added = true;
                }
            }
            if added {
                let _ = kad.bootstrap();
            }
        }
    }

    pub(super) fn begin_dht_put(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
        tx: oneshot::Sender<Result<()>>,
    ) {
        let record = kad::Record {
            key: kad::RecordKey::new(&key),
            value,
            publisher: Some(self.self_peer_id()),
            expires: Some(std::time::Instant::now() + Duration::from_secs(DHT_RECORD_TTL_SECS)),
        };
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            let _ = tx.send(Err(P2pError::Protocol("dht disabled".to_string())));
            return;
        };
        match kad.put_record(record, kad::Quorum::One) {
            Ok(query_id) => {
                self.pending_dht_put.insert(query_id, tx);
            }
            Err(e) => {
                let _ = tx.send(Err(P2pError::Protocol(format!("dht put failed: {e}"))));
            }
        }
    }

    pub(super) fn begin_dht_get(
        &mut self,
        key: Vec<u8>,
        tx: oneshot::Sender<Result<Option<Vec<u8>>>>,
    ) {
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            let _ = tx.send(Err(P2pError::Protocol("dht disabled".to_string())));
            return;
        };
        let query_id = kad.get_record(kad::RecordKey::new(&key));
        self.pending_dht_get.insert(query_id, tx);
    }

    /// 网关职责的 provide：start_providing + put_record，并登记周期重发。
    /// 相同 (key, value) 幂等空操作；失败时撤销登记，调用方可重试。
    pub(super) fn begin_dht_provide(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
        tx: oneshot::Sender<Result<()>>,
    ) {
        if self.provided_records.get(&key) == Some(&value) {
            let _ = tx.send(Ok(()));
            return;
        }
        if self.swarm.behaviour().kad.as_ref().is_none() {
            let _ = tx.send(Err(P2pError::Protocol("dht disabled".to_string())));
            return;
        }
        self.provided_records.insert(key.clone(), value.clone());
        match self.republish_provided(&key, &value) {
            Ok(()) => {
                let _ = tx.send(Ok(()));
            }
            Err(e) => {
                self.provided_records.remove(&key);
                let _ = tx.send(Err(e));
            }
        }
    }

    /// 单个 key 的 start_providing + put_record（首次调用与周期重发共用）。
    pub(super) fn republish_provided(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let record = kad::Record {
            key: kad::RecordKey::new(&key),
            value: value.to_vec(),
            publisher: Some(self.self_peer_id()),
            expires: Some(std::time::Instant::now() + Duration::from_secs(DHT_RECORD_TTL_SECS)),
        };
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            return Err(P2pError::Protocol("dht disabled".to_string()));
        };
        // provider 声明与记录发布彼此独立：无已知路由节点时 start_providing 会
        // 报 NoKnownPeers，本地仍持有记录，路由表建立后的周期重发会补上
        let _ = kad.start_providing(kad::RecordKey::new(&key));
        kad.put_record(record, kad::Quorum::One)
            .map(|_| ())
            .map_err(|e| P2pError::Protocol(format!("dht provide failed: {e}")))
    }

    pub(super) fn begin_dht_get_providers(
        &mut self,
        key: Vec<u8>,
        tx: oneshot::Sender<Result<Vec<String>>>,
    ) {
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            let _ = tx.send(Err(P2pError::Protocol("dht disabled".to_string())));
            return;
        };
        let query_id = kad.get_providers(kad::RecordKey::new(&key));
        self.pending_dht_providers.insert(query_id, tx);
    }

    pub(super) fn resolve_dht_providers(
        &mut self,
        query_id: kad::QueryId,
        result: kad::GetProvidersResult,
    ) {
        let Some(tx) = self.pending_dht_providers.remove(&query_id) else {
            return;
        };
        match result {
            Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) => {
                let _ = tx.send(Ok(providers.iter().map(ToString::to_string).collect()));
            }
            Ok(kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {
                let _ = tx.send(Ok(Vec::new()));
            }
            Err(e) => {
                let _ = tx.send(Err(P2pError::Protocol(format!(
                    "dht get providers failed: {e}"
                ))));
            }
        }
    }

    pub(super) fn resolve_dht_put(&mut self, query_id: kad::QueryId, result: kad::PutRecordResult) {
        let Some(tx) = self.pending_dht_put.remove(&query_id) else {
            return;
        };
        match result {
            Ok(_) => {
                let _ = tx.send(Ok(()));
            }
            Err(e) => {
                let _ = tx.send(Err(P2pError::Protocol(format!("dht put failed: {e}"))));
            }
        }
    }

    pub(super) fn resolve_dht_get(&mut self, query_id: kad::QueryId, result: kad::GetRecordResult) {
        // 竞速查询：优先类目 peer 的 DHT 竞速命中/未命中走 rediscovery 分支
        if let Some(peer) = self.rediscovery_dht_queries.remove(&query_id) {
            match &result {
                Ok(kad::GetRecordOk::FoundRecord(peer_record)) => {
                    self.on_rediscovery_dht_hit(peer, &peer_record.record.value, Some(query_id));
                }
                _ => {
                    self.on_rediscovery_dht_miss(peer, Some(query_id));
                }
            }
            // 竞速查询不属于普通 pending_dht_get，直接返回
            return;
        }
        match result {
            Ok(kad::GetRecordOk::FoundRecord(peer_record)) => {
                if let Some(tx) = self.pending_dht_get.remove(&query_id) {
                    let _ = tx.send(Ok(Some(peer_record.record.value.clone())));
                }
                // 命中节点存在记录：走三层确认后按未验证口径入邻居池
                self.confirm_dht_node_record(&peer_record.record.value);
                // 命中组织私有 DHT 成员提示（§15 的 {peerId, addresses} 线形）：
                // 回填业务层，由宿主按未验证口径入邻居池
                if let Some(hint) = OrgMemberHint::from_record_value(&peer_record.record.value)
                    && hint.peer_id != self.self_peer_id().to_base58()
                {
                    self.host.on_org_member_hints(&[hint]);
                }
                // 命中组织地址记录（§16 线形）：五步校验链通过即沉淀本地缓存
                // （解析顺序的 DHT 命中落缓存环节；gossip 命中同样沉淀）
                if let Ok(record) = serde_json::from_slice::<crate::org::OrgAddressRecord>(
                    &peer_record.record.value,
                ) && crate::org::verify_org_address_record(&record, self.now()).is_ok()
                {
                    let _ = crate::org::cache_org_address_record(&mut self.storage, &record);
                }
            }
            Ok(kad::GetRecordOk::FinishedWithNoAdditionalRecord { .. })
            | Err(kad::GetRecordError::NotFound { .. }) => {
                if let Some(tx) = self.pending_dht_get.remove(&query_id) {
                    let _ = tx.send(Ok(None));
                }
            }
            Err(e) => {
                if let Some(tx) = self.pending_dht_get.remove(&query_id) {
                    let _ = tx.send(Err(P2pError::Protocol(format!("dht get failed: {e}"))));
                }
            }
        }
    }

    /// 节点存在记录的 DHT 周期重发：内容直接复用 node-announce 签名报文，
    /// key = sha256("spark:node:" + peerId)（announce.rs `node_presence_record_key`）。
    pub(super) fn publish_node_presence_record(&mut self) {
        // 发布侧排序：IPv6 直连在前、电路中继在后（peer-rediscovery §4.6.3）
        let sorted = crate::p2p::peer_targets::sort_addresses(self.listen_addr_strings());
        let Some(addresses) = prepare_publish_addresses(&sorted) else {
            return;
        };
        let self_id = self.self_peer_id().to_base58();
        let Ok(announce) = sign_node_announce(&self.keypair, &self_id, &addresses, self.now())
        else {
            return;
        };
        let record = kad::Record {
            key: kad::RecordKey::new(&node_presence_record_key(&self_id)),
            value: announce_to_json(&announce).into_bytes(),
            publisher: Some(self.self_peer_id()),
            expires: Some(std::time::Instant::now() + Duration::from_secs(DHT_RECORD_TTL_SECS)),
        };
        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            let _ = kad.put_record(record, kad::Quorum::One);
        }
    }

    /// DHT 命中的外部记录走三层身份确认（development_plan 口径）：
    /// ①announce 签名与 PeerId 匹配 → ②identify 协议清单含 /spark/ 前缀 →
    /// ③node-challenge 验签通过。全过才按未验证口径（source=Exchange）入邻居池。
    fn confirm_dht_node_record(&mut self, value: &[u8]) {
        let Ok(text) = std::str::from_utf8(value) else {
            return;
        };
        // ①签名与 PeerId 匹配（DHT 记录不做新鲜度/限流判定）
        let Some(announce) = verify_announce_text(text) else {
            return;
        };
        if announce.peer_id == self.self_peer_id().to_base58() {
            return;
        }
        let Ok(peer) = announce.peer_id.parse::<PeerId>() else {
            return;
        };
        // ②identify 协议清单含 /spark/ 前缀
        let is_spark = self
            .peer_protocols
            .get(&peer)
            .is_some_and(|ps| ps.iter().any(|p| p.starts_with("/spark/")));
        if !is_spark {
            return;
        }
        // ③challenge-response（未连接无法确认，丢弃）
        if !self.swarm.is_connected(&peer) {
            return;
        }
        let nonce = challenge::generate_nonce();
        let request = challenge::build_challenge_request(&nonce, self.now());
        let request_id = self
            .swarm
            .behaviour_mut()
            .node_challenge_rr
            .send_request(&peer, request);
        self.pending_challenge_confirm
            .insert(request_id, (peer, nonce, announce));
    }

    pub(super) fn begin_challenge(&mut self, peer_id: &str, tx: oneshot::Sender<Result<bool>>) {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            let _ = tx.send(Err(P2pError::Malformed("invalid peer id".to_string())));
            return;
        };
        if !self.swarm.is_connected(&peer) {
            let _ = tx.send(Ok(false));
            return;
        }
        let nonce = challenge::generate_nonce();
        let request = challenge::build_challenge_request(&nonce, self.now());
        let request_id = self
            .swarm
            .behaviour_mut()
            .node_challenge_rr
            .send_request(&peer, request);
        self.pending_challenge.insert(request_id, (peer, nonce, tx));
    }

    pub(super) fn handle_challenge_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let now = self.now();
        // 形状非法/限流：静默不回包，请求方按超时收场（对齐 announce 静默丢弃口径）
        let Some(parsed) = challenge::parse_challenge_request(&request) else {
            return;
        };
        if self
            .challenge_limiter
            .is_rate_limited(&peer.to_base58(), now)
        {
            return;
        }
        if let Ok(text) = challenge::sign_challenge_response(&self.keypair, &parsed.nonce, now) {
            let _ = self
                .swarm
                .behaviour_mut()
                .node_challenge_rr
                .send_response(channel, text);
        }
    }

    pub(super) fn resolve_challenge(
        &mut self,
        request_id: request_response::OutboundRequestId,
        response: Option<String>,
    ) {
        // 显式 challenge 命令
        if let Some((peer, nonce, tx)) = self.pending_challenge.remove(&request_id) {
            let ok = response.as_deref().is_some_and(|text| {
                challenge::verify_challenge_response(text, &peer.to_base58(), &nonce, self.now())
                    .is_ok()
            });
            let _ = tx.send(Ok(ok));
            return;
        }
        // DHT 三层确认链路：通过即按未验证口径入邻居池
        if let Some((peer, nonce, announce)) = self.pending_challenge_confirm.remove(&request_id) {
            let ok = response.as_deref().is_some_and(|text| {
                challenge::verify_challenge_response(text, &peer.to_base58(), &nonce, self.now())
                    .is_ok()
            });
            if ok {
                let now = self.now();
                let mut store = OverlayPeerStore::new(&mut self.storage);
                let _ = store.remember(
                    &announce.peer_id,
                    &announce.addresses,
                    OverlayPeerSource::Exchange,
                    false,
                    now,
                );
            }
        }
    }
}
