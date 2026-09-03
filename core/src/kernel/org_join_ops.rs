//! 受邀加入编排与成员移除通知（从 `org_ops` 拆出，文件长度硬线）：
//! `accept_invite`——阶段四A P2 起默认 stub 自举 + orgsync 收敛等待
//!（legacy pull 编排保留为开关回退）；`org-member-removed` 定向通知
//!（P2 L3，替代 legacy pull removed 传播）。

use std::time::Duration;

use serde_json::{Map, Value};

use super::{Kernel, KernelError, Result};
use crate::collection::DocumentCollection;
use crate::org::service::InviteAcceptance;
use crate::org::sync_state::sync_state_after_pull_synced;
use crate::org::{
    OrgInvitePayload, OrganizationNodeInfo, OrganizationService, PluginDocSyncItem,
    apply_plugin_doc_sync_items, build_organization_sync_versions_default, sign_node_info_claim,
};
use crate::p2p::node::system_now_ms;
use crate::p2p::PeerNodeInfo;
use crate::storage::StorageBackend;

/// 阶段四A P2（L1 join 通道迁移）开关：true = join 走 orgsync 收敛等待
///（stub 自举 + 即时 hello + 有界轮询）；false = 回退 legacy pull 编排。
/// 默认 true（P2 上线）；回滚开关供排障/评审（测试翻转须串行）。
static JOIN_VIA_ORGSYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// P2 join 通道回滚开关。
pub fn set_join_via_orgsync(via_orgsync: bool) {
    JOIN_VIA_ORGSYNC.store(via_orgsync, std::sync::atomic::Ordering::Relaxed);
}

/// 当前 join 通道口径（true = orgsync 收敛等待）。
pub fn join_via_orgsync() -> bool {
    JOIN_VIA_ORGSYNC.load(std::sync::atomic::Ordering::Relaxed)
}


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
    /// 阶段四A P2（L1 join 通道迁移）：默认走 **orgsync 收敛等待**——stub
    /// 记录自举 + 即时 orgsync-hello + 有界轮询本地到达，随后成员自写条目
    /// （见 [`Self::accept_invite_via_orgsync`]）；legacy pull 编排（claim
    /// 捎带 + pull-list/pull-org + 快照落库含 pluginDocs/副本记账）原样保留
    /// 于 [`Self::accept_invite_via_legacy_pull`]，`set_join_via_orgsync(false)`
    /// 回退（P3 出站停发时删除）。
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

        if join_via_orgsync() {
            self.accept_invite_via_orgsync(&payload, &root_id, &inviter, &local, self_device_uid)?;
        } else {
            self.accept_invite_via_legacy_pull(
                &payload,
                &root_id,
                inviter.clone(),
                &local,
                self_device_uid,
                now,
            )?;
        }
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
                        node_info: Some(
                            crate::org::types::OrganizationDeviceSet::from_single(self_endpoint),
                        ),
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

    /// legacy join 通道（P2 保留作开关回退，`set_join_via_orgsync(false)`
    /// 启用）：org-pull-list 捎带自签 nodeInfoClaim + org-pull-org 拉取快照
    /// 落库 + pluginDocs 应用 + 副本记账。P3 出站停发时随 legacy 平面删除。
    fn accept_invite_via_legacy_pull(
        &mut self,
        payload: &OrgInvitePayload,
        root_id: &str,
        inviter: PeerNodeInfo,
        local: &crate::p2p::LocalP2PNodeInfo,
        self_device_uid: Option<String>,
        now: i64,
    ) -> Result<()> {
        let node = self.p2p.as_ref().expect("p2p checked above").clone();
        // 自签 nodeInfoClaim（bootstrap.ts `buildSelfNodeInfoClaim`）：随首次 pull
        // 捎带，供管理员回填本机节点地址并经 gossip 扩散。
        let claim = sign_node_info_claim(
            &self
                .unlocked
                .as_ref()
                .expect("unlocked checked above")
                .identity
                .signing_key,
            OrganizationNodeInfo {
                device_uid: self_device_uid,
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
        list_payload.insert("requesterRootId".to_string(), Value::from(root_id.to_string()));
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
        org_payload.insert("requesterRootId".to_string(), Value::from(root_id.to_string()));
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
                let io_lock = std::sync::Arc::clone(&self.io_lock);
                let merged = OrganizationService::apply_incoming_snapshot(
                    self.require_storage_mut()?,
                    &io_lock,
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
                // 副本记账（org-pull-sync.ts `recordPullSyncState`；O1 账号口径：
                // 邀请人 rootId 定键）
                {
                    let versions = merged
                        .sync
                        .as_ref()
                        .map(|sync| sync.versions)
                        .unwrap_or_else(|| build_organization_sync_versions_default(&merged));
                    let state = sync_state_after_pull_synced(versions, now);
                    self.require_storage_mut()?.put(
                        &crate::org::sync_state::org_sync_state_account_key(
                            &payload.inviter.root_id,
                            &merged.org_id,
                        ),
                        &state.to_json(),
                    )?;
                }
            }
        }
        Ok(())
    }
}
