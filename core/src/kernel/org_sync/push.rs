//! org-share 推送链路（org-share-sync.ts `syncOrganizationToMember` /
//! service.ts `syncOrganizationToKnownMembers`）：stale 跳过 → 直连优先 →
//! pubsub 五次重试等 ack → sync-state 记账。

use std::time::Duration;

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

        // 推送前跳过判定（正确语义版，sync_state.rs 的"有意修复"）
        if let Some(peer_id) = &target_peer_id {
            let state = self.read_sync_state(peer_id, org_id);
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

        let plugin_docs =
            collect_syncable_plugin_docs(&self.storage, org_id).map_err(|e| e.to_string())?;
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
            if let Some(peer_id) = &target_peer_id {
                self.save_sync_state(
                    peer_id,
                    org_id,
                    sync_state_after_share_delivered(versions, self.now()),
                );
            }
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
                if let Some(peer_id) = &target_peer_id {
                    self.save_sync_state(
                        peer_id,
                        org_id,
                        sync_state_after_share_acked(versions, self.now()),
                    );
                }
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
        for member in recipients {
            let Some(info) = member.node_info.clone() else {
                continue;
            };
            let peer = PeerNodeInfo {
                peer_id: info.peer_id,
                addresses: info.addresses,
            };
            if let Err(e) = self
                .sync_org_to_member(&peer, &member.root_id, org_id)
                .await
            {
                // 预录模型：成员离线不视为失败（service.ts:563-569 console.warn）
                self.warn(format!(
                    "[org] member sync deferred (peer unreachable): orgId={org_id}, targetRootId={}, error={e}",
                    member.root_id
                ));
            }
        }
    }
}
