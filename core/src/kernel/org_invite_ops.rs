//! 组织邀请（DM 邀约 + 记录持久化，从 `org_ops` 拆出，文件长度硬线）：
//! `org_send_invite`（出站记录 + org-invite 信封尽力投递）、
//! `org_respond_invite`（接受=走 join 编排后标 accepted + org-invite-reply
//! 回发）与邀请记录查询。

use serde_json::Value;

use super::dm_delivery::DM_RETRY_DELAYS;
use super::dm_envelope::{KIND_ORG_INVITE, KIND_ORG_INVITE_REPLY};
use super::{Kernel, KernelError, Result};
use crate::contact::ContactService;
use crate::org::{
    OrgInviteDirection, OrgInviteRecord, OrgInviteStatus, OrganizationService, decode_org_invite_at,
};
use crate::p2p::node::system_now_ms;
use crate::p2p::PeerNodeInfo;

impl Kernel {
    // ------------------------------------------------------------------
    // 组织邀请（DM 邀约 + 记录持久化）
    // ------------------------------------------------------------------

    /// 经 DM 发出组织邀请（仅 admin）：生成邀请码（复用
    /// [`OrganizationService::create_org_invite`] 的权限检查与编码）→ 落出站
    /// 邀请记录（`org:inv:out:{orgId}:{target}`；同一对只留一条，重复邀请
    /// 原地更新并回到 pending）→ 构造 `org-invite` 信封尽力投递（spawn
    /// 异步任务，不阻塞命令）。目标寻址：显式 targetPeerId/targetAddresses →
    /// 组织成员 nodeInfo（前端 addMember 预录）→ 朋友记录；均无则报错。
    /// 返回落库后的记录（status=pending；对方回应经入站 `org-invite-reply`
    /// 落库并以 `P2pEvent::OrgInviteUpdated` 事件回传）。
    pub fn org_send_invite(
        &mut self,
        org_id: &str,
        target_root_id: &str,
        target_peer_id: Option<&str>,
        target_addresses: &[String],
        target_nickname: Option<&str>,
    ) -> Result<OrgInviteRecord> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let root_id = self.require_unlocked_root_id()?;
        if target_root_id == root_id {
            return Err(KernelError::Internal("不能邀请自己".to_string()));
        }
        let now = system_now_ms();
        // 仅 admin + 邀请码编码（p2p 未启动报"本机 P2P 节点尚未启动"）
        let created = self.create_org_invite(org_id)?;
        let org = OrganizationService::get_record(self.require_storage()?, org_id)?
            .expect("create_org_invite 已校验组织存在");
        let target = self.resolve_invite_target_peer(
            org_id,
            target_root_id,
            target_peer_id,
            target_addresses,
        )?;

        // 邀请 id：`inv-{ms}-{count}` 风格（照 create_outgoing_request），撞 id 递增避让
        let mut count =
            OrganizationService::list_all_invite_records(self.require_storage()?)?.len();
        let mut invite_id = format!("inv-{now}-{count}");
        while OrganizationService::find_invite_by_id(self.require_storage()?, &invite_id)?.is_some()
        {
            count += 1;
            invite_id = format!("inv-{now}-{count}");
        }

        let nickname = target_nickname
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        let record = match OrganizationService::get_outgoing_invite(
            self.require_storage()?,
            org_id,
            target_root_id,
        )? {
            // 重复邀请：原地更新（新邀请码/id、回 pending），保留首次 createdAt
            Some(mut r) => {
                r.id = invite_id;
                r.org_name = org.name.clone();
                r.org_avatar = (!org.avatar.is_empty()).then(|| org.avatar.clone());
                if let Some(n) = nickname {
                    r.peer_nickname = n;
                }
                r.status = OrgInviteStatus::Pending;
                r.updated_at = now;
                r
            }
            None => OrgInviteRecord {
                id: invite_id,
                org_id: org_id.to_string(),
                org_name: org.name.clone(),
                org_avatar: (!org.avatar.is_empty()).then(|| org.avatar.clone()),
                peer_root_id: target_root_id.to_string(),
                peer_nickname: nickname.unwrap_or_else(|| "待加入成员".to_string()),
                direction: OrgInviteDirection::Outgoing,
                status: OrgInviteStatus::Pending,
                invite_code: None,
                created_at: now,
                updated_at: now,
            },
        };
        // P5：邀请记录写 pmeta（自设备 pdsync 同步）+ batch3 §2 管理面投影
        // 双写（org:invitations@v1 / org:invpub:，同一 batch 原子）
        crate::org::service::put_invite_record_with_projection(
            self.require_storage_mut()?,
            &record,
            &root_id,
        )?;

        // 信封 body：inviteCode 供接受方走 accept_invite 编排；组织/邀请人
        // 展示字段为自报，仅展示用（信任模型见 inbound_dm 模块头注释）
        let mut body = serde_json::json!({
            "inviteId": record.id,
            "inviteCode": created.invite,
            "orgId": org_id,
            "orgName": created.org_name,
            "inviterNickname": self.my_nickname(&root_id),
        });
        if !org.description.is_empty() {
            body["orgDescription"] = Value::from(org.description.clone());
        }
        if !org.avatar.is_empty() {
            body["orgAvatar"] = Value::from(org.avatar.clone());
        }
        if let Some(avatar) = self.my_avatar(&root_id) {
            body["inviterAvatar"] = Value::from(avatar);
        }
        let envelope = self.build_dm_envelope(KIND_ORG_INVITE, target_root_id, body)?;
        // 邀请丢失即对端永远不可见（无失败 UI），带退避重试防拨号竞争瞬态失败
        self.spawn_deliveries_with_retry(vec![(target, envelope)], &DM_RETRY_DELAYS);
        Ok(record)
    }

    /// 邀请目标寻址：显式 peerId/addresses → 组织成员 nodeInfo（前端预录）→
    /// 朋友记录；均无则报错。
    fn resolve_invite_target_peer(
        &self,
        org_id: &str,
        target_root_id: &str,
        target_peer_id: Option<&str>,
        target_addresses: &[String],
    ) -> Result<PeerNodeInfo> {
        let peer_id = target_peer_id
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let addresses: Vec<String> = target_addresses
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        if peer_id.is_some() || !addresses.is_empty() {
            return Ok(PeerNodeInfo { peer_id, addresses });
        }
        if let Some(record) = OrganizationService::get_record(self.require_storage()?, org_id)?
            && let Some(member) = record.find_member(target_root_id)
            && let Some(set) = &member.node_info
            && let Some(info) = set
                .iter()
                .find(|e| e.peer_id.is_some() || !e.addresses.is_empty())
        {
            return Ok(PeerNodeInfo {
                peer_id: info.peer_id.clone(),
                addresses: info.addresses.clone(),
            });
        }
        if let Some(friend) = ContactService::get_friend(self.require_storage()?, target_root_id)?
            && let Some(p) = friend
                .peers
                .into_iter()
                .find(|p| !p.peer_id.is_empty() || !p.addresses.is_empty())
        {
            return Ok(PeerNodeInfo {
                peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                addresses: p.addresses,
            });
        }
        Err(KernelError::Internal(
            "无法确定对方节点地址，请携带对方名片信息或先将其预录为成员".to_string(),
        ))
    }

    /// 回应收到的组织邀请（我是被邀请人）：幂等（已终态直接返回记录）。
    /// accept=true 先用记录里的 inviteCode 走 [`Kernel::accept_invite`] 编排
    /// （加入成功才标 accepted；失败原样报错、记录保持 pending、不回发），
    /// accept=false 直接标 declined；随后构造 `org-invite-reply` 信封尽力
    /// 回发邀请人（寻址取自 inviteCode 载荷的 inviter.peerId/addresses；
    /// 回发失败不阻塞本地状态）。返回最终记录。
    pub fn org_respond_invite(&mut self, invite_id: &str, accept: bool) -> Result<OrgInviteRecord> {
        let root_id = self.require_unlocked_root_id()?;
        // 查询类读取不加 io_lock（与 list_orgs 等查询口径一致）
        let record =
            OrganizationService::find_incoming_invite_by_id(self.require_storage()?, invite_id)?
                .ok_or_else(|| KernelError::Internal("组织邀请不存在".to_string()))?;
        if record.status != OrgInviteStatus::Pending {
            return Ok(record);
        }
        if accept {
            let code = record
                .invite_code
                .clone()
                .ok_or_else(|| KernelError::Internal("邀请记录缺少邀请码，无法接受".to_string()))?;
            // 加入编排（网络段，不持 io_lock，与 org_accept_invite 命令同口径）；
            // 失败原样报错：记录保持 pending、不回发
            self.accept_invite(&code)?;
        }
        let status = if accept {
            OrgInviteStatus::Accepted
        } else {
            OrgInviteStatus::Declined
        };
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let updated = OrganizationService::mark_invite_status_pdsync(
            self.require_storage_mut()?,
            OrgInviteDirection::Incoming,
            &record.org_id,
            &record.peer_root_id,
            status,
            now,
            &node_id,
        )?;
        let Some(updated) = updated else {
            // 并发下已被置终态（重复回应）：返回当前记录，不再回发
            return OrganizationService::get_incoming_invite(
                self.require_storage()?,
                &record.org_id,
                &record.peer_root_id,
            )?
            .ok_or_else(|| KernelError::Internal("组织邀请不存在".to_string()));
        };
        self.deliver_org_invite_reply(&updated, accept, &root_id);
        Ok(updated)
    }

    /// 回发 `org-invite-reply` 回执（尽力投递，全部失败路径静默跳过——本地
    /// 状态已落库，与 friend-accept 回发口径一致）。邀请人寻址取自 inviteCode
    /// 载荷的 inviter.peerId/addresses：`decode_org_invite_at(code, 0)` 只借
    /// 其成熟解析提取寻址字段（now=0 时新鲜度检查恒过——回执不受邀请码
    /// 24h 有效期约束）。
    fn deliver_org_invite_reply(&self, record: &OrgInviteRecord, accept: bool, my_root_id: &str) {
        let Some(code) = &record.invite_code else {
            return;
        };
        let Ok(payload) = decode_org_invite_at(code, 0) else {
            return;
        };
        let inviter = PeerNodeInfo {
            peer_id: payload.inviter.peer_id.clone(),
            addresses: payload.inviter.addresses.clone(),
        };
        if inviter.peer_id.is_none() && inviter.addresses.is_empty() {
            return;
        }
        let mut body = serde_json::json!({
            "inviteId": record.id,
            "orgId": record.org_id,
            "accept": accept,
            "nickname": self.my_nickname(my_root_id),
        });
        if let Some(avatar) = self.my_avatar(my_root_id) {
            body["avatar"] = Value::from(avatar);
        }
        if let Ok(envelope) =
            self.build_dm_envelope(KIND_ORG_INVITE_REPLY, &record.peer_root_id, body)
        {
            // F4 第一层（org-followups-batch3 §1.2）：退避重试（2s/5s）耗尽
            // 仍不可达 → 入 `org:dm:pending:{orgId}:` 队列（邀请人端点上线时
            // 经 host `on_peer_app_ready` 的 Org 空间通道按成员表端点补投——
            // 邀请人不必是好友）。终态拒绝（blocked 等语义性拒绝）不入队
            // （重试无意义；本侧 accepted 状态保留，不回滚已加入事实）。
            // messageId 定 inviteId——同邀请重复回执幂等覆盖。
            let mut storage = match self.require_storage() {
                Ok(s) => s.clone(),
                Err(_) => return,
            };
            let io_lock = std::sync::Arc::clone(&self.io_lock);
            let org_id = record.org_id.clone();
            let to_root_id = record.peer_root_id.clone();
            let message_id = format!("org-invite-reply-{}", record.id);
            let node_id = self.sync_node_id();
            // 入队助手：退避耗尽与「p2p 未启动 = 即时不可达」共用（裁决 §1.2
            // 「重试耗尽（或即时判定不可达）后入队」）。
            let enqueue_pending = {
                let io_lock = std::sync::Arc::clone(&io_lock);
                let org_id = org_id.clone();
                move |storage: &mut super::KernelStorage, envelope: Value| {
                    let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let to = to_root_id.clone();
                    let pending = crate::dm_offline::PendingRecord {
                        to: to.clone(),
                        message_id,
                        kind: KIND_ORG_INVITE_REPLY.to_string(),
                        space_key: format!("org:{org_id}"),
                        conv_id: None, // write_back 空操作（无消息状态可回写）
                        envelope,
                        created_at: crate::p2p::node::system_now_ms(),
                    };
                    let now = pending.created_at;
                    if let Err(e) = crate::dm_offline::enqueue(
                        storage,
                        crate::dm_offline::PendingSpace::Org(&org_id),
                        &to,
                        &pending,
                        &node_id,
                        now,
                    ) {
                        log::warn!("[ORG-INVITE] reply enqueue failed: {e}");
                    } else {
                        log::info!(
                            "[ORG-INVITE] reply queued for flush | org={org_id} to={}",
                            &to[..std::cmp::min(16, to.len())]
                        );
                    }
                }
            };
            let Some(node) = self.p2p.clone() else {
                // p2p 未启动 = 即时不可达：直接入队（不错过补投窗口）
                enqueue_pending(&mut storage, envelope);
                return;
            };
            self.runtime.handle().spawn(async move {
                let mut result = node.dm_direct(&inviter, envelope.clone()).await;
                for delay in DM_RETRY_DELAYS.iter() {
                    if !crate::kernel::dm_delivery::delivery_needs_retry(&result) {
                        break;
                    }
                    tokio::time::sleep(*delay).await;
                    result = node.dm_direct(&inviter, envelope.clone()).await;
                }
                if crate::kernel::dm_delivery::delivery_needs_retry(&result) {
                    enqueue_pending(&mut storage, envelope);
                }
            });
        }
    }

    /// 指定组织的全部邀请记录（出/入站合并；前端按身份各取所需）。
    pub fn org_invite_records(&self, org_id: &str) -> Result<Vec<OrgInviteRecord>> {
        self.require_current_root_id()?;
        Ok(OrganizationService::list_invite_records(
            self.require_storage()?,
            org_id,
        )?)
    }
}
