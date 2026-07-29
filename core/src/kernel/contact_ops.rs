//! 通讯录门面（`Kernel` 的通讯录 API）：空间总览、资料/拉黑/分组薄封装，
//! 以及好友申请出站（名片寻址 + dm 信封投递）与入站确认编排。
//!
//! 标签/分组/组织树的 id 一律由前端生成传入（contact 服务层的 `*_with_id`
//! 变体落库）；好友申请/确认的投递为尽力而为——投递 spawn 到 kernel
//! runtime（不在 io_lock 内 block_on 等应答），命令落库 pending 后立即
//! 返回，终态经 `P2pEvent::FriendRequestSent` 事件回传（失败置 Failed，
//! 前端可以同 id 重试）。

use serde::Deserialize;

use super::dm_envelope::{KIND_FRIEND_ACCEPT, KIND_FRIEND_REQUEST};
use super::{Kernel, KernelError, Result};
use crate::contact::{
    ContactGroup, ContactService, ContactTag, FriendRecord, FriendRequestRecord,
    FriendRequestStatus, OrgGroupNode, PeerRef, ProfilePatch, SpaceContactsView,
};
use crate::org::OrganizationService;
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::p2p::node::system_now_ms;

/// `contact_send_request` 的入参（serde camelCase；`id` 由前端生成）。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendFriendRequestInput {
    /// 申请 id（客户端生成，同时作信封 `requestId`）。
    pub id: String,
    /// 对方 rootId。
    pub root_id: String,
    /// 原始输入（扫码名片串 / 搜索关键字等；能按节点名片解析时取其中
    /// peerId/addresses 作为投递地址）。
    pub raw: String,
    /// 来源展示文案（如「RootID 搜索」「扫码」）。
    pub source: String,
    /// 验证消息。
    pub message: String,
}

impl Kernel {
    // ------------------------------------------------------------------
    // 视图与资料薄封装
    // ------------------------------------------------------------------

    /// 空间通讯录总览（个人空间：朋友/申请/标签/扁平分组；组织空间：成员
    /// 附加资料/标签/分组树）。
    ///
    /// 个人空间的 friends 恒含自己（rootId == 当前身份）：不存在则以当前
    /// 昵称创建（peer=None、资料默认），已存在则刷新 nickname（addedAt
    /// 保留首次创建时间）。给自己发消息 = 同步到同身份的其他设备（配对
    /// 产生的 rootId==自己 且 peer 非空的设备记录）。
    pub fn contact_overview(&mut self, space: &str) -> Result<SpaceContactsView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        if space == "personal" {
            self.ensure_self_friend()?;
        }
        Ok(ContactService::overview(self.require_storage()?, space)?)
    }

    /// 注入/刷新「自己」朋友条目（无当前身份时跳过）。
    fn ensure_self_friend(&mut self) -> Result<()> {
        let Some(root_id) = self.current_root_id()? else {
            return Ok(());
        };
        let nickname = self.my_nickname(&root_id);
        match ContactService::get_friend(self.require_storage()?, &root_id)? {
            Some(mut friend) => {
                if friend.nickname != nickname {
                    friend.nickname = nickname;
                    ContactService::upsert_friend(self.require_storage_mut()?, &friend)?;
                }
            }
            None => {
                let friend = FriendRecord {
                    root_id,
                    nickname,
                    signature: String::new(),
                    gender: None,
                    added_at: system_now_ms(),
                    peer: None,
                    remark: String::new(),
                    phones: Vec::new(),
                    tag_ids: Vec::new(),
                    group_id: String::new(),
                    memo: String::new(),
                    photos: Vec::new(),
                    permission: "open".to_string(),
                    blocked: false,
                };
                ContactService::upsert_friend(self.require_storage_mut()?, &friend)?;
            }
        }
        Ok(())
    }

    /// 更新联系人本地资料（`None` 字段保持不变）。
    pub fn contact_update_profile(
        &mut self,
        space: &str,
        root_id: &str,
        patch: ProfilePatch,
    ) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::update_profile(
            self.require_storage_mut()?,
            space,
            root_id,
            patch,
            system_now_ms(),
        )?;
        Ok(())
    }

    /// 设置/取消拉黑（自己 rootId 拒绝）。
    pub fn contact_set_blocked(&mut self, space: &str, root_id: &str, blocked: bool) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        if self.current_root_id()?.as_deref() == Some(root_id) {
            return Err(KernelError::Internal("不能拉黑自己".to_string()));
        }
        ContactService::set_blocked(
            self.require_storage_mut()?,
            space,
            root_id,
            blocked,
            system_now_ms(),
        )?;
        Ok(())
    }

    /// 删除朋友（个人空间；自己 rootId 拒绝）。
    pub fn contact_remove_friend(&mut self, root_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        if self.current_root_id()?.as_deref() == Some(root_id) {
            return Err(KernelError::Internal("不能删除自己".to_string()));
        }
        ContactService::remove_friend(self.require_storage_mut()?, root_id)?;
        Ok(())
    }

    /// 设置联系人所属分组（`""` = 未分组）。
    pub fn contact_set_group(&mut self, space: &str, root_id: &str, group_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::set_contact_group(
            self.require_storage_mut()?,
            space,
            root_id,
            group_id,
            system_now_ms(),
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 标签 / 分组 / 组织分组树（id 由前端生成传入）
    // ------------------------------------------------------------------

    /// 新建标签。
    pub fn contact_tag_create(&mut self, space: &str, id: &str, name: &str) -> Result<ContactTag> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ContactService::create_tag_with_id(
            self.require_storage_mut()?,
            space,
            id,
            name,
        )?)
    }

    /// 重命名标签。
    pub fn contact_tag_rename(&mut self, space: &str, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_tag(self.require_storage_mut()?, space, id, name)?;
        Ok(())
    }

    /// 删除标签（从所有资料中摘除）。
    pub fn contact_tag_delete(&mut self, space: &str, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_tag(self.require_storage_mut()?, space, id)?;
        Ok(())
    }

    /// 新建个人空间扁平分组。
    pub fn contact_group_create(&mut self, id: &str, name: &str) -> Result<ContactGroup> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ContactService::create_group_with_id(
            self.require_storage_mut()?,
            id,
            name,
        )?)
    }

    /// 重命名分组。
    pub fn contact_group_rename(&mut self, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_group(self.require_storage_mut()?, id, name)?;
        Ok(())
    }

    /// 删除分组（组内朋友复位为未分组）。
    pub fn contact_group_delete(&mut self, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_group(self.require_storage_mut()?, id)?;
        Ok(())
    }

    /// 拖拽重排分组（越界夹紧）。
    pub fn contact_group_move(&mut self, id: &str, to_index: usize) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::move_group(self.require_storage_mut()?, id, to_index)?;
        Ok(())
    }

    /// 新建组织分组（`parent_id` 为 `""` 挂根层；父不存在返回 `Ok(None)`）。
    pub fn contact_org_group_create(
        &mut self,
        space: &str,
        parent_id: &str,
        id: &str,
        name: &str,
    ) -> Result<Option<OrgGroupNode>> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ContactService::create_org_group_with_id(
            self.require_storage_mut()?,
            space,
            parent_id,
            id,
            name,
        )?)
    }

    /// 重命名组织分组。
    pub fn contact_org_group_rename(&mut self, space: &str, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_org_group(self.require_storage_mut()?, space, id, name)?;
        Ok(())
    }

    /// 删除组织分组（子节点提升一层）。
    pub fn contact_org_group_delete(&mut self, space: &str, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_org_group(self.require_storage_mut()?, space, id)?;
        Ok(())
    }

    /// 同级拖拽重排组织分组。
    pub fn contact_org_group_move(&mut self, space: &str, id: &str, to_index: usize) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::move_org_group_sibling(self.require_storage_mut()?, space, id, to_index)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 好友申请（出站 / 确认）
    // ------------------------------------------------------------------

    /// 发出好友申请：
    /// 1. outbox 已有同 id 记录 → 重试路径：状态须为 pending/failed，复用
    ///    已存记录的 rootId/peer/message/source（忽略本次 `raw` 的名片解析
    ///    ——名片来源的申请重试时 raw 可能只是 rootId，重新寻址必败），重置
    ///    pending 并刷新 updated_at 后重新投递；
    /// 2. 新申请：已是朋友 → 报错；寻址（`raw` 按节点名片（org.md §17）
    ///    解析取 peerId/addresses，否则遍历我的组织成员 nodeInfo 匹配
    ///    rootId）失败报错；
    /// 3. outbox 落库 pending（id 用 `input.id`）后立即返回；
    /// 4. 投递 spawn 到 kernel runtime（不在 io_lock 内 block_on 等应答，
    ///    模式同 dm_delivery 的 spawn_chat_delivery），终态经
    ///    `P2pEvent::FriendRequestSent` 事件回传：ok 应答带 nickname 回填
    ///    昵称，无应答/失败置 Failed；p2p 未运行直接置 Failed 并同步发事件。
    pub fn contact_send_request(
        &mut self,
        input: SendFriendRequestInput,
    ) -> Result<FriendRequestRecord> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        // 重试路径：同 id 记录已存在（前端重试）
        if let Some(existing) =
            ContactService::get_outgoing_request(self.require_storage()?, &input.id)?
        {
            if !matches!(
                existing.status,
                FriendRequestStatus::Pending | FriendRequestStatus::Failed
            ) {
                return Err(KernelError::Internal(
                    "该申请已处理，无法重新发送".to_string(),
                ));
            }
            let mut request = existing;
            request.status = FriendRequestStatus::Pending;
            request.updated_at = now;
            ContactService::put_outgoing_request(self.require_storage_mut()?, &request)?;
            self.deliver_friend_request(&request, &my_root_id)?;
            return Ok(request);
        }
        // 自己放行：允许重复配对以刷新设备地址（overview 注入的「自己」条目
        // 不视为冲突）
        if input.root_id != my_root_id
            && ContactService::get_friend(self.require_storage()?, &input.root_id)?.is_some()
        {
            return Err(KernelError::Internal("对方已经是你的朋友".to_string()));
        }
        let peer = self.resolve_request_peer(&input)?;
        let request = FriendRequestRecord {
            id: input.id.clone(),
            root_id: input.root_id.clone(),
            nickname: String::new(),
            message: input.message.clone(),
            source: input.source.clone(),
            status: FriendRequestStatus::Pending,
            created_at: now,
            updated_at: now,
            peer: Some(peer),
        };
        ContactService::put_outgoing_request(self.require_storage_mut()?, &request)?;
        self.deliver_friend_request(&request, &my_root_id)?;
        Ok(request)
    }

    /// 投递 friend-request 信封（捎带本机节点信息）：p2p 运行时 spawn 异步
    /// 任务（终态回写 + 事件见 `spawn_friend_request_delivery`）；p2p 未
    /// 运行时投递必然失败，直接置 Failed 并同步发 `FriendRequestSent` 事件。
    /// 信封构造失败/记录无地址则跳过投递，outbox 保留 pending。
    fn deliver_friend_request(
        &mut self,
        request: &FriendRequestRecord,
        my_root_id: &str,
    ) -> Result<()> {
        let Some(peer) = &request.peer else {
            return Ok(());
        };
        let mut body = serde_json::json!({
            "requestId": request.id,
            "nickname": self.my_nickname(my_root_id),
            "message": request.message,
            "source": request.source,
        });
        if let Some(node_info) = self.local_node_info_json() {
            body["nodeInfo"] = node_info;
        }
        let Ok(envelope) = self.build_dm_envelope(KIND_FRIEND_REQUEST, &request.root_id, body)
        else {
            return Ok(());
        };
        let target = PeerNodeInfo {
            peer_id: (!peer.peer_id.is_empty()).then(|| peer.peer_id.clone()),
            addresses: peer.addresses.clone(),
        };
        if self.p2p.is_none() {
            let mut record = request.clone();
            record.status = FriendRequestStatus::Failed;
            record.updated_at = system_now_ms();
            ContactService::put_outgoing_request(self.require_storage_mut()?, &record)?;
            let _ = self.event_tx.send(P2pEvent::FriendRequestSent(
                serde_json::json!({ "request": record }),
            ));
            return Ok(());
        }
        self.spawn_friend_request_delivery(request.clone(), target, envelope);
        Ok(())
    }

    /// spawn 好友申请投递任务（句柄先克隆再 move，不捕获 `&Kernel`，模式同
    /// dm_delivery 的 `spawn_chat_delivery`）：ok 应答捎带对方昵称时回填
    /// outbox 记录昵称（「新的朋友」列表展示名），无应答/失败置 Failed；
    /// 两种结局都 emit `FriendRequestSent`（data 为最终 outbox 记录，前端
    /// 按 id upsert）。命令侧立即返回 pending 记录，前端按事件更新。
    fn spawn_friend_request_delivery(
        &self,
        request: FriendRequestRecord,
        peer: PeerNodeInfo,
        envelope: serde_json::Value,
    ) {
        let (Some(node), Some(mut storage)) = (self.p2p.clone(), self.storage.clone()) else {
            return;
        };
        let event_tx = self.event_tx.clone();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        self.runtime.handle().spawn(async move {
            let resp = node.dm_direct(&peer, envelope).await.ok().flatten();
            let mut record = request;
            match resp {
                Some(resp) if resp.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
                    let nickname = resp
                        .get("nickname")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if !nickname.is_empty() {
                        record.nickname = nickname.to_string();
                        record.updated_at = system_now_ms();
                    }
                }
                _ => {
                    record.status = FriendRequestStatus::Failed;
                    record.updated_at = system_now_ms();
                }
            }
            {
                let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = ContactService::put_outgoing_request(&mut storage, &record);
            }
            let _ = event_tx.send(P2pEvent::FriendRequestSent(
                serde_json::json!({ "request": record }),
            ));
        });
    }

    /// 处理收到的好友申请（pending → accepted/ignored）；接受时建朋友
    /// （peer 取请求记录、`permission` 写入资料）并向对方发 friend-accept
    /// 信封（尽力而为）。申请不存在或已处理报错。
    pub fn contact_resolve_request(
        &mut self,
        request_id: &str,
        accept: bool,
        permission: Option<&str>,
    ) -> Result<FriendRequestRecord> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let resolved = ContactService::resolve_incoming_request(
            self.require_storage_mut()?,
            request_id,
            accept,
            now,
        )?;
        if !resolved {
            return Err(KernelError::Internal(
                "好友申请不存在或已处理".to_string(),
            ));
        }
        let request = ContactService::get_incoming_request(self.require_storage()?, request_id)?
            .expect("resolved above");
        if accept {
            self.accept_request_side_effects(&request, permission, &my_root_id, now)?;
        }
        Ok(request)
    }

    /// 接受好友申请的副作用：建/更新朋友（peer 取请求记录；已有记录保留
    /// 本地资料与 addedAt，仅刷新非空 nickname 与 Some 的 peer，permission
    /// 仅新建时写入）并向对方发 friend-accept 信封（尽力而为：spawn 异步
    /// 投递，无应答处理，不在 io_lock 内 block_on）。
    fn accept_request_side_effects(
        &mut self,
        request: &FriendRequestRecord,
        permission: Option<&str>,
        my_root_id: &str,
        now: i64,
    ) -> Result<()> {
        let existing = ContactService::get_friend(self.require_storage()?, &request.root_id)?;
        let mut friend = existing.unwrap_or(FriendRecord {
            root_id: request.root_id.clone(),
            nickname: String::new(),
            signature: String::new(),
            gender: None,
            added_at: now,
            peer: None,
            remark: String::new(),
            phones: Vec::new(),
            tag_ids: Vec::new(),
            group_id: String::new(),
            memo: String::new(),
            photos: Vec::new(),
            permission: permission.unwrap_or("open").to_string(),
            blocked: false,
        });
        if !request.nickname.is_empty() {
            friend.nickname = request.nickname.clone();
        }
        if request.peer.is_some() {
            friend.peer = request.peer.clone();
        }
        ContactService::upsert_friend(self.require_storage_mut()?, &friend)?;
        if let Some(peer) = &request.peer {
            let mut body = serde_json::json!({
                "requestId": request.id,
                "nickname": self.my_nickname(my_root_id),
            });
            if let Some(node_info) = self.local_node_info_json() {
                body["nodeInfo"] = node_info;
            }
            if let Ok(envelope) =
                self.build_dm_envelope(KIND_FRIEND_ACCEPT, &request.root_id, body)
            {
                let target = PeerNodeInfo {
                    peer_id: (!peer.peer_id.is_empty()).then(|| peer.peer_id.clone()),
                    addresses: peer.addresses.clone(),
                };
                self.spawn_deliveries(vec![(target, envelope)]);
            }
        }
        Ok(())
    }

    /// 好友申请寻址：名片解析 → 组织成员 nodeInfo；均失败报错。
    fn resolve_request_peer(&self, input: &SendFriendRequestInput) -> Result<PeerRef> {
        let now = system_now_ms();
        if let Ok(card) = crate::org::parse_and_verify_node_card(&input.raw, now) {
            return Ok(PeerRef {
                peer_id: card.peer_id,
                addresses: card.addresses,
            });
        }
        let storage = self.require_storage()?;
        for record in OrganizationService::read_all_organizations(storage)? {
            if let Some(member) = record.find_member(&input.root_id)
                && let Some(info) = &member.node_info
            {
                return Ok(PeerRef {
                    peer_id: info.peer_id.clone().unwrap_or_default(),
                    addresses: info.addresses.clone(),
                });
            }
        }
        Err(KernelError::Internal(
            "无法确定对方节点地址，请使用扫码名片添加".to_string(),
        ))
    }
}
