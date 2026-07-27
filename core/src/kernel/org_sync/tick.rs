//! keepalive 组织保活周期任务（p2p-node.ts `maintainOrganizationNetwork`）：
//! 网关 DHT 提供 → 组织地址发布 → 候选拨号 → 反熵拉取 → 补副本 → recovery
//! 触发（覆盖网维护已在 p2p 事件循环内完成）。

use std::collections::HashSet;

use super::{
    DIAL_BUDGET_PER_TICK, ORG_ADDRESS_REPUBLISH_INTERVAL_MS, OrgSyncContext,
    PULL_CANDIDATES_PER_TICK, REPLICA_PUSH_PER_ORG, collect_org_peer_candidates,
};
use crate::org::gateway::{OrgMemberHint, org_members_dht_key};
use crate::org::sync_state::{OrgSyncState, org_sync_state_key};
use crate::org::{OrganizationService, compute_org_sync_overview, resolve_local_versions};
use crate::p2p::constants::OVERLAY_TOPIC;
use crate::p2p::envelope::build_org_body;
use crate::p2p::keepalive::plan_organization_dials;
use crate::p2p::peer_activity::PeerActivityStore;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::StorageBackend;

impl OrgSyncContext {
    // ------------------------------------------------------------------
    // keepalive 组织保活（p2p-node.ts:379-445 `maintainOrganizationNetwork`）
    // ------------------------------------------------------------------

    /// 单个 keepalive tick 的组织层保活：网关 DHT 提供 → 候选拨号 → 反熵拉取
    /// → 补副本 → recovery 触发（覆盖网维护已在 p2p 事件循环内完成）。
    pub(crate) async fn maintain_org_tick(&self) {
        let Some(root_id) = self.root_id() else {
            return;
        };
        // 0) 网关职责：本机是某组织网关 → 在该组织私有 DHT key 上提供成员提示
        //    （§15；幂等，dht off 时静默跳过）
        self.refresh_gateway_providing(&root_id).await;
        // 0.5) 公开组织职责：持钥节点新签/重发组织地址记录，非持钥网关重发缓存
        //      记录（§16；同落点同静默口径）
        self.refresh_org_address_publishing(&root_id).await;

        let now = self.now();
        let candidates = collect_org_peer_candidates(&self.storage, &root_id);
        if candidates.is_empty() {
            // 无任何已知成员地址：全员不可达更重形态，仍尝试定向恢复
            self.maybe_run_org_recovery(true, &root_id).await;
            return;
        }

        let connected: HashSet<String> = self
            .node
            .local_node_info()
            .await
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();
        let sorted = {
            let mut storage = self.storage.clone();
            let mut store = PeerActivityStore::new(&mut storage);
            store
                .sort_candidates_by_priority(&candidates, now)
                .unwrap_or_else(|_| candidates.clone())
        };

        // 1) 候选拨号：每 tick 最多新拨 3 个（node.connect_peer 内部已记账
        //    活跃度 success/failure）
        let (to_dial, mut connected_candidates) =
            plan_organization_dials(&sorted, &connected, DIAL_BUDGET_PER_TICK);
        for candidate in to_dial {
            if self.node.connect_peer(&candidate).await.is_ok() {
                connected_candidates.push(candidate);
            }
        }

        // 2) 反熵拉取：最多 2 个已连接候选（捎带自签 claim）
        for candidate in connected_candidates.iter().take(PULL_CANDIDATES_PER_TICK) {
            if let Err(e) = self.reconcile_from_peer(candidate, true).await {
                self.warn(format!(
                    "[p2p][keepalive] pull from candidate failed: peerId={:?}, error={e}",
                    candidate.peer_id
                ));
            }
        }

        // 3) 管理员补副本
        self.replenish_replicas(&root_id).await;

        // 4) 失联 recovery
        self.maybe_run_org_recovery(connected_candidates.is_empty(), &root_id)
            .await;
    }

    /// 管理员补副本（p2p-node.ts:520-573 `replenishOrganizationReplicas`）：
    /// 副本不足 K 时向未同步成员推送快照（每组织最多 2 个）。
    async fn replenish_replicas(&self, root_id: &str) {
        let now = self.now();
        let records = match OrganizationService::read_all_organizations(&self.storage) {
            Ok(records) => records,
            Err(e) => {
                self.warn(format!("replenish replicas: read orgs failed: {e}"));
                return;
            }
        };
        for record in records {
            if !record.is_admin(root_id) {
                continue;
            }
            let versions = record
                .sync
                .as_ref()
                .map(|s| s.versions)
                .or_else(|| Some(resolve_local_versions(&record)));
            let storage = self.storage.clone();
            let overview = compute_org_sync_overview(
                &record.org_id,
                &record.members,
                Some(root_id),
                versions.as_ref(),
                |peer_id| {
                    storage
                        .get(&org_sync_state_key(peer_id, &record.org_id))
                        .ok()
                        .flatten()
                        .and_then(|raw| OrgSyncState::from_json(&raw))
                },
                now,
            );
            if overview.is_replica_sufficient() {
                continue;
            }
            let mut pushed_for_org = 0;
            for member in &overview.members {
                if pushed_for_org >= REPLICA_PUSH_PER_ORG {
                    break;
                }
                if member.is_self || member.ever_synced {
                    continue;
                }
                let node_info = record
                    .find_member(&member.root_id)
                    .and_then(|m| m.node_info.clone())
                    .filter(|info| {
                        info.peer_id
                            .as_deref()
                            .is_some_and(|p| !p.trim().is_empty())
                            || !info.addresses.is_empty()
                    });
                let Some(info) = node_info else {
                    continue;
                };
                let peer = PeerNodeInfo {
                    peer_id: info.peer_id,
                    addresses: info.addresses,
                };
                if self
                    .sync_org_to_member(&peer, &member.root_id, &record.org_id)
                    .await
                    .is_ok()
                {
                    pushed_for_org += 1;
                }
            }
        }
    }

    /// 网关职责检测（org.md §14 + p2p-messages.md §15）：本机 rootId 在某组织
    /// `gateways` 列表中且持有 orgSecret → 在 `org_members_dht_key` 上
    /// start_providing + 发布成员提示记录（只含 {peerId, addresses}）。
    /// 节点侧幂等去重，每 tick 调用一次即可；周期重发由节点挂 tick 计数完成。
    async fn refresh_gateway_providing(&self, root_id: &str) {
        let records =
            OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        let keys: Vec<String> = records
            .iter()
            .filter(|record| record.is_gateway(root_id) && record.find_member(root_id).is_some())
            .filter_map(|record| record.org_secret().map(org_members_dht_key))
            .collect();
        if keys.is_empty() {
            return;
        }
        let Ok(info) = self.node.local_node_info().await else {
            return;
        };
        let Some(peer_id) = info.peer_id else {
            return;
        };
        if info.addresses.is_empty() {
            return;
        }
        let value = OrgMemberHint {
            peer_id,
            addresses: info.addresses,
        }
        .to_record_value();
        for key in keys {
            // 相同 (key, value) 节点侧幂等空操作；dht off 等失败静默（下轮重试）
            let _ = self
                .node
                .dht_provide_record(key.as_bytes(), value.clone())
                .await;
        }
    }

    /// 公开组织的地址记录发布（org.md §16 + p2p-messages.md §16），与
    /// [`Self::refresh_gateway_providing`] 同落点：
    ///
    /// - **持钥节点**（本机为成员且 extra 中有可解密的 `orgRootSecret`）：无缓存
    ///   记录 / 展示名或网关变更 / 到达重发间隔时，以 `seq+1` 新签记录 →
    ///   `dht_put_record`（key = sha256(orgPublicKey) 字节，TTL 8h）→
    ///   gossip 扩散（spark-overlay 信封 `type='org-address'`）→ 沉淀本地缓存
    /// - **非持钥网关**（本机在 `gateways` 列表但不持根私钥）：按同一间隔重发
    ///   缓存中仍有效的记录（不重签、不 gossip）
    /// - 展示名取 `orgDisplayName` 覆盖，缺省用组织名；全部失败静默（下轮重试）
    async fn refresh_org_address_publishing(&self, root_id: &str) {
        use crate::org::org_address as oa;

        let records =
            OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        let now = self.now();
        for record in records {
            if !record.is_public || record.find_member(root_id).is_none() {
                continue;
            }
            let Some(org_address) = record.org_address.clone() else {
                continue;
            };
            let signing = oa::org_root_signing_key(&record);
            let is_gateway = record.is_gateway(root_id);
            if signing.is_none() && !is_gateway {
                continue;
            }
            let Some(dht_key) = oa::org_address_dht_key(&org_address) else {
                continue;
            };
            let display_name = record
                .display_name_override()
                .map(str::to_string)
                .or_else(|| Some(record.name.clone()))
                .filter(|name| !name.trim().is_empty());
            let last = self
                .org_address_publish
                .lock()
                .unwrap()
                .get(&org_address)
                .copied()
                .unwrap_or(0);
            let due = now - last >= ORG_ADDRESS_REPUBLISH_INTERVAL_MS;
            let cached = oa::read_cached_org_address_record(&self.storage, &org_address);

            if let Some(signing_key) = signing {
                let changed = cached.as_ref().is_none_or(|c| {
                    c.gateways != record.gateways || c.display_name != display_name
                });
                if cached.is_none() || changed || due {
                    let seq = cached.as_ref().map(|c| c.seq).unwrap_or(0) + 1;
                    let signed = oa::sign_org_address_record(
                        &signing_key,
                        &record.org_id,
                        display_name,
                        record.gateways.clone(),
                        seq,
                        now,
                        oa::ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
                    );
                    // DHT put（dht off 静默失败；失败不写时间戳，下轮重试）
                    let published = self
                        .node
                        .dht_put_record(&dht_key, signed.to_record_value())
                        .await
                        .is_ok();
                    // gossip 扩散（仅新签记录，天然低频）
                    if let Ok(value) = serde_json::to_value(&signed) {
                        let body = build_org_body(oa::ORG_ADDRESS_GOSSIP_TYPE, value);
                        let _ = self.node.broadcast(OVERLAY_TOPIC, body).await;
                    }
                    let mut storage = self.storage.clone();
                    if let Err(e) = oa::cache_org_address_record(&mut storage, &signed) {
                        self.warn(format!("org address cache save failed: {e}"));
                    }
                    if published {
                        self.org_address_publish
                            .lock()
                            .unwrap()
                            .insert(org_address, now);
                    }
                }
            } else if due {
                // 非持钥网关：重发缓存中仍有效的记录（记录自身 ttl 内的副本才值得重发）
                if let Some(cached) = cached.filter(|c| !oa::org_address_record_expired(c, now)) {
                    // 与持钥分支同口径：发布失败不写时间戳，下轮重试
                    let published = self
                        .node
                        .dht_put_record(&dht_key, cached.to_record_value())
                        .await
                        .is_ok();
                    if published {
                        self.org_address_publish
                            .lock()
                            .unwrap()
                            .insert(org_address, now);
                    }
                }
            }
        }
    }
}
