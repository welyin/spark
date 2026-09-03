//! org-share 推送链路（org-share-sync.ts `syncOrganizationToMember` /
//! service.ts `syncOrganizationToKnownMembers`）：stale 跳过 → 直连优先 →
//! pubsub 五次重试等 ack → sync-state 记账。
//!
//! M6 收尾：管理员补副本（`replenishOrganizationReplicas`）由 keepalive tick 周期
//! 触发改为**组织写入事件驱动**——`push_org_to_known_members` 主推送完成后挂一次
//! `ensure_replicas_after_write`（纯决策在 `replica` 子模块，网络推送复用
//! `sync_org_to_member`）。tick 自此零主动外联。

use std::time::Duration;

use super::replica::{plan_replica_push_targets, replica_check_due};
use super::{
    ACK_WAIT_MS, OrgSyncContext, RETRY_INTERVALS_MS, SUBSCRIBER_POLL_MS, SUBSCRIBER_WAIT_MS,
    generate_sync_id,
};
use crate::org::sync_state::{
    should_skip_share_push, sync_state_after_share_acked, sync_state_after_share_delivered,
};
use crate::org::{
    OrganizationService, build_organization_sync_snapshot, collect_syncable_plugin_docs,
    resolve_local_versions,
};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::envelope::build_org_body;
use crate::p2p::peer_targets::{PeerNodeInfo, extract_peer_id};

impl OrgSyncContext {
    // ------------------------------------------------------------------
    // org-share 推送（org-share-sync.ts:384-484 `syncOrganizationToMember`）
    // ------------------------------------------------------------------

    /// 向单个成员推送组织快照：stale 跳过 → connectPeer → 等订阅者 →
    /// 直连优先 → pubsub 五次重试等 ack → 记账。
    pub(crate) async fn sync_org_to_member(
        &self,
        node_info: &PeerNodeInfo,
        target_root_id: &str,
        org_id: &str,
    ) -> Result<(), String> {
        let record = OrganizationService::get_record(&self.storage, org_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Organization not found".to_string())?;
        // O2b §20.8 能力探测出站（单出口）：对端该设备已证明支持 orgsync →
        // 停用 org-share 快照推送（旧链路出站对该端停用），由 orgsync 反熵
        // 承接；从未回应 orgsync 的旧端 → 回退旧快照链路（本函数继续）。
        if let Some(peer_id) = extract_peer_id(node_info)
            && self
                .orgsync_capable_member_peers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(peer_id.as_str())
        {
            log::info!(
                "[org] org-share push skipped (peer orgsync-capable): orgId={org_id}, targetRootId={target_root_id}"
            );
            return Ok(());
        }
        // 推送线形：TS `organization.sync ? organization : buildOrganizationSyncSnapshot`
        // （原始记录优先；spec §13.3）
        // ⚠️ 原始记录路径须剥除 orgRootSecret（组织根私钥密文，org.md §15 不同步出
        //    本机）；快照重建路径经 extract_metadata 已剔除
        let organization = if record.sync.is_some() {
            let mut value = serde_json::to_value(&record).map_err(|e| e.to_string())?;
            crate::org::strip_org_root_secret(&mut value);
            value
        } else {
            serde_json::to_value(build_organization_sync_snapshot(&record, &[]))
                .map_err(|e| e.to_string())?
        };
        let versions = resolve_local_versions(&record);
        let target_peer_id = extract_peer_id(node_info);

        // 推送前跳过判定（正确语义版，sync_state.rs 的"有意修复"；O1 账号
        // 口径：rootId 定键，旧 peerId 键迁移读取）
        {
            let state = self.read_sync_state(target_root_id, org_id, target_peer_id.as_deref());
            if should_skip_share_push(state.as_ref(), &versions) {
                return Ok(());
            }
        }

        let sync_id = generate_sync_id();
        self.node
            .connect_peer(node_info)
            .await
            .map_err(|e| e.to_string())?;
        self.wait_topic_subscriber(target_peer_id.as_deref(), SUBSCRIBER_WAIT_MS)
            .await;

        // F6：本函数开头已对 orgsync-capable 对端整体跳过（见上方能力探测），
        // 走到这里是**旧端**（非 orgsync-capable）→ 不停用旧通道，旧端成员
        // 仍收 pluginDocs（灰度停用只对 capable 收件人生效）。
        let plugin_docs = collect_syncable_plugin_docs(&self.storage, org_id, false)
            .map_err(|e| e.to_string())?;
        let payload = serde_json::json!({
            "targetRootId": target_root_id,
            "syncId": sync_id,
            "organization": organization,
            "pluginDocs": serde_json::to_value(&plugin_docs).map_err(|e| e.to_string())?,
            "nodeInfo": {
                "peerId": node_info.peer_id,
                "addresses": node_info.addresses,
            },
        });

        // 直连优先：ok && syncId 匹配即送达（等价收到 ack）
        if self
            .node
            .org_share_direct(node_info, payload.clone())
            .await
            .unwrap_or(false)
        {
            self.save_sync_state(
                target_root_id,
                org_id,
                sync_state_after_share_delivered(versions, self.now()),
            );
            return Ok(());
        }

        // pubsub 兜底：[0, 400, 1000, 2000, 3500]ms × 5 次，每次等 ack 1500ms
        let body = build_org_body("org-share", payload);
        for (attempt, wait_ms) in RETRY_INTERVALS_MS.iter().enumerate() {
            if *wait_ms > 0 {
                tokio::time::sleep(Duration::from_millis(*wait_ms)).await;
            }
            self.node
                .broadcast(SYNC_TOPIC, body.clone())
                .await
                .map_err(|e| e.to_string())?;
            if self.wait_ack(&sync_id, ACK_WAIT_MS).await {
                self.save_sync_state(
                    target_root_id,
                    org_id,
                    sync_state_after_share_acked(versions, self.now()),
                );
                return Ok(());
            }
            let _ = attempt;
        }
        Err(format!(
            "Organization sync ack timeout: orgId={org_id}, targetRootId={target_root_id}, syncId={sync_id}"
        ))
    }

    /// 等待 ack：先查竞态缓存（ack 先于等待到达），再注册 oneshot 等待器。
    async fn wait_ack(&self, sync_id: &str, timeout_ms: u64) -> bool {
        let rx = {
            let mut tracker = self.org_acks.lock().unwrap();
            if tracker.take_early_ack(sync_id) {
                return true;
            }
            tracker.register(sync_id)
        };
        let acked = tokio::time::timeout(Duration::from_millis(timeout_ms), rx)
            .await
            .is_ok();
        if !acked {
            self.org_acks.lock().unwrap().remove_waiter(sync_id);
        }
        acked
    }

    /// 等待对端出现在 spark-sync 订阅者列表（200ms 轮询，总窗口 5000ms；
    /// 无目标 peerId 直接返回——org-share-session.ts 同等场景不阻塞）。
    async fn wait_topic_subscriber(&self, target_peer_id: Option<&str>, budget_ms: u64) {
        let Some(target) = target_peer_id else {
            return;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_millis(budget_ms);
        loop {
            if let Ok(info) = self.node.local_node_info().await
                && info.spark_sync_subscribers.iter().any(|p| p == target)
            {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(SUBSCRIBER_POLL_MS)).await;
        }
    }

    /// `syncOrganizationToKnownMembers`（service.ts:537-571）：向组织的已知
    /// 成员（排除操作者，要求 nodeInfo 可达）逐个尽力推送；失败仅告警。
    pub(crate) async fn push_org_to_known_members(&self, org_id: &str, actor_root_id: &str) {
        let record = match OrganizationService::get_record(&self.storage, org_id) {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(e) => {
                self.warn(format!("org push: read record failed: {e}"));
                return;
            }
        };
        let recipients = OrganizationService::sync_recipients(&record, actor_root_id);
        // 端点化：遍历成员端点集，逐端点推送（多设备聚合），展开为
        // (端点, 成员 rootId) 目标列表
        let mut targets: Vec<(PeerNodeInfo, String)> = Vec::new();
        // leaf 模式 §5 单连接（H-D）：推送收件人收敛为单一目标——已有组织
        // 连接取其端点，否则取排序首候选（网关活跃集 → 数据账号 → 最近在线
        // 成员）；非 leaf 逐成员端点扇出逐字不变
        if self.node.leaf_mode() {
            if let Some(target) = self.leaf_single_push_target(&record, actor_root_id).await {
                targets.push(target);
            }
        } else {
            for member in recipients {
                let Some(set) = member.node_info.clone() else {
                    continue;
                };
                for info in set.iter() {
                    targets.push((
                        PeerNodeInfo {
                            peer_id: info.peer_id.clone(),
                            addresses: info.addresses.clone(),
                        },
                        member.root_id.clone(),
                    ));
                }
            }
        }
        let mut any_sync_ok = false;
        for (peer, member_root_id) in targets {
            if let Err(e) = self
                .sync_org_to_member(&peer, &member_root_id, org_id)
                .await
            {
                // 预录模型：成员离线不视为失败（service.ts:563-569 console.warn）
                self.warn(format!(
                    "[org] member sync deferred (peer unreachable): orgId={org_id}, targetRootId={}, error={e}",
                    member_root_id
                ));
            } else {
                any_sync_ok = true;
            }
        }
        // M6 懒连接链：本地记录端点**全不通**（无任一成员推送成功）→ 走 DHT 刷新
        // 环节（组织私有 DHT 成员提示 + 恢复 token 查询刷新端点再拨一次，失败即
        // 沉默）。仅在 org 写入推送路径（事件驱动）触发。
        if !any_sync_ok {
            self.refresh_org_endpoints_and_dial(actor_root_id).await;
        }
        // M6 事件驱动补副本：组织写入（主推送完成）后由管理员触发一次副本充足性
        // 检查——不足 K 才向未同步成员推快照（复用 `sync_org_to_member`，内部本就
        // 是发送时懒拨号）。替代被删除的 keepalive tick 周期补副本；带每 org 最小
        // 检查间隔节流（见 [`Self::ensure_replicas_after_write`]）。
        self.ensure_replicas_after_write(org_id, actor_root_id)
            .await;
    }

    /// leaf 模式 §5（H-D）：组织写入推送的单一目标——① 当前组织连接（已连接
    /// 成员端点即用）；② 否则 `leaf_ordered_org_candidates` 排序首候选（网关
    /// 活跃集 → 数据账号 → 最近在线成员）。返回（端点, 成员 rootId）供
    /// [`Self::sync_org_to_member`] 记账；无候选返回 None（推送目标为空，
    /// 触发下方 `refresh_org_endpoints_and_dial` 恢复环节）。
    async fn leaf_single_push_target(
        &self,
        record: &crate::org::types::OrganizationRecord,
        actor_root_id: &str,
    ) -> Option<(PeerNodeInfo, String)> {
        let now = self.now();
        let connected: std::collections::HashSet<String> = self
            .node
            .local_node_info()
            .await
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();
        // ① 当前组织连接：已连接成员端点即用（单连接保持语义）
        for member in &record.members {
            if member.root_id == actor_root_id {
                continue;
            }
            let Some(set) = &member.node_info else {
                continue;
            };
            for info in set.iter() {
                if info
                    .peer_id
                    .as_deref()
                    .is_some_and(|p| connected.contains(p))
                {
                    return Some((
                        PeerNodeInfo {
                            peer_id: info.peer_id.clone(),
                            addresses: info.addresses.clone(),
                        },
                        member.root_id.clone(),
                    ));
                }
            }
        }
        // ② 排序首候选，并回查其成员 rootId（记账键）
        let mut storage = self.storage.clone();
        let mut last_seen_of = |peer_id: &str| {
            crate::p2p::peer_activity::PeerActivityStore::new(&mut storage)
                .get(peer_id)
                .ok()
                .flatten()
                .map(|r| r.last_seen_at)
        };
        let first =
            super::dial::leaf_ordered_org_candidates(record, actor_root_id, now, &mut last_seen_of)
                .into_iter()
                .next()?;
        let root_id = record
            .members
            .iter()
            .find(|m| {
                m.root_id != actor_root_id
                    && m.node_info
                        .as_ref()
                        .is_some_and(|set| set.iter().any(|info| info.peer_id == first.peer_id))
            })?
            .root_id
            .clone();
        Some((first, root_id))
    }

    /// M6 事件驱动补副本（`replenishOrganizationReplicas` 收尾）：组织写入推送
    /// 完成后的副本充足性检查。本机为管理员、副本不足 K 时向未同步成员推快照
    /// （每组织最多 [`super::REPLICA_PUSH_PER_ORG`] 个），复用
    /// [`Self::sync_org_to_member`]（内部本来就是发送时懒拨号，语义不变）。
    ///
    /// 节流（防风暴）：连续多次组织写入不应每次都全量扫描 + 推送（副本不足且
    /// 目标离线时推送失败不写 sync-state，会持续判定不足）。每 org 记录最近检查
    /// 时间，距上次 < [`super::REPLICA_CHECK_MIN_INTERVAL_MS`] 直接短路跳过；
    /// 「副本已足」时同步-state 记账让下次检查近乎零成本判定 sufficient 返回
    /// （读本地记录 + sync-state，无网络）。纯决策见 [`super::replica`]。
    async fn ensure_replicas_after_write(&self, org_id: &str, actor_root_id: &str) {
        let now = self.now();
        // 节流：每 org 最小检查间隔内跳过（重复写入防风暴）
        {
            let mut last = self.replica_check.lock().unwrap_or_else(|e| e.into_inner());
            let prev = last.get(org_id).copied().unwrap_or(0);
            if !replica_check_due(prev, now) {
                return;
            }
            last.insert(org_id.to_string(), now);
        }
        let record = match OrganizationService::get_record(&self.storage, org_id) {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(e) => {
                self.warn(format!("replenish replicas: read org failed: {e}"));
                return;
            }
        };
        let targets = plan_replica_push_targets(
            &self.storage,
            &record,
            actor_root_id,
            |root_id_q, legacy_peer_id| {
                self.read_sync_state(root_id_q, &record.org_id, legacy_peer_id)
            },
            now,
        );
        for (peer, member_root_id) in targets {
            // 复用 `sync_org_to_member`：内部本来就带发送时懒拨号，语义不变。
            let _ = self
                .sync_org_to_member(&peer, &member_root_id, &record.org_id)
                .await;
        }
    }
}
