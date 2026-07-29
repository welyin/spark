//! 组织门面（`Kernel` 的组织 API）：组织 CRUD/成员/邀请码、受邀加入编排与
//! 组织同步编排的同步包装（委托 `org` 服务层与 `org_sync` worker 上下文）。

use serde_json::{Map, Value};

use super::org_sync::OrgSyncRequest;
use super::{Kernel, KernelError, PeerOrgSyncResult, Result};
use crate::collection::DocumentCollection;
use crate::org::service::{CreateOrganizationInput, CreatedOrgInvite, InviteAcceptance};
use crate::org::sync_state::{org_sync_state_key, sync_state_after_pull_synced};
use crate::org::{
    OrgInvitePayload, OrganizationNodeInfo, OrganizationService, OrganizationView,
    PluginDocSyncItem, apply_plugin_doc_sync_items, build_organization_sync_versions_default,
    sign_node_info_claim,
};
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pError, PeerNodeInfo, extract_peer_id};
use crate::storage::StorageBackend;

impl Kernel {
    // ------------------------------------------------------------------
    // 组织 API（委托 org::OrganizationService）
    // ------------------------------------------------------------------

    /// 当前用户为成员的组织视图列表（`listMine`，updatedAt 降序）。
    pub fn list_orgs(&self) -> Result<Vec<OrganizationView>> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::list_mine(
            self.require_storage()?,
            &root_id,
        )?)
    }

    /// 创建组织（需要已解锁身份）：创建者为唯一初始 admin。
    pub fn create_org(&mut self, input: CreateOrganizationInput) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::create_organization(
            self.require_storage_mut()?,
            &input,
            &root_id,
            system_now_ms(),
        )?;
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 生成组织邀请码（仅 admin；需要 p2p 运行以携带本机节点信息，
    /// 否则报"本机 P2P 节点尚未启动"）。
    pub fn create_org_invite(&self, org_id: &str) -> Result<CreatedOrgInvite> {
        let root_id = self.require_unlocked_root_id()?;
        let (peer_id, addresses) = match &self.p2p {
            Some(node) => {
                let info = self.runtime.handle().block_on(node.local_node_info())?;
                (info.peer_id, info.addresses)
            }
            None => (None, Vec::new()),
        };
        Ok(OrganizationService::create_org_invite(
            self.require_storage()?,
            org_id,
            &root_id,
            peer_id.as_deref(),
            &addresses,
            system_now_ms(),
        )?)
    }

    /// 接受邀请码的纯逻辑部分：解码校验 + 拒绝自邀；返回邀请载荷
    /// （邀请人 rootId/peerId/addresses 供壳层连接拉取，随后以
    /// [`Kernel::check_join`] 做落库确认）。
    pub fn join_by_invite(&self, code: &str) -> Result<OrgInvitePayload> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::prepare_accept_invite(
            code,
            &root_id,
            system_now_ms(),
        )?)
    }

    /// `acceptOrgInvite` 的落库确认：拉取完成后本地已有成员记录才算加入成功。
    pub fn check_join(&self, org_id: &str) -> Result<InviteAcceptance> {
        let root_id = self.require_current_root_id()?;
        Ok(OrganizationService::check_invite_accepted(
            self.require_storage()?,
            org_id,
            &root_id,
        )?)
    }

    /// 添加组织成员（仅 admin；重复添加 = 更新 nodeInfo，service.ts:216-309）。
    ///
    /// 落库后经 org-sync worker 向已知成员推送快照（尽力而为，成员离线仅
    /// 告警——对齐 service.ts `syncOrganizationToKnownMembers` 的预录模型；
    /// p2p 未启动时跳过推送，其他成员经后续反熵获得变更）。
    pub fn org_add_member(
        &mut self,
        org_id: &str,
        member_root_id: &str,
        node_info: Option<&OrganizationNodeInfo>,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::add_member(
            self.require_storage_mut()?,
            org_id,
            member_root_id,
            node_info,
            &root_id,
            system_now_ms(),
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
                actor_root_id: root_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 移除组织成员（仅 admin；移除 admin 时组织至少保留 1 名 admin，
    /// service.ts:460-498）。TS 移除路径**不推送**（成员经 org-pull 的
    /// `removed` 状态传播剔除），本方法同样只落库。
    pub fn org_remove_member(
        &mut self,
        org_id: &str,
        member_root_id: &str,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::remove_member(
            self.require_storage_mut()?,
            org_id,
            member_root_id,
            &root_id,
            system_now_ms(),
        )?;
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 指定组织网关（仅 admin；org.md §14：2–3 名本组织成员的 rootId）。
    ///
    /// 落库后经 org-sync worker 向已知成员推送快照（与 addMember 同模式，
    /// 尽力而为）；网关节点在随后的 keepalive tick 检测到自己的网关角色后
    /// 开始在组织私有 DHT 上提供成员提示（p2p-messages.md §15）。
    pub fn org_set_gateways(
        &mut self,
        org_id: &str,
        gateways: &[String],
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::set_org_gateways(
            self.require_storage_mut()?,
            org_id,
            gateways,
            &root_id,
            system_now_ms(),
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
                actor_root_id: root_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 更新组织名称/描述（仅 admin）。落库后经 org-sync worker 向已知成员
    /// 推送快照（与 setGateways/setPublic 同模式，尽力而为）。
    pub fn org_update_info(
        &mut self,
        org_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::update_org_info(
            self.require_storage_mut()?,
            org_id,
            name,
            description,
            &root_id,
            system_now_ms(),
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
                actor_root_id: root_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 开关组织公开标志（仅 admin；org.md §16），可选更新地址记录展示名。
    ///
    /// 落库后经 org-sync worker 向已知成员推送快照（与 setGateways 同模式）；
    /// 公开组织的发布动作由 keepalive tick 的
    /// `refresh_org_address_publishing` 捡起重发/新签（本机持根私钥或身为网关）。
    pub fn org_set_public(
        &mut self,
        org_id: &str,
        public: bool,
        display_name: Option<&str>,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let record = OrganizationService::set_org_public(
            self.require_storage_mut()?,
            org_id,
            public,
            display_name,
            &root_id,
            system_now_ms(),
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
                actor_root_id: root_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 删除组织（仅 admin，service.ts:199-214）。只落库不推送（对齐 TS——
    /// 删除经 org-pull 的 `removed` 状态传播）。
    pub fn org_delete(&mut self, org_id: &str) -> Result<()> {
        let root_id = self.require_unlocked_root_id()?;
        OrganizationService::delete_organization(
            self.require_storage_mut()?,
            org_id,
            &root_id,
            system_now_ms(),
        )?;
        Ok(())
    }

    /// `acceptOrgInvite` 编排（service.ts:345-374 + org-pull-sync.ts 的受邀组织段）：
    /// 解码邀请码 → 连邀请人 → org-pull-list（捎带自签 nodeInfoClaim，供管理员回填
    /// 本机地址）→ org-pull-org 拉取受邀组织 → 快照落库（含 pluginDocs 与副本记账）
    /// → 成员确认。
    ///
    /// 与 TS 的差异：TS 的 `connectAndPull` 是一次全量反熵（协调双方全部共同组织），
    /// 本方法按加入语义只拉取受邀组织；其他共同组织的协调留给组织 keepalive 编排
    /// （阶段③后续）。
    ///
    /// 需要已解锁身份（claim 签名）与运行中的 P2P（否则报 TS 文案
    /// "P2P 网络未启动，无法通过邀请码加入"）；邀请人连接失败按 TS
    /// `connectPeer` 文案报错；拉取无响应/非成员按 TS 路径降级为
    /// [`OrganizationService::check_invite_accepted`] 的"未能加入组织"错误。
    pub fn accept_invite(&mut self, code: &str) -> Result<InviteAcceptance> {
        let root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let payload = OrganizationService::prepare_accept_invite(code, &root_id, now)?;
        let inviter = PeerNodeInfo {
            peer_id: payload.inviter.peer_id.clone(),
            addresses: payload.inviter.addresses.clone(),
        };

        if self.p2p.is_none() {
            return Err(KernelError::Internal(
                "P2P 网络未启动，无法通过邀请码加入".to_string(),
            ));
        }
        let node = self.p2p.as_ref().expect("p2p checked above");
        let local = self.runtime.handle().block_on(node.local_node_info())?;

        // 自签 nodeInfoClaim（bootstrap.ts `buildSelfNodeInfoClaim`）：随首次 pull
        // 捎带，供管理员回填本机节点地址并经 gossip 扩散
        let claim = sign_node_info_claim(
            &self
                .unlocked
                .as_ref()
                .expect("unlocked checked above")
                .identity
                .signing_key,
            OrganizationNodeInfo {
                peer_id: local.peer_id.clone(),
                addresses: local.addresses.clone(),
            },
            now,
        );

        // 连接失败按 TS `connectPeer` 文案中断（service.ts 不再继续拉取）
        self.runtime
            .handle()
            .block_on(node.connect_peer(&inviter))
            .map_err(|e| {
                KernelError::Internal(format!("Failed to connect peer by provided addresses: {e}"))
            })?;

        // org-pull-list：本流程只借其捎带 claim 的副作用（管理员侧回填），
        // 响应体不消费；失败不中断（对齐 TS requestDirect 的 null 降级）
        let mut list_payload = Map::new();
        list_payload.insert("requesterRootId".to_string(), Value::from(root_id.clone()));
        if let Some(peer) = &local.peer_id {
            list_payload.insert("requesterPeerId".to_string(), Value::from(peer.clone()));
        }
        list_payload.insert("nodeInfoClaim".to_string(), serde_json::to_value(&claim)?);
        let mut list_request = Map::new();
        list_request.insert("type".to_string(), Value::from("org-pull-list"));
        list_request.insert("payload".to_string(), Value::Object(list_payload));
        let _ = self
            .runtime
            .handle()
            .block_on(node.org_pull_request(&inviter, &Value::Object(list_request).to_string()));

        // org-pull-org：拉取受邀组织；无响应/非成员均降级为末尾的成员确认错误
        let mut org_payload = Map::new();
        org_payload.insert("requesterRootId".to_string(), Value::from(root_id.clone()));
        if let Some(peer) = &local.peer_id {
            org_payload.insert("requesterPeerId".to_string(), Value::from(peer.clone()));
        }
        org_payload.insert("orgId".to_string(), Value::from(payload.org_id.clone()));
        let mut org_request = Map::new();
        org_request.insert("type".to_string(), Value::from("org-pull-org"));
        org_request.insert("payload".to_string(), Value::Object(org_payload));
        let response = self
            .runtime
            .handle()
            .block_on(node.org_pull_request(&inviter, &Value::Object(org_request).to_string()))
            .ok()
            .flatten();

        if let Some(response) = response {
            let ok = response.get("ok").and_then(Value::as_bool) == Some(true);
            let status = response.get("status").and_then(Value::as_str);
            let organization = response.get("organization").filter(|v| !v.is_null());
            if ok
                && status == Some("member")
                && let Some(organization) = organization
            {
                let now = system_now_ms();
                let merged = OrganizationService::apply_incoming_snapshot(
                    self.require_storage_mut()?,
                    organization,
                    now,
                )?;
                // pluginDocs 随快照捎带（plugin-org-sync.ts `applyPluginDocSyncItems`；
                // 集合适配器取 doc_* 登记的索引配置，未登记按无索引处理——同 host.rs）
                if let Some(docs) = response.get("pluginDocs").and_then(Value::as_array) {
                    let items: Vec<PluginDocSyncItem> = docs
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect();
                    let configs = self.collection_configs.clone();
                    apply_plugin_doc_sync_items(
                        self.require_storage_mut()?,
                        &items,
                        |domain, collection| {
                            let config = configs
                                .lock()
                                .unwrap()
                                .get(&(domain.to_string(), collection.to_string()))
                                .cloned()
                                .unwrap_or_default();
                            DocumentCollection::new(domain, collection, config)
                        },
                        now,
                    )?;
                }
                // 副本记账（org-pull-sync.ts `recordPullSyncState`）
                if let Some(peer_id) = extract_peer_id(&inviter) {
                    let versions = merged
                        .sync
                        .as_ref()
                        .map(|sync| sync.versions)
                        .unwrap_or_else(|| build_organization_sync_versions_default(&merged));
                    let state = sync_state_after_pull_synced(versions, now);
                    self.require_storage_mut()?.put(
                        &org_sync_state_key(&peer_id, &merged.org_id),
                        &state.to_json(),
                    )?;
                }
            }
        }

        self.check_join(&payload.org_id)
    }

    // ------------------------------------------------------------------
    // 组织同步编排 API（org_sync/；ipc/p2p.ts 对齐）
    // ------------------------------------------------------------------

    /// 向指定成员推送组织快照（org-share-sync.ts `syncOrganizationToMember`：
    /// stale 跳过 → 直连优先 → pubsub 五次重试等 ack → sync-state 记账）。
    ///
    /// p2p 未启动报 `p2p node not started`；全部重试失败报
    /// `Organization sync ack timeout: ...`（TS 同文案）。
    pub fn sync_org_to_member(
        &self,
        node_info: &OrganizationNodeInfo,
        target_root_id: &str,
        org_id: &str,
    ) -> Result<()> {
        let ctx = self.org_sync_context().ok_or(P2pError::NotStarted)?;
        let peer = PeerNodeInfo {
            peer_id: node_info.peer_id.clone(),
            addresses: node_info.addresses.clone(),
        };
        self.runtime
            .handle()
            .block_on(ctx.sync_org_to_member(&peer, target_root_id, org_id))
            .map_err(KernelError::Internal)
    }

    /// `p2p-sync-peer-organizations`（ipc/p2p.ts:72-93）：从指定 peer 反熵
    /// 对账全部共同组织（不带 claim，对齐该通道的调用形状）。校验顺序与
    /// 错误文案对齐 TS：p2p 未启动 → 身份锁定 → 地址缺失。
    pub fn sync_peer_organizations(
        &self,
        target_peer: &OrganizationNodeInfo,
    ) -> Result<PeerOrgSyncResult> {
        let ctx = self.org_sync_context().ok_or_else(|| {
            KernelError::Internal(
                "P2P node is not started. Start P2P before syncing organizations.".to_string(),
            )
        })?;
        self.require_unlocked_root_id()?;
        if target_peer.addresses.is_empty() {
            return Err(KernelError::Internal(
                "Target peer addresses are required".to_string(),
            ));
        }
        let peer = PeerNodeInfo {
            peer_id: target_peer.peer_id.clone(),
            addresses: target_peer.addresses.clone(),
        };
        let stats = self
            .runtime
            .handle()
            .block_on(ctx.reconcile_from_peer(&peer, false))
            .map_err(KernelError::Internal)?;
        Ok(stats.into())
    }

    /// 手动执行一次组织保活 tick（候选拨号/反熵/补副本/recovery；
    /// 周期 tick 由事件泵驱动，本方法供测试与壳层诊断注入）。
    pub fn org_keepalive_once(&self) -> Result<()> {
        let ctx = self.org_sync_context().ok_or(P2pError::NotStarted)?;
        self.runtime.handle().block_on(ctx.maintain_org_tick());
        Ok(())
    }
}
