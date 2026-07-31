//! gossip 入站与信封发布：spark-overlay（node-announce / org-address 记录）与
//! spark-sync（业务消息、org-share 推送与 ack）的 pubsub 处理，以及
//! `publish_envelope` / `publish_raw` 出口。

use libp2p::gossipsub;
use libp2p::PeerId;
use serde_json::{Map, Value};

use crate::p2p::announce::{announce_to_json, prepare_publish_addresses, sign_node_announce};
use crate::p2p::constants::{OVERLAY_TOPIC, PLUGIN_ANNOUNCE_TOPIC, SYNC_TOPIC};
use crate::p2p::envelope::Envelope;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::plugin_announce::{AnnounceUpsert, PluginAnnounceStore};
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::EventLoop;

impl<S: StorageBackend> EventLoop<S> {
    // ------------------------------------------------------------------
    // 广播与信封
    // ------------------------------------------------------------------

    pub(super) fn publish_envelope(&mut self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let evidence_head = self.host.evidence_head_hash();
        let mut envelope = Envelope::new(body, evidence_head, self.now());
        envelope.sign(&self.signer);
        let bytes = envelope.to_compact_json().into_bytes();
        self.publish_raw(topic, bytes)
    }

    fn publish_raw(&mut self, topic: &str, bytes: Vec<u8>) -> Result<()> {
        let ident = gossipsub::IdentTopic::new(topic);
        match self.swarm.behaviour_mut().gossipsub.publish(ident, bytes) {
            Ok(_) => Ok(()),
            // 对齐 allowPublishToZeroTopicPeers：零订阅者不算失败
            Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => Ok(()),
            Err(e) => Err(P2pError::Protocol(format!("publish failed: {e}"))),
        }
    }

    // ------------------------------------------------------------------
    // node-announce
    // ------------------------------------------------------------------

    pub(super) fn publish_announce(&mut self) -> Result<bool> {
        let Some(addresses) = prepare_publish_addresses(&self.listen_addr_strings()) else {
            return Ok(false);
        };
        let count = addresses.len();
        let announce = sign_node_announce(
            &self.keypair,
            &self.self_peer_id().to_base58(),
            &addresses,
            self.now(),
        )
        .map_err(|e| P2pError::Swarm(format!("announce sign failed: {e}")))?;
        self.publish_raw(OVERLAY_TOPIC, announce_to_json(&announce).into_bytes())?;
        self.last_announced_at = self.now();
        self.emit(P2pEvent::AnnouncePublished { addresses: count });
        Ok(true)
    }

    pub(super) fn handle_inbound_announce(&mut self, text: &str) {
        let now = self.now();
        let self_id = self.self_peer_id().to_base58();
        // 限流判定需要邻居池已存地址；先按消息里的 peerId 读取
        let known = {
            let parsed_peer = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.get("peerId")?.as_str().map(ToString::to_string));
            let mut store = OverlayPeerStore::new(&mut self.storage);
            match parsed_peer {
                Some(pid) => store
                    .get(&pid)
                    .ok()
                    .flatten()
                    .map(|r| r.addresses)
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        };
        match self
            .announce_validator
            .validate(text, &self_id, &known, now)
        {
            Ok(announce) => {
                let mut store = OverlayPeerStore::new(&mut self.storage);
                let _ = store.remember(
                    &announce.peer_id,
                    &announce.addresses,
                    OverlayPeerSource::Announce,
                    true,
                    now,
                );
                self.emit(P2pEvent::AnnounceAccepted {
                    peer_id: announce.peer_id,
                });
            }
            Err(_) => { /* 静默丢弃（TS 口径） */ }
        }
    }

    // ------------------------------------------------------------------
    // plugin-announce（插件市场广播索引，plugin-dist §8）
    // ------------------------------------------------------------------

    /// 发布声明：消息自含签名与 PoW（§8.2），不走信封（与 node-announce 同口径）。
    pub(super) fn publish_plugin_announce_raw(&mut self, json: &str) -> Result<()> {
        self.publish_raw(PLUGIN_ANNOUNCE_TOPIC, json.as_bytes().to_vec())
    }

    /// 入站声明处理（§8.6）：校验链（结构/限流/TTL/PoW/签名）→ 入本地索引
    /// （单 id 最新）→ 按传播源资历门控转发（Strict 验证模式显式上报：
    /// 合格 Accept 转发 / 资历不足 Ignore 只收不转 / 校验失败 Reject 扣分）。
    pub(super) fn handle_inbound_plugin_announce(
        &mut self,
        text: &str,
        source: PeerId,
        message_id: gossipsub::MessageId,
    ) {
        let now = self.now();
        let source_str = source.to_base58();
        match self
            .plugin_announce_validator
            .validate(text, &source_str, now)
        {
            Ok(announce) => {
                let outcome = {
                    let mut store = PluginAnnounceStore::new(&mut self.storage);
                    store.upsert(&announce, now)
                };
                match outcome {
                    Ok(AnnounceUpsert::Inserted) | Ok(AnnounceUpsert::Replaced) => {
                        self.emit(P2pEvent::PluginAnnounceReceived {
                            id: announce.id.clone(),
                            publisher: announce.publisher.clone(),
                        });
                    }
                    // Stale（同 id 已有更新）/ Duplicate：静默，不发事件
                    _ => {}
                }
                // relay 资历制（§8.6）：本机自发消息不经此路径；传播源连续接入
                // 时长不足阈值只收不转
                let connected_since = self
                    .peer_connected_since
                    .get(&source)
                    .copied()
                    .unwrap_or(now);
                let acceptance = if now.saturating_sub(connected_since)
                    >= self.plugin_announce_tenure_ms
                {
                    gossipsub::MessageAcceptance::Accept
                } else {
                    gossipsub::MessageAcceptance::Ignore
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .report_message_validation_result(&message_id, &source, acceptance);
            }
            Err(_) => {
                // 校验失败：Reject（gossipsub peer scoring 扣传播源分数）
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .report_message_validation_result(
                        &message_id,
                        &source,
                        gossipsub::MessageAcceptance::Reject,
                    );
            }
        }
    }

    // ------------------------------------------------------------------
    // 组织地址记录 gossip 入站（p2p-messages.md §16）
    // ------------------------------------------------------------------

    /// spark-overlay 信封 `type='org-address'` 的入站校验链：
    /// 信封规则（§3.4：携带签名则必须验签通过）→ 记录五步校验链 →
    /// seq/publishedAt 冲突裁决 → 沉淀本地缓存（`p2p:org-address:` 前缀）。
    /// 任一失败静默丢弃（与 node-announce 同口径）。
    pub(super) fn handle_inbound_org_address(&mut self, text: &str) {
        let verified = match crate::p2p::envelope::parse_and_verify_envelope(text) {
            Ok(v) => v,
            Err(_) => return, // 信封验签失败/畸形：静默丢弃（§3.4）
        };
        if verified.msg_type != crate::org::ORG_ADDRESS_GOSSIP_TYPE {
            return;
        }
        let Some(payload) = verified.map.get("payload") else {
            return;
        };
        let Ok(record) = serde_json::from_value::<crate::org::OrgAddressRecord>(payload.clone())
        else {
            return;
        };
        if !crate::org::verify_org_address_record(&record, self.now()).is_ok() {
            return;
        }
        // 冲突裁决在 cache_org_address_record 内（seq 最大，同 seq 取 publishedAt 最新）
        let _ = crate::org::cache_org_address_record(&mut self.storage, &record);
    }

    // ------------------------------------------------------------------
    // pubsub 业务消息（spark-sync）
    // ------------------------------------------------------------------

    pub(super) fn handle_sync_message(&mut self, text: &str) {
        let verified = match crate::p2p::envelope::parse_and_verify_envelope(text) {
            Ok(v) => v,
            Err(P2pError::SignatureInvalid) => {
                self.emit(P2pEvent::MessageDropped {
                    reason: "signature invalid".to_string(),
                });
                return;
            }
            Err(_) => {
                self.emit(P2pEvent::MessageDropped {
                    reason: "invalid json".to_string(),
                });
                return;
            }
        };
        if crate::p2p::envelope::is_signature_mandatory_type(&verified.msg_type) && !verified.signed
        {
            self.emit(P2pEvent::MessageDropped {
                reason: format!("unsigned data message: {}", verified.msg_type),
            });
            return;
        }

        let map = &verified.map;
        let get_str = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .map(ToString::to_string)
        };
        match verified.msg_type.as_str() {
            "update" | "delete" => {
                let (Some(domain), Some(collection), Some(id)) =
                    (get_str("domain"), get_str("collection"), get_str("id"))
                else {
                    return;
                };
                let Some(meta) = map.get("meta").cloned() else {
                    return;
                };
                if meta.is_null() {
                    return;
                }
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                let schema = map.get("schema").cloned();
                if let Err(e) =
                    self.host
                        .apply_remote_update(&domain, &collection, &id, payload, meta, schema)
                {
                    self.emit(P2pEvent::Warning(format!(
                        "apply remote update failed: {e}"
                    )));
                    return;
                }
                // 存证头不一致仅告警不丢弃
                if let Some(remote_head) = get_str("evidenceHeadHash")
                    && !remote_head.is_empty()
                    && self.host.evidence_head_hash().as_deref() != Some(remote_head.as_str())
                {
                    self.emit(P2pEvent::Warning(
                        "evidence head mismatch, peer may have diverged".to_string(),
                    ));
                }
                self.emit(P2pEvent::SyncMessageApplied {
                    msg_type: verified.msg_type.clone(),
                    domain,
                });
            }
            "history-response" => {
                let (Some(domain), Some(collection), Some(id)) =
                    (get_str("domain"), get_str("collection"), get_str("id"))
                else {
                    return;
                };
                let Some(meta) = map.get("meta").cloned().filter(|m| !m.is_null()) else {
                    return;
                };
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                let schema = map.get("schema").cloned();
                if let Err(e) =
                    self.host
                        .apply_remote_update(&domain, &collection, &id, payload, meta, schema)
                {
                    self.emit(P2pEvent::Warning(format!(
                        "apply history-response failed: {e}"
                    )));
                    return;
                }
                self.emit(P2pEvent::SyncMessageApplied {
                    msg_type: verified.msg_type,
                    domain,
                });
            }
            "org-share" => {
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                match self.host.apply_incoming_org_share(payload, "pubsub") {
                    Ok(Some(ack)) => {
                        let org_id = ack.org_id.clone();
                        let sync_id = ack.sync_id.clone();
                        self.emit(P2pEvent::OrgShareAccepted {
                            org_id,
                            sync_id: sync_id.clone(),
                            source: "pubsub",
                        });
                        if let Some(sync_id) = &ack.sync_id {
                            let ack_payload = serde_json::json!({
                                "syncId": sync_id,
                                "orgId": ack.org_id,
                                "targetRootId": ack.target_root_id,
                                "receiverRootId": ack.receiver_root_id,
                            });
                            let body =
                                crate::p2p::envelope::build_org_body("org-share-ack", ack_payload);
                            if let Err(e) = self.publish_envelope(SYNC_TOPIC, body) {
                                self.emit(P2pEvent::Warning(format!(
                                    "org-share-ack broadcast failed: {e}"
                                )));
                            }
                        }
                    }
                    Ok(None) => { /* 未接受，静默 */ }
                    Err(e) => self.emit(P2pEvent::Warning(format!("org-share apply failed: {e}"))),
                }
            }
            "org-share-ack" => {
                let payload = map.get("payload").cloned().unwrap_or(Value::Null);
                if payload.get("syncId").and_then(Value::as_str).is_some() {
                    self.host.on_org_share_ack(payload);
                }
            }
            _ => { /* 插件自定义等：不强制签名，p2p 不处理 */ }
        }
    }
}
