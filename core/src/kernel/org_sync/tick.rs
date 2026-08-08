//! keepalive 组织保活周期任务（p2p-node.ts `maintainOrganizationNetwork`）：
//! 网关 DHT 提供 → 组织地址发布 → 候选拨号 → 反熵拉取 → 补副本 → recovery
//! 触发（覆盖网维护已在 p2p 事件循环内完成）。

use std::collections::HashSet;

use super::{
    DIAL_BUDGET_PER_TICK, ORG_ADDRESS_REPUBLISH_INTERVAL_MS, OrgSyncContext,
    PULL_CANDIDATES_PER_TICK, REPLICA_PUSH_PER_ORG, collect_org_peer_candidates,
};
use crate::contact::ContactService;
use crate::org::gateway::{OrgMemberHint, org_members_dht_key};
use crate::org::sync_state::{OrgSyncState, org_sync_state_key};
use crate::org::{OrganizationService, compute_org_sync_overview, resolve_local_versions};
use crate::p2p::constants::OVERLAY_TOPIC;
use crate::p2p::envelope::build_org_body;
use crate::p2p::keepalive::plan_organization_dials;
use crate::p2p::node::LocalP2PNodeInfo;
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
        // 本机节点信息一次取用：候选收集（排除本机 peerId）与连接快照共用
        let local_info = self.node.local_node_info().await.ok();
        // 0.6) 自设备保活：配对设备未连接时补拨（独立于组织候选，无组织
        //      成员时也要维持自设备链路——自会话/资料/设备清单同步全依赖它）
        self.maintain_self_device_link(&root_id, local_info.as_ref())
            .await;
        let candidates = collect_org_peer_candidates(
            &self.storage,
            &root_id,
            local_info.as_ref().and_then(|i| i.peer_id.as_deref()),
        );
        if candidates.is_empty() {
            // 无任何已知成员地址：全员不可达更重形态，仍尝试定向恢复
            self.maybe_run_org_recovery(true, &root_id).await;
            return;
        }

        let connected: HashSet<String> = local_info
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

    /// 自设备保活：配对设备（自 FriendRecord.peer）未连接时补拨，每 tick 一次；
    /// 断→连跳变时向对端重发 device-sync + profile-sync 快照。
    ///
    /// 登录后的一次性广播在对端离线时静默失败后不再重试；挂 keepalive tick
    /// 周期补拨，使两端错峰上线也能自动会合。仅打通连接不够——两端启动时的
    /// 快照广播早已互相错过，重连成功时必须重发快照才能收敛（对端收
    /// device-sync 恒回发、profile-sync 按 LWW 裁决回发，双向齐全）。
    async fn maintain_self_device_link(&self, root_id: &str, local_info: Option<&LocalP2PNodeInfo>) {
        let mut storage = self.storage.clone();
        // 双来源解析配对设备：FriendRecord.peer 优先（带地址），DeviceRecord
        // 兜底（仅 peerId，已连接时 dm_direct 短路；未连接时无地址无法补拨，
        // 但候选收集里的 FriendRecord 来源仍可提供地址）
        let (peer_id, addresses) = {
            let from_friend = ContactService::get_friend(&mut storage, root_id)
                .ok()
                .flatten()
                .and_then(|f| f.peer)
                .filter(|p| !p.peer_id.is_empty());
            match from_friend {
                Some(p) => (p.peer_id, p.addresses),
                None => {
                    let my_peer = local_info.and_then(|i| i.peer_id.as_deref());
                    let device = crate::device::DeviceService::list(&storage)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|r| Some(r.peer_id.as_str()) != my_peer);
                    match device {
                        Some(d) => (d.peer_id, Vec::new()),
                        None => return,
                    }
                }
            }
        };
        if peer_id.is_empty() {
            return;
        }
        let connected = local_info
            .map(|info| info.connected_peers.iter().any(|p| p == &peer_id))
            .unwrap_or(false);

        // 状态机决策在独立作用域内完成（MutexGuard 不可跨 await）
        enum Action {
            StayConnected,
            Resync,
            Dial,
        }
        let action = {
            let mut last = self.self_device_link.lock().unwrap_or_else(|e| e.into_inner());
            if connected {
                if last.as_deref() == Some(peer_id.as_str()) {
                    Action::StayConnected
                } else {
                    // 断→连跳变（含启动后首次观察到连接）
                    *last = Some(peer_id.clone());
                    Action::Resync
                }
            } else {
                *last = None;
                Action::Dial
            }
        };
        match action {
            Action::StayConnected => {}
            // 重发快照补齐错过的变更。幂等（LWW 裁决），启动一次性广播可能
            // 造成的少量重复投递可接受。
            Action::Resync => self.send_self_snapshots(root_id, &peer_id).await,
            Action::Dial => {
                let target = PeerNodeInfo {
                    peer_id: Some(peer_id),
                    addresses,
                };
                // 短超时：后台保活不可长阻塞组织保活串行队列
                let _ = self
                    .node
                    .connect_peer_with_timeout(&target, std::time::Duration::from_secs(5))
                    .await;
            }
        }
    }

    /// 向已会合的自设备重发本机 device-sync + profile-sync + contact-sync
    /// 快照（断→连跳变触发；装配口径同 `Kernel::broadcast_device_sync` /
    /// `host.rs::spawn_profile_sync_reply` / `Kernel::broadcast_contact_sync`，
    /// 失败静默）。
    async fn send_self_snapshots(&self, root_id: &str, peer_id: &str) {
        let signing_key = self.signing_key.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(signing_key) = signing_key else {
            return;
        };
        let my_peer_id = self.node.peer_id().to_string();
        let target = PeerNodeInfo {
            peer_id: Some(peer_id.to_string()),
            addresses: Vec::new(), // 已连接：dm_direct 短路直发
        };
        let now = self.now();
        // 1) device-sync：本机设备记录（读取既有记录，不 upsert——避免每 tick
        //    刷新 updatedAt 推高 LWW 水位）
        if let Ok(Some(record)) =
            crate::device::DeviceService::get(&self.storage, &my_peer_id)
        {
            if let Ok(body) = serde_json::to_value(&record) {
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_DEVICE_SYNC,
                    root_id,
                    root_id,
                    now,
                    body,
                    &signing_key,
                );
                let _ = self.node.dm_direct(&target, envelope).await;
            }
        }
        // 2) profile-sync：身份文件全量资料快照（昵称为空回退 rootId 前 8 位）
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(file) = crate::identity::IdentityFile::from_json(&raw) {
                let nickname = file.nickname.clone().unwrap_or_default();
                let nickname = if nickname.trim().is_empty() {
                    root_id.chars().take(8).collect::<String>()
                } else {
                    nickname
                };
                let body = serde_json::json!({
                    "nickname": nickname,
                    "avatar": file.avatar,
                    "gender": file.gender,
                    "region": file.region,
                    "signature": file.signature,
                    "updatedAt": file.updated_at,
                });
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_PROFILE_SYNC,
                    root_id,
                    root_id,
                    now,
                    body,
                    &signing_key,
                );
                let _ = self.node.dm_direct(&target, envelope).await;
            }
        }
        // 3) contact-sync：通讯录全量快照（朋友/申请/标签/分组/拉黑；
        //    LWW 幂等，对端按时间戳裁决，重复投递无害）
        if let Ok(body) = crate::contact::build_contact_sync_snapshot(&self.storage, root_id) {
            let envelope = crate::kernel::dm_envelope::build_envelope(
                crate::kernel::dm_envelope::KIND_CONTACT_SYNC,
                root_id,
                root_id,
                now,
                body,
                &signing_key,
            );
            let _ = self.node.dm_direct(&target, envelope).await;
        }
        // 4) conv-sync：会话元数据快照（direct 会话外壳 + 置顶/免打扰/草稿）
        if let Ok(body) = crate::message::build_conv_sync_snapshot(&self.storage) {
            let envelope = crate::kernel::dm_envelope::build_envelope(
                crate::kernel::dm_envelope::KIND_CONV_SYNC,
                root_id,
                root_id,
                now,
                body,
                &signing_key,
            );
            let _ = self.node.dm_direct(&target, envelope).await;
        }
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
