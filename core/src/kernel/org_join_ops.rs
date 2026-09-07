//! 受邀加入编排与成员移除通知（从 `org_ops` 拆出，文件长度硬线）：
//! `accept_invite`——阶段四A P2 起 stub 自举 + orgsync 收敛等待（legacy
//! pull 回退编排已随 org-share/org-pull 平面退役删除）；
//! `org-member-removed` 定向通知（P2 L3，替代 legacy pull removed 传播）。

use std::time::Duration;

use serde_json::Value;

use super::{Kernel, KernelError, Result};
use crate::org::service::InviteAcceptance;
use crate::org::{OrgInvitePayload, OrganizationNodeInfo, OrganizationService};
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;
use crate::storage::StorageBackend;

impl Kernel {
    /// P2 L3：`org-member-removed` 定向通知投递（spawn 异步，不阻塞命令）。
    /// 逐端点退避重试；任一端点送达即视为账号已通知（其余自设备由 legacy
    /// pull removed / 后续收敛覆盖）；全部失败入 org 域 pending 队列补投。
    pub(crate) fn notify_org_member_removed(
        &self,
        org_id: &str,
        target_root_id: &str,
        peers: Vec<PeerNodeInfo>,
    ) {
        if peers.is_empty() {
            return; // 无寻址：无法通知，收敛通道兜底（见上）
        }
        let (Some(node), Some(storage)) = (self.p2p.clone(), self.storage.clone()) else {
            return;
        };
        let body = serde_json::json!({
            "orgId": org_id,
            // 发送侧补投寻址快照（收端不消费）：被移除者出成员表后 flush
            // 无法从成员表反查 rootId，靠此匹配连接事件
            "targetPeerIds": peers.iter().filter_map(|p| p.peer_id.clone()).collect::<Vec<_>>(),
        });
        let Ok(envelope) = self.build_dm_envelope(
            super::dm_envelope::KIND_ORG_MEMBER_REMOVED,
            target_root_id,
            body.clone(),
        ) else {
            return;
        };
        let node_id = self.sync_node_id();
        let org_id = org_id.to_string();
        let to = target_root_id.to_string();
        self.runtime.handle().spawn(async move {
            let mut reached = false;
            for peer in &peers {
                let mut result = node.dm_direct(peer, envelope.clone()).await;
                for delay in super::dm_delivery::DM_RETRY_DELAYS.iter() {
                    if !super::dm_delivery::delivery_needs_retry(&result) {
                        break;
                    }
                    tokio::time::sleep(*delay).await;
                    result = node.dm_direct(peer, envelope.clone()).await;
                }
                if matches!(&result, Ok(Some(resp)) if resp.get("ok").and_then(Value::as_bool) == Some(true))
                {
                    reached = true;
                    break;
                }
            }
            if !reached {
                let mut storage = storage;
                super::dm_delivery::enqueue_sync_pending(
                    &mut storage,
                    crate::dm_offline::PendingSpace::Org(&org_id),
                    &to,
                    super::dm_envelope::KIND_ORG_MEMBER_REMOVED,
                    &body,
                    &envelope,
                    &node_id,
                );
            }
        });
    }

    /// `acceptOrgInvite` 编排（service.ts:345-374）：解码邀请码 → 连邀请人 →
    /// 等待组织记录到达 → 成员确认。
    ///
    /// 阶段四A P2（L1 join 通道迁移）：走 **orgsync 收敛等待**——stub
    /// 记录自举 + 即时 orgsync-hello + 有界轮询本地到达，随后成员自写条目
    /// （见 [`Self::accept_invite_via_orgsync`]）。legacy pull 编排（claim
    /// 捎带 + pull-list/pull-org + 快照落库）已随 org-share/org-pull 平面
    /// 退役删除。
    ///
    /// 与 TS 的差异：TS 的 `connectAndPull` 是一次全量反熵（协调双方全部共同组织），
    /// 本方法按加入语义只协调受邀组织；其他共同组织的协调留给组织 keepalive 编排。
    ///
    /// 需要已解锁身份与运行中的 P2P（否则报 TS 文案
    /// "P2P 网络未启动，无法通过邀请码加入"）；邀请人连接失败按 TS
    /// `connectPeer` 文案报错；收敛超时/非成员按 TS 路径降级为
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
        // 端点化：声明携带本机 deviceUid，管理员侧按 deviceUid 聚合成员端点、
        // 判同设备 peerId 墓碑化。先取（可变借用存储），再借 p2p。
        let self_device_uid =
            crate::device::get_or_create_device_uid(self.require_storage_mut()?).ok();
        let node = self.p2p.as_ref().expect("p2p checked above");
        let local = self.runtime.handle().block_on(node.local_node_info())?;

        self.accept_invite_via_orgsync(&payload, &root_id, &inviter, &local, self_device_uid)?;
        self.check_join(&payload.org_id)
    }

    /// P2 join 新通道（阶段四A 设计 §4 P2 L1）：connect 后**不再走
    /// org-pull**——
    ///
    /// 1. 本地无该组织记录时自举 **stub 记录**（邀请载荷携带 orgId/orgName/
    ///    邀请人）：members = [邀请人（Admin), 本人（Member，含本机端点）]，
    ///    `updated_at = 0` 保证 F1 合并时真实记录全字段秩高（stub 只提供
    ///    「from ∈ 成员表」与复制组判定的最小前提；本人端点经成员并集 +
    ///    nodeInfo 并集自然存活）；stub 经版本化句柄落库（本机分量 bump，
    ///    与对端 whole 判 Concurrent → 结构化合并而非整值覆盖），落库后
    ///    pmeta.ts 压 0（裁决 §10.1：零信息占位在任何裁决面皆输——对齐
    ///    pdsync ts 裁决与条目级合并秩）；
    /// 2. 立即向邀请人发 orgsync-hello（本机集合 vv 为空 ⟹ 邀请人回推全量
    ///    data）；发送失败不中断——邀请人侧 tick hello 会反向触发同一交换；
    /// 3. 有界轮询本地记录「真实到达」（`updated_at > 0` ⟹ 已与邀请人版本
    ///    合并过，排除 stub 自身命中）；
    /// 4. 到达后**成员自写条目**（L2：端点经 `org:member:{org}:{self}` 全员
    ///    扩散，claim 通道退役）。
    ///
    /// 轮询超时/非成员不另行报错——统一落到末尾 `check_join` 的既有错误
    /// （与 TS 降级路径一致）。
    fn accept_invite_via_orgsync(
        &mut self,
        payload: &OrgInvitePayload,
        root_id: &str,
        inviter: &PeerNodeInfo,
        local: &crate::p2p::LocalP2PNodeInfo,
        self_device_uid: Option<String>,
    ) -> Result<()> {
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        let org_id = &payload.org_id;

        // 1. stub 自举（已有记录 = 重入/他设备已加入 → 跳过）。整段持
        //    io_lock（评审修复）：版本化落库与 ts 压 0 的 raw 补写之间若不
        //    持锁，入站合入线程可穿插——真实 whole 合并落地后被 ts=0 补写
        //        回盖成 stub pmeta（vv 回退 + 内容/vv 脱节，F8 类病灶）。
        //    锁域只包本段（步骤 2/3 的 block_on/轮询不持锁）。
        {
            let __io = std::sync::Arc::clone(&self.io_lock);
            let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
            if OrganizationService::get_record(self.require_storage()?, org_id)?.is_none() {
                let self_endpoint = OrganizationNodeInfo {
                    device_uid: self_device_uid.clone(),
                    peer_id: local.peer_id.clone(),
                    addresses: local.addresses.clone(),
                };
                let stub = crate::org::types::OrganizationRecord {
                    org_id: org_id.clone(),
                    name: payload.org_name.clone(),
                    created_at: 0,
                    created_by: payload.inviter.root_id.clone(),
                    updated_at: 0,
                    members: vec![
                        crate::org::types::OrganizationMember {
                            root_id: payload.inviter.root_id.clone(),
                            role: crate::org::types::OrganizationRole::Admin,
                            joined_at: 0,
                            added_by: payload.inviter.root_id.clone(),
                            ..Default::default()
                        },
                        crate::org::types::OrganizationMember {
                            root_id: root_id.to_string(),
                            role: crate::org::types::OrganizationRole::Member,
                            joined_at: 0,
                            added_by: payload.inviter.root_id.clone(),
                            node_info: Some(crate::org::types::OrganizationDeviceSet::from_single(
                                self_endpoint,
                            )),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                };
                let storage = self.require_storage_mut()?;
                OrganizationService::save_record_pdsync(storage, &stub, now, &node_id)?;
                // 双写初始条目（与 create 口径一致——装配视图条目为权威段）
                for member in &stub.members {
                    storage.put(
                        &crate::org::types::org_member_key(org_id, &member.root_id),
                        &serde_json::to_string(member)?,
                    )?;
                }
                // 内建 all-members 集合声明（hello 摘要需声明在库；与邀请人侧
                // 同策略声明收敛到 (declaredAt, declaredBy) 小者，无争用面）
                crate::plugindata::declare_builtin_org_collections(
                    storage, org_id, root_id, now, &node_id,
                )?;
                // 裁决 §10.1（org-p2-channel-review 建议 1）：stub 是**零信息
                // 占位**——`updated_at=0` 只压住 F1 记录秩；版本化句柄落库的
                // pmeta.ts = 当下时刻，pdsync 的 Concurrent 裁决按 pmeta.ts，
                // 后加入设备的 stub 可凭 ts 优势整值覆盖先加入设备的真实
                // whole（多设备窗口）。此处把 stub whole 与 stub 成员条目的
                // pmeta.ts 压 0（任何裁决面皆输；vv 保留——传播/后续合并正常）。
                // 已核：apply_personal_remote 无 ts 时间窗拦截（pwv 专用窗不
                // 涉及），折叠只看 vv 不受 ts=0 影响。
                {
                    let mut keys = vec![crate::org::types::organization_key(org_id)];
                    keys.extend(
                        stub.members
                            .iter()
                            .map(|m| crate::org::types::org_member_key(org_id, &m.root_id)),
                    );
                    let raw = self.require_storage_mut()?.raw_mut();
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
                }
            }
        }

        // 2. 即时 orgsync-hello 踢一脚（失败不中断——等对端 tick 反向触发）
        {
            let storage = self.require_storage()?;
            if let Ok(collections) = crate::sync::orgsync::collect_org_collections(
                storage,
                org_id,
                &[("org:structure".to_string(), "1".to_string())],
                &payload.inviter.root_id,
                inviter.peer_id.as_deref().unwrap_or_default(),
            ) && let Some(record) = OrganizationService::get_record(storage, org_id)?
            {
                let roles = crate::sync::orgsync::self_roles(&record, root_id, now);
                let hello = crate::sync::orgsync::build_orgsync_hello(
                    org_id,
                    collections,
                    &roles,
                    crate::sync::pdsync::local_device_class(),
                );
                let envelope = super::dm_envelope::build_envelope(
                    super::dm_envelope::KIND_ORGSYNC_HELLO,
                    root_id,
                    &payload.inviter.root_id,
                    now,
                    hello,
                    &self
                        .unlocked
                        .as_ref()
                        .expect("unlocked above")
                        .identity
                        .signing_key,
                );
                let node = self.p2p.as_ref().expect("p2p above").clone();
                let inviter = inviter.clone();
                let _ = self
                    .runtime
                    .handle()
                    .block_on(node.dm_direct(&inviter, envelope));
            }
        }

        // 3. 有界轮询「真实到达」（updated_at > 0 ⟹ 已与邀请人版本合并，
        //    排除 stub 自命中）：40 × 250ms = 10s 上限
        for _ in 0..40 {
            let arrived = OrganizationService::get_record(self.require_storage()?, org_id)?
                .is_some_and(|rec| rec.updated_at > 0 && rec.find_member(root_id).is_some());
            if arrived {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }

        // 4. 成员自写条目（L2 claim 退役的取代通道；未加入时函数内成员
        //    校验为无操作）
        OrganizationService::upsert_own_member_entry(
            self.require_storage_mut()?,
            org_id,
            root_id,
            &OrganizationNodeInfo {
                device_uid: self_device_uid,
                peer_id: local.peer_id.clone(),
                addresses: local.addresses.clone(),
            },
        )?;
        Ok(())
    }
}
