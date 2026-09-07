//! gossip 入站与信封发布：spark-overlay（node-announce / org-address 记录）与
//! spark-sync（业务消息）的 pubsub 处理，以及
//! `publish_envelope` / `publish_raw` 出口。

use libp2p::PeerId;
use libp2p::gossipsub;
use serde_json::{Map, Value};

use crate::p2p::announce::{announce_to_json, prepare_publish_addresses, sign_node_announce};
use crate::p2p::constants::{OVERLAY_TOPIC, PLUGIN_ANNOUNCE_TOPIC};
use crate::p2p::envelope::Envelope;
use crate::p2p::overlay_store::{OverlayPeerSource, OverlayPeerStore};
use crate::p2p::plugin_announce::{AnnounceUpsert, PluginAnnounceReject, PluginAnnounceStore};
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
        // IdentTopic 构造含 topic 字符串哈希：topic 集合为协议常量，缓存复用
        let ident = self
            .topic_cache
            .entry(topic.to_string())
            .or_insert_with(|| gossipsub::IdentTopic::new(topic))
            .clone();
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
        // leaf 模式 §3：node-announce 是发布服务，叶子关闭（叶子上没有要广播的对象）
        if self.leaf_mode {
            return Ok(false);
        }
        // S7 兜底：剔除黑名单命中的污染地址再发布（根治 S2 已止源头，此为防旧污染残留）
        let strings = self.listen_addr_strings();
        let Some(addresses) = prepare_publish_addresses(&self.drop_blacklisted(strings)) else {
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
                let self_addrs = self.self_listen_addr_set();
                let mut store = OverlayPeerStore::new(&mut self.storage);
                let _ = store.remember(
                    &announce.peer_id,
                    &announce.addresses,
                    OverlayPeerSource::Announce,
                    true,
                    now,
                    Some(&self_id),
                    &self_addrs,
                );
                // M9 valid 证据：announce 验签通过说明这些地址「活着」
                let _ = store.mark_addrs_valid(&announce.peer_id, &announce.addresses, now);
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
        // leaf 模式 §3：发布切断（订阅保留只消费市场索引，见 behaviour.rs
        // 订阅门控注释——待产品确认项）
        if self.leaf_mode {
            return Ok(());
        }
        self.publish_raw(PLUGIN_ANNOUNCE_TOPIC, json.as_bytes().to_vec())
    }

    /// 入站声明处理（§8.6）：校验链（结构/限流/TTL/PoW/签名）→ 入本地索引
    /// （单 id 最新）→ 按传播源资历门控转发（Strict 验证模式显式上报：
    /// 合格 Accept 转发 / 资历不足 Ignore 只收不转 / 限流 Ignore 不扣分 /
    /// 其余校验失败 Reject 扣分）。
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
                    // 桥（mobile-leaf-mode）：gossip 接收路径是裸 sled 句柄，
                    // 显式设置 nodeId 使 save 受管（带 pmeta）——公告经 pdsync
                    // `mkt:ann` 类目同步给叶子（PC 桥角色，无需新增组件）
                    let node_id = self.self_peer_id().to_base58();
                    let mut store =
                        PluginAnnounceStore::new(&mut self.storage).with_node_id(&node_id);
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
                let acceptance =
                    if now.saturating_sub(connected_since) >= self.plugin_announce_tenure_ms {
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
            Err(reject) => {
                // 校验失败上报（§8.6）：限流报 Ignore（诚实中继在高频转发时可能
                // 撞限流，Reject 扣分会误伤）；其余校验失败 Reject 扣传播源分数
                let acceptance = match reject {
                    PluginAnnounceReject::RateLimited => gossipsub::MessageAcceptance::Ignore,
                    _ => gossipsub::MessageAcceptance::Reject,
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .report_message_validation_result(&message_id, &source, acceptance);
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
    // 议题元数据公告 gossip 入站（affair-metadata §2/§4，C4）
    // ------------------------------------------------------------------

    /// spark-affair-meta 信封 `type='affair-meta'` 的入站校验链：
    /// 信封规则（§3.4：携带签名则必须验签通过；本类型不强制签名）→ 公告线形
    /// parse（metaV=1、affairId == 信封 id、metaSeq/updatedAt 存在）→ 交宿主
    /// 回调（不落业务库，暂存区归 C10/宿主）。`type='org-card'` 信封分流到
    /// [`Self::handle_inbound_org_card`]（affair-metadata §6 组织公开名片
    /// 收录，C11）；`type='indexer-card'` 分流到
    /// [`Self::handle_inbound_indexer_card`]（§7 indexer 目录名片收录）。
    /// 其余类型静默丢弃（与 node-announce 同口径）。
    pub(super) fn handle_inbound_affair_meta(&mut self, text: &str) {
        let verified = match crate::p2p::envelope::parse_and_verify_envelope(text) {
            Ok(v) => v,
            Err(_) => return, // 信封验签失败/畸形：静默丢弃（§3.4）
        };
        if verified.msg_type == "org-card" {
            self.handle_inbound_org_card(&verified);
            return;
        }
        if verified.msg_type == crate::index::directory::INDEXER_CARD_TYPE {
            self.handle_inbound_indexer_card(&verified);
            return;
        }
        if verified.msg_type != "affair-meta" {
            return;
        }
        let Some(affair_id) = verified.map.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(payload) = verified.map.get("payload").cloned() else {
            return;
        };
        // 线形 parse（affair-metadata §3）：字段缺失/类型错即丢弃
        if payload.get("metaV").and_then(Value::as_u64) != Some(1) {
            return;
        }
        if payload.get("affairId").and_then(Value::as_str) != Some(affair_id) {
            return; // affairId 必须等于信封 id
        }
        if !crate::affair::is_valid_identity_id(affair_id) {
            return;
        }
        if payload.get("metaSeq").and_then(Value::as_u64).is_none() {
            return;
        }
        if payload.get("updatedAt").and_then(Value::as_i64).is_none() {
            return;
        }
        self.host.on_affair_meta(payload);
    }

    /// 组织公开名片收录（affair-metadata §6，C11）：spark-affair-meta 主题
    /// `type='org-card'` 信封，domain='affair'、id=orgAddress、payload =
    /// 组织地址记录全文（org-address §16 线形）。
    ///
    /// 收录点校验链：信封 id 必须等于记录 orgAddress → §16.3 五步校验链
    /// （结构 → ttl 窗口 → orgId 格式 → 自认证闭环 → 验签，记录本就自认证
    /// 签名，无需新机制）→ seq/publishedAt 冲突裁决后沉淀本地缓存
    /// （`p2p:org-address:` 前缀，与 spark-overlay `org-address` 入站同径
    /// 同库）。查询路径 = kernel `resolve_org_address` / `search_known_orgs`
    /// （读同一缓存，org.md §16.4），收录即接通。任一失败静默丢弃（与
    /// node-announce 同口径）。
    fn handle_inbound_org_card(&mut self, verified: &crate::p2p::envelope::VerifiedEnvelope) {
        let Some(id) = verified.map.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(payload) = verified.map.get("payload") else {
            return;
        };
        let Ok(record) = serde_json::from_value::<crate::org::OrgAddressRecord>(payload.clone())
        else {
            return;
        };
        if record.org_address != id {
            return; // 信封 id 必须等于 orgAddress（affair-metadata §6）
        }
        if !crate::org::verify_org_address_record(&record, self.now()).is_ok() {
            return;
        }
        // 冲突裁决在 cache_org_address_record 内（seq 最大，同 seq 取 publishedAt 最新）
        let _ = crate::org::cache_org_address_record(&mut self.storage, &record);
    }

    // ------------------------------------------------------------------
    // indexer 目录名片（affair-metadata §7 目录面，indexer-card）
    // ------------------------------------------------------------------

    /// 发布本节点 indexer 名片：payload 以 libp2p 节点私钥签名（绑定 peerId，
    /// node-announce 同款自证口径），装信封（type='indexer-card'、
    /// domain='affair'、id=peerId）经 `spark-affair-meta` 洪泛；随后自卡
    /// 落本地目录（gossipsub 不回灌自发消息，本机目录需显式补记）。
    /// 返回名片 payload（调用方确认/测试断言用）。
    pub(super) fn publish_indexer_card_now(
        &mut self,
        coverage: &crate::index::directory::IndexCoverage,
    ) -> Result<Value> {
        // leaf 模式 §3：叶子不为他人服务，发布切断（tick 路径已门控，此为
        // 命令直达路径的双保险）
        if self.leaf_mode {
            return Err(P2pError::Protocol(
                "indexer card publish disabled in leaf mode".to_string(),
            ));
        }
        let peer_id = self.self_peer_id().to_base58();
        let now = self.now();
        let signing_payload =
            crate::index::directory::build_card_signing_payload(&peer_id, coverage, now);
        let signature = self
            .keypair
            .sign(signing_payload.as_bytes())
            .map_err(|e| P2pError::Swarm(format!("indexer card sign failed: {e}")))?;
        let card = crate::index::directory::build_card(&peer_id, coverage, now, &signature);
        let payload = crate::index::directory::card_to_value(&card);
        let mut body = Map::new();
        body.insert(
            "type".to_string(),
            Value::String(crate::index::directory::INDEXER_CARD_TYPE.to_string()),
        );
        body.insert("domain".to_string(), Value::String("affair".to_string()));
        body.insert("id".to_string(), Value::String(peer_id));
        body.insert("payload".to_string(), payload.clone());
        self.publish_envelope(crate::p2p::constants::AFFAIR_META_TOPIC, body)?;
        // 自卡落本地目录（与入站收录同径同库）
        let _ = crate::index::directory::upsert_card(&mut self.storage, &card, now);
        Ok(payload)
    }

    /// `type='indexer-card'` 信封收录（§7）：信封 id 必须等于卡内 peerId →
    /// 名片校验链（结构 + 覆盖线形 + peerId 内嵌公钥验签）→ 目录 upsert
    /// （updatedAt 裁决 + 本地 TTL）。任一失败静默丢弃（与 node-announce
    /// 同口径）；本机 peerId 的名片不重复收录（发布路径已落账）。
    fn handle_inbound_indexer_card(&mut self, verified: &crate::p2p::envelope::VerifiedEnvelope) {
        let Some(id) = verified.map.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(payload) = verified.map.get("payload") else {
            return;
        };
        let Ok(card) = crate::index::directory::parse_card(payload) else {
            return;
        };
        if card.peer_id != id {
            return; // 信封 id 必须等于卡内 peerId
        }
        if card.peer_id == self.self_peer_id().to_base58() {
            return;
        }
        let now = self.now();
        let _ = crate::index::directory::upsert_card(&mut self.storage, &card, now);
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
            _ => { /* 插件自定义等：不强制签名，p2p 不处理 */ }
        }
    }
}
