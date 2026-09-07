//! 共同体域编排（组织加入共同体的邀请/接受流，community-affairs §4.5 +
//! org-genesis §3/§4）：对齐既有个人邀请模式（`org_invite_ops` 的
//! `org_send_invite`/邀请码两段式），传输换成跨组织网关邮箱（org-mail
//! `community-org-invite` 载荷，p2p-org-mail §21）。
//!
//! 五 op：
//! - [`Kernel::community_create_invite`]：创建邀请码（纯本地，仅共同体 admin）；
//! - [`Kernel::community_send_invite`]：邀请码经 org-mail 投递到目标组织信箱
//!   （邀请人体纲寻址：目标组织地址记录 + 收件人域身份，带外通道获得，
//!   §21.1）；
//! - [`Kernel::community_accept_invite`]：接受落库——组织根私钥派生共同体域
//!   身份（[`crate::org::OrgDomainIdentity`]，org-genesis §4）→ 落库前
//!   `validate_org_join` 硬规则 → `kind = org` 成员条目（orgBinding opt-in）
//!   → 尽力回发 `community-org-join-notice`；
//! - [`Kernel::community_leave`]：退出落库——组织根私钥派生域身份核账 →
//!   `org:cleave:` 留史记录（append-only）+ 名册移除；最后一个成员组织退出
//!   后域进入空域只读档案（community-model：域不可解散，只可退出）；
//! - [`Kernel::community_list_members`]：列共同体成员（名册 kind=org 条目）。

use serde_json::Value;

use super::{Kernel, KernelError, Result};
use crate::org::community_invite::{
    build_community_invite_mail_body, build_community_join_notice, community_domain,
    decode_community_org_invite_at,
};
use crate::org::mailbox::{domain_id_of, org_mail_domain};
use crate::org::service::{
    CommunityJoinOutcome, CommunityLeaveOutcome, CommunityOrgMemberView, CreatedCommunityOrgInvite,
};
use crate::org::{OrgDomainIdentity, OrganizationService, org_root_signing_key};
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;

/// `community_accept_invite` 的返回：落库结果 + 加入通知回发是否成功。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityAcceptResult {
    /// 共同体域 orgId。
    pub community_org_id: String,
    /// 共同体名（本地记录口径）。
    pub community_org_name: String,
    /// 本组织在该共同体的域身份 id（成员条目 rootId 槽位）。
    pub member_identity: String,
    /// 是否已是成员（重复接受幂等）。
    pub already_member: bool,
    /// 加入通知（`community-org-join-notice`）是否已回发邀请人——邀请载荷
    /// 缺回信寻址（communityOrgAddress/replyDomainId）或投递失败时为 false，
    /// 不阻塞本地落库（尽力而为，同 org-invite-reply 口径）。
    pub notice_sent: bool,
}

impl Kernel {
    /// 创建共同体邀请码（仅共同体域 admin；对齐 `create_org_invite`：p2p
    /// 运行时携带本机节点信息作为邀请人寻址线索）。
    ///
    /// 载荷附带：共同体地址记录完整线形（本地缓存内未过期且验签通过才带，
    /// 私有共同体缺省）+ 邀请人收回信的域身份（`org-mail:{communityOrgId}`
    /// 个人域身份公钥，p2p-org-mail §21.1 收件人域身份的带外分发通道之一）。
    pub fn community_create_invite(
        &self,
        community_org_id: &str,
    ) -> Result<CreatedCommunityOrgInvite> {
        let root_id = self.require_unlocked_root_id()?;
        let (peer_id, addresses) = match &self.p2p {
            Some(node) => {
                let info = self.runtime.handle().block_on(node.local_node_info())?;
                (info.peer_id, info.addresses)
            }
            None => (None, Vec::new()),
        };
        let now = system_now_ms();
        let storage = self.require_storage()?;
        // 共同体地址记录（回信寻址用）：本地缓存取且验签通过才附带
        let community_org_address = OrganizationService::get_record(storage, community_org_id)?
            .and_then(|record| record.org_address)
            .and_then(|addr| crate::org::read_cached_org_address_record(storage, &addr))
            .filter(|record| crate::org::verify_org_address_record(record, now).is_ok())
            .and_then(|record| serde_json::to_string(&record).ok());
        // 邀请人收回信的域身份（org-mail:{communityOrgId}，双层身份红线：
        // rootId 不出线——信封/回信路由一律域身份）
        let seed = self
            .unlocked
            .as_ref()
            .map(|u| u.seed)
            .ok_or(KernelError::Locked)?;
        let reply_identity =
            crate::identity::derive_domain_identity(&seed, &org_mail_domain(community_org_id));
        let reply_domain_id = domain_id_of(&reply_identity.signing_key.verifying_key());
        Ok(OrganizationService::create_community_org_invite(
            storage,
            community_org_id,
            &root_id,
            crate::org::OrgInviteInviter {
                root_id: root_id.clone(),
                peer_id,
                addresses,
            },
            community_org_address,
            Some(reply_domain_id),
            now,
        )?)
    }

    /// 邀请码经 org-mail 投递到目标组织信箱（编排 = [`Kernel::org_mail_send`]：
    /// 解析目标组织地址记录 → 逐网关直连 deliver）。`to_org_address` 为目标
    /// 组织地址记录完整线形，`recipient_domain_id` 为目标组织侧收件人的
    /// `org-mail:{orgId}` 域身份公钥 b64（带外通道获得，§21.1）。
    /// 邀请码与 community_org_id 不吻合即拒绝（防串域误投）。
    #[allow(clippy::too_many_arguments)]
    pub fn community_send_invite(
        &mut self,
        community_org_id: &str,
        code: &str,
        to_org_address: &str,
        recipient_domain_id: &str,
        gateway_peer_id: Option<&str>,
        gateway_addresses: &[String],
    ) -> Result<Value> {
        self.require_unlocked_root_id()?;
        let payload = decode_community_org_invite_at(code, system_now_ms())
            .map_err(crate::org::OrgError::from)?;
        if payload.community_org_id != community_org_id {
            return Err(KernelError::Internal(
                "邀请码与共同体组织不匹配".to_string(),
            ));
        }
        let body = build_community_invite_mail_body(&payload, code);
        let hint = if gateway_peer_id.is_some() || !gateway_addresses.is_empty() {
            Some(PeerNodeInfo {
                peer_id: gateway_peer_id.map(str::to_string),
                addresses: gateway_addresses.to_vec(),
            })
        } else {
            None
        };
        self.org_mail_send(
            community_org_id,
            to_org_address,
            recipient_domain_id,
            &body,
            hint.as_ref(),
        )
    }

    /// 接受共同体邀请（本机为待加入组织的管理员，代表组织行事）：
    ///
    /// 1. 解码校验邀请码（类型/共同体标识/邀请人/24h 新鲜度）；
    /// 2. 本机须持有待加入组织的**组织根私钥**（extra 封存，org-address §15）
    ///    ——共同体域身份由根密钥对派生（`community:{communityOrgId}`，
    ///    org-genesis §4，OrgDomainIdentity 的生产使用点）；
    /// 3. 服务层落库（stub 自举 → **validate_org_join 硬规则** → kind=org
    ///    成员条目 + orgBinding opt-in）；
    /// 4. stub 自举的 pmeta.ts 压 0（零信息占位在任何裁决面皆输，同
    ///    `org_join_ops` 裁决 §10.1 口径）；
    /// 5. 尽力回发 `community-org-join-notice`（邀请载荷缺回信寻址或投递
    ///    失败仅告警，不阻塞本地落库）。
    pub fn community_accept_invite(
        &mut self,
        joiner_org_id: &str,
        code: &str,
        publish_binding: bool,
    ) -> Result<CommunityAcceptResult> {
        let root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let payload =
            decode_community_org_invite_at(code, now).map_err(crate::org::OrgError::from)?;

        // 组织根私钥 → 共同体域身份（org-genesis §4 派生的生产接线点）
        let joiner = OrganizationService::get_record(self.require_storage()?, joiner_org_id)?
            .ok_or(crate::org::OrgError::OrganizationNotFound)?;
        let root_key = org_root_signing_key(&joiner).ok_or_else(|| {
            KernelError::Internal("本机不持有该组织根私钥，无法以组织身份加入共同体".to_string())
        })?;
        let member_identity =
            OrgDomainIdentity::derive(&root_key, &community_domain(&payload.community_org_id))
                .identity();

        let node_id = self.sync_node_id();
        let outcome = {
            let __io = std::sync::Arc::clone(&self.io_lock);
            let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
            let outcome = OrganizationService::accept_community_org_invite(
                self.require_storage_mut()?,
                &payload,
                joiner_org_id,
                &root_id,
                &member_identity,
                publish_binding,
                now,
                Some(&node_id),
            )?;
            if outcome.stub_bootstrapped {
                self.press_stub_meta_ts_zero(&payload.community_org_id, &member_identity)?;
            }
            outcome
        };

        let notice_sent = self.send_community_join_notice(joiner_org_id, &payload, &outcome);
        Ok(CommunityAcceptResult {
            community_org_id: outcome.community_org_id,
            community_org_name: outcome.community_org_name,
            member_identity: outcome.member_identity,
            already_member: outcome.already_member,
            notice_sent,
        })
    }

    /// 列共同体成员（本地名册中 kind=org 的条目）。
    pub fn community_list_members(
        &self,
        community_org_id: &str,
    ) -> Result<Vec<CommunityOrgMemberView>> {
        self.require_current_root_id()?;
        Ok(OrganizationService::list_community_org_members(
            self.require_storage()?,
            community_org_id,
        )?)
    }

    /// 组织退出共同体（本机为退出组织的管理员，代表组织行事——校验口径
    /// 对齐 [`Kernel::community_accept_invite`]：本机须持有退出组织的
    /// **组织根私钥**，域身份由根密钥对派生（org-genesis §4），持钥即组织
    /// 身份；服务层再校验当前身份为该组织在任 admin）：
    ///
    /// 1. 派生本组织在该共同体的域身份 id（与加入时名册条目一致）；
    /// 2. 服务层落库：`org:cleave:` 留史记录（append-only，退出留史）+
    ///    名册移除（成员条目墓碑化）+ 本地 `member-leave` 事务审计；
    /// 3. 留史记录与名册变更经 orgsync 结构集合流动——各节点由「名册无
    ///    kind=org 成员 + 存在留史记录」确定性推导空域只读档案状态；
    ///    不发专用通知（加入通知的回信寻址不持久化，退出以名册传播为准）。
    pub fn community_leave(
        &mut self,
        community_org_id: &str,
        leaver_org_id: &str,
    ) -> Result<CommunityLeaveOutcome> {
        let root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();

        // 组织根私钥 → 共同体域身份（与加入路径同派生，org-genesis §4）
        let leaver = OrganizationService::get_record(self.require_storage()?, leaver_org_id)?
            .ok_or(crate::org::OrgError::OrganizationNotFound)?;
        let root_key = org_root_signing_key(&leaver).ok_or_else(|| {
            KernelError::Internal("本机不持有该组织根私钥，无法以组织身份退出共同体".to_string())
        })?;
        let member_identity =
            OrgDomainIdentity::derive(&root_key, &community_domain(community_org_id)).identity();

        let node_id = self.sync_node_id();
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        Ok(OrganizationService::leave_community(
            self.require_storage_mut()?,
            community_org_id,
            leaver_org_id,
            &root_id,
            &member_identity,
            now,
            Some(&node_id),
        )?)
    }

    /// stub 自举的 pmeta.ts 压 0（裁决 §10.1，同 `org_join_ops` 个人 stub
    /// 口径）：stub whole 与本组织成员条目在任何裁决面皆输，真实记录到达后
    /// 全字段秩高覆盖；vv 保留（传播/后续合并正常）。
    fn press_stub_meta_ts_zero(
        &mut self,
        community_org_id: &str,
        member_identity: &str,
    ) -> Result<()> {
        let keys = vec![
            crate::org::types::organization_key(community_org_id),
            crate::org::types::org_member_key(community_org_id, member_identity),
        ];
        let raw = self.require_storage_raw_mut()?;
        for key in keys {
            let Some(mut meta) = crate::sync::get_personal_meta(raw, &key)
                .map_err(|e| KernelError::Internal(e.to_string()))?
            else {
                continue;
            };
            meta.ts = 0;
            crate::sync::set_personal_meta(raw, &key, &meta)
                .map_err(|e| KernelError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    /// 加入通知回发（尽力而为）：邀请载荷携带回信寻址（共同体地址记录 +
    /// 邀请人 org-mail 域身份）且 p2p 运行时投递；缺寻址/未启动/投递失败
    /// 均仅告警返回 false——本地落库已完成，不回滚已加入事实。
    fn send_community_join_notice(
        &mut self,
        joiner_org_id: &str,
        payload: &crate::org::CommunityOrgInvitePayload,
        outcome: &CommunityJoinOutcome,
    ) -> bool {
        let (Some(address_record), Some(reply_domain_id)) = (
            payload.community_org_address.as_deref(),
            payload.reply_domain_id.as_deref(),
        ) else {
            return false;
        };
        if self.p2p.is_none() {
            return false;
        }
        let binding = self
            .require_storage()
            .ok()
            .and_then(|storage| {
                OrganizationService::get_record(storage, &outcome.community_org_id)
                    .ok()
                    .flatten()
            })
            .and_then(|record| {
                record
                    .find_member(&outcome.member_identity)
                    .and_then(|m| m.org_binding.clone())
            });
        let body = build_community_join_notice(
            &outcome.community_org_id,
            &outcome.member_identity,
            binding.as_ref(),
            system_now_ms(),
        );
        match self.org_mail_send(joiner_org_id, address_record, reply_domain_id, &body, None) {
            Ok(_) => true,
            Err(e) => {
                log::warn!(
                    "[COMMUNITY] join notice deliver failed | community={} err={e}",
                    outcome.community_org_id
                );
                false
            }
        }
    }
}
