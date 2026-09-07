//! 组织门面（`Kernel` 的组织 API）：组织 CRUD/成员/邀请码与组织同步编排的
//! 同步包装（委托 `org` 服务层与 `org_sync` worker 上下文）。
//! DM 邀约在 `org_invite_ops`，受邀加入与成员移除通知在 `org_join_ops`
//! （文件长度硬线拆分）。

use std::time::Duration;

use super::org_sync::OrgSyncRequest;
use super::{Kernel, KernelError, PeerOrgSyncResult, Result};
use crate::org::service::{
    CreateOrganizationInput, CreatedOrgInvite, InviteAcceptance, OrgIdentityPatch,
};
use crate::org::{OrgInvitePayload, OrganizationNodeInfo, OrganizationService, OrganizationView};
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pError, PeerNodeInfo};

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
        let node_id = self.sync_node_id();
        let record = OrganizationService::create_organization_pdsync(
            self.require_storage_mut()?,
            &input,
            &root_id,
            system_now_ms(),
            &node_id,
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
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::add_member_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            member_root_id,
            node_info,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 移除组织成员（仅 admin；移除 admin 时组织至少保留 1 名 admin，
    /// service.ts:460-498）。TS 移除路径**不推送**（历史口径：成员经 org-pull
    /// 的 `removed` 状态传播剔除，该平面已退役；本实现替代通道见下）。
    ///
    /// 阶段四A P2（L3 移除通道迁移）：落库后向被移除者发
    /// `org-member-removed` 定向通知（逐端点投递 + 退避重试，全失败入
    /// org 域 pending 队列补投——F4 既有设施，on_peer_app_ready flush；
    /// 被移除者已出成员表，flush 反查走 pending 记录内嵌的 targetPeerIds
    /// 快照）。通知失败/无寻址不阻塞移除——成员条目墓碑 + whole 双写经
    /// orgsync 收敛兜底。
    pub fn org_remove_member(
        &mut self,
        org_id: &str,
        member_root_id: &str,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        // 通知寻址须在移除前取（移除后条目墓碑化，装配视图不再可见）
        let notify_peers: Vec<PeerNodeInfo> =
            OrganizationService::get_record(self.require_storage()?, org_id)?
                .and_then(|record| {
                    record
                        .find_member(member_root_id)
                        .and_then(|m| m.node_info.clone())
                })
                .map(|set| {
                    set.iter()
                        .filter(|e| e.peer_id.is_some() || !e.addresses.is_empty())
                        .map(|e| PeerNodeInfo {
                            peer_id: e.peer_id.clone(),
                            addresses: e.addresses.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::remove_member_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            member_root_id,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        self.notify_org_member_removed(org_id, member_root_id, notify_peers);
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
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::set_org_gateways_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            gateways,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 指定数据账号（仅 admin；O1 账号角色模型：≥1 名成员，空列表 = 清除
    /// 显式指定、回落缺省全体管理员）。落库后经 org-sync worker 推送快照
    /// （与 setGateways 同模式，尽力而为）。
    pub fn org_set_data_accounts(
        &mut self,
        org_id: &str,
        data_accounts: &[String],
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::set_org_data_accounts_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            data_accounts,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 晋升/降级成员角色（仅 admin；O1：数据职责随角色自动进出——缺省数据
    /// 账号 = 全体管理员）。降级最后一个管理员拒绝（MustKeepAdmin）。
    pub fn org_set_member_role(
        &mut self,
        org_id: &str,
        member_root_id: &str,
        role: crate::org::OrganizationRole,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::set_member_role_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            member_root_id,
            role,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 更新组织名称/描述/logo（仅 admin）。落库后经 org-sync worker 向已知成员
    /// 推送快照（与 setGateways/setPublic 同模式，尽力而为）。
    pub fn org_update_info(
        &mut self,
        org_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        avatar: Option<&str>,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::update_org_info_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            name,
            description,
            avatar,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 更新自己的组织内身份字段（任何成员可改，仅改本人成员记录）。
    /// 落库后经 org-sync worker 向已知成员推送快照（与 updateOrgInfo 同模式）。
    pub fn org_update_my_identity(
        &mut self,
        org_id: &str,
        patch: &OrgIdentityPatch,
    ) -> Result<OrganizationView> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::update_my_identity_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            patch,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
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
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let record = OrganizationService::set_org_public_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            public,
            display_name,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        if let Some(tx) = &self.org_sync_tx {
            let _ = tx.send(OrgSyncRequest::PushOrg {
                org_id: record.org_id.clone(),
            });
        }
        Ok(OrganizationService::to_view(&record, &root_id))
    }

    /// 删除组织（仅 admin，service.ts:199-214）。只落库不推送（对齐 TS——
    /// 删除传播：P2 起经 org-member-removed 通知 + orgsync 墓碑收敛；P3 前
    /// 另有 org-pull `removed` 状态兜底，P3 出站停发后移除）。
    pub fn org_delete(&mut self, org_id: &str) -> Result<()> {
        let root_id = self.require_unlocked_root_id()?;
        let node_id = self.sync_node_id();
        OrganizationService::delete_organization_pdsync(
            self.require_storage_mut()?,
            org_id,
            &root_id,
            system_now_ms(),
            &node_id,
        )?;
        Ok(())
    }
    // ------------------------------------------------------------------
    // 组织同步编排 API（org_sync/；ipc/p2p.ts 对齐）
    // ------------------------------------------------------------------

    /// `p2p-sync-peer-organizations`（ipc/p2p.ts:72-93）：**阶段四A P3 起改
    /// 为 orgsync 触发**——连接目标 peer 并向其发送本机全部组织的
    /// orgsync-hello（对端回 need 拉走 diff；收敛异步完成）。legacy
    /// pull 对账（pull-list/pull-org 出站）已停发。
    ///
    /// 校验顺序与错误文案对齐 TS：p2p 未启动 → 身份锁定 → 地址缺失。
    /// 返回形状的 pull 字段恒 0（无 pull 发生）；`attempted` = 是否成功
    /// 连接并发出 hello。
    pub fn sync_peer_organizations(
        &self,
        target_peer: &OrganizationNodeInfo,
    ) -> Result<PeerOrgSyncResult> {
        self.sync_peer_organizations_with_dial_timeout(
            target_peer,
            Duration::from_secs(crate::p2p::constants::CONNECT_TIMEOUT_SECS),
        )
    }

    /// 同 `sync_peer_organizations`，但单 peer 拨号超时由调用方指定：
    /// 手动 sync-now（用户可感路径）传更短超时让不可达成员快速失败。
    pub fn sync_peer_organizations_with_dial_timeout(
        &self,
        target_peer: &OrganizationNodeInfo,
        dial_timeout: Duration,
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
        self.runtime
            .handle()
            .block_on(ctx.orgsync_round_with_peer(&peer, dial_timeout))
            .map_err(KernelError::Internal)
    }

    /// 手动执行一次组织保活 tick（候选拨号/反熵/补副本/recovery；
    /// 周期 tick 由事件泵驱动，本方法供测试与壳层诊断注入）。
    pub fn org_keepalive_once(&self) -> Result<()> {
        let ctx = self.org_sync_context().ok_or(P2pError::NotStarted)?;
        self.runtime.handle().block_on(ctx.maintain_org_tick());
        Ok(())
    }
}
