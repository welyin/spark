//! 通讯录门面（`Kernel` 的通讯录 API）：空间总览、资料/拉黑薄封装，
//! 以及好友申请出站（名片寻址 + dm 信封投递）与入站确认编排。
//! 标签/分组/组织分组树在 `contact_group_ops.rs`、好友申请来回回复
//! （询问/答复）在 `contact_request_ops.rs`（本文件长度上限拆出）。
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
    ContactService, FriendRecord, FriendRequestRecord, FriendRequestStatus, PeerRef,
    ProfilePatch, SpaceContactsView,
};
use crate::org::OrganizationService;
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::p2p::node::system_now_ms;
use crate::plugin::{PluginHostShared, Result as PluginResult};

/// Bot 联系人注册的共享实现：[`Kernel::contact_ensure_bot`] 门面与插件后台
/// 运行时的 `contact.ensureBot` 能力共用——已存在仅刷新 nickname（不覆盖
/// 备注/分组等用户自定义字段）。
pub(crate) fn ensure_bot_shared(
    host: &PluginHostShared,
    bot_root_id: &str,
    display_name: &str,
) -> PluginResult<()> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut storage = host.require_storage()?;
    let now = system_now_ms();
    let existing = ContactService::get_friend(&storage, bot_root_id)?;
    let friend = if let Some(existing) = existing {
        let mut f = existing;
        if !display_name.is_empty() {
            f.nickname = display_name.to_string();
        }
        f
    } else {
        FriendRecord {
            root_id: bot_root_id.to_string(),
            nickname: display_name.to_string(),
            avatar: None,
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
            permission: "open".to_string(),
            blocked: false,
            updated_at: now,
        }
    };
    ContactService::upsert_friend(&mut storage, &friend)?;
    Ok(())
}

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
    /// 可省：前端解析名片（spark-card JSON / 名片内容文本）得到的 peerId；
    /// 与 `addresses` 同时提供时优先于 `raw` 的节点名片解析。
    #[serde(default)]
    pub peer_id: Option<String>,
    /// 可省：前端解析名片得到的 multiaddr 列表（非空才生效）。
    #[serde(default)]
    pub addresses: Option<Vec<String>>,
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
                    avatar: None,
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
                    updated_at: system_now_ms(),
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
        if space == "personal" {
            self.broadcast_contact_sync();
        }
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
        if space == "personal" {
            self.broadcast_contact_sync();
        }
        Ok(())
    }

    /// 删除朋友（个人空间；自己 rootId 拒绝）。`block` 为 true 时删除后
    /// 同时写入拉黑集合（§5.5「删除同时拉黑」；陌生人可拉黑语义同
    /// `contact_set_blocked`，删除朋友本就不清拉黑状态）。
    pub fn contact_remove_friend(&mut self, root_id: &str, block: bool) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        if self.current_root_id()?.as_deref() == Some(root_id) {
            return Err(KernelError::Internal("不能删除自己".to_string()));
        }
        ContactService::remove_friend(self.require_storage_mut()?, root_id)?;
        if block {
            ContactService::set_blocked(
                self.require_storage_mut()?,
                "personal",
                root_id,
                true,
                system_now_ms(),
            )?;
        }
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
        if space == "personal" {
            self.broadcast_contact_sync();
        }
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
    /// 2. 新申请：已是朋友 → 报错；寻址（优先入参显式携带的 peerId/addresses
    ///    （前端已解析名片），否则 `raw` 按节点名片（org.md §17）解析，再否则
    ///    遍历我的组织成员 nodeInfo 匹配 rootId）失败报错；
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
            self.broadcast_contact_sync();
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
            avatar: None,
            message: input.message.clone(),
            source: input.source.clone(),
            status: FriendRequestStatus::Pending,
            created_at: now,
            updated_at: now,
            peer: Some(peer),
            thread: Vec::new(),
            invite_code: None,
        };
        ContactService::put_outgoing_request(self.require_storage_mut()?, &request)?;
        self.deliver_friend_request(&request, &my_root_id)?;
        self.broadcast_contact_sync();
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
        if let Some(avatar) = self.my_avatar(my_root_id) {
            body["avatar"] = serde_json::Value::from(avatar);
        }
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
    /// outbox 记录昵称（「新的朋友」列表展示名；昵称缺失/超上限按未提供
    /// 处理——投递已成功，**不动状态**），无应答/失败置 Failed；
    /// 两种结局都 emit `FriendRequestSent`（data 为最终 outbox 记录，前端
    /// 按 id upsert）。命令侧立即返回 pending 记录，前端按事件更新。
    ///
    /// 终态回写为读-改-写（锁内重读当前记录，只动 status/updatedAt/nickname）
    /// ——投递等待期间记录可能已被 friend-reply 入站等路径更新（thread 追加、
    /// replied），整写旧快照会回退这些变更。
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
            let delivered_ok = resp
                .as_ref()
                .and_then(|r| r.get("ok"))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            // 应答捎带的对端自报昵称：trim 后超上限/为空一律按未提供处理——
            // 对端可控字段只受帧上限约束，原样落库会刷大记录、撑破列表 UI。
            // 「ok 但昵称缺失/非法」与「无应答/投递失败」必须正交判定：
            // 前者投递已成功（保持 Pending），后者才允许置 Failed
            let ok_nickname = if delivered_ok {
                resp.as_ref()
                    .and_then(|r| r.get("nickname"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| {
                        !s.is_empty() && s.chars().count() <= crate::identity::NICKNAME_MAX_CHARS
                    })
                    .map(str::to_string)
            } else {
                None
            };
            let record = {
                let _io = io_lock.lock().unwrap_or_else(|e| e.into_inner());
                let read = ContactService::get_outgoing_request(&storage, &request.id);
                let Ok(Some(mut current)) = read else {
                    return;
                };
                if let Some(nickname) = ok_nickname {
                    current.nickname = nickname;
                    current.updated_at = system_now_ms();
                } else if !delivered_ok {
                    // 仅仍 Pending 时置 Failed——投递等待期间对方可能已
                    // accept（→Accepted）或 reply（→Replied）入站，无条件
                    // 回退会丢终态/询问 UI 态，且 Failed 允许重发会在对端
                    // 再造一条 pending 申请
                    if current.status == FriendRequestStatus::Pending {
                        current.status = FriendRequestStatus::Failed;
                        current.updated_at = system_now_ms();
                    }
                }
                let _ = ContactService::put_outgoing_request(&mut storage, &current);
                current
            };
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
        self.broadcast_contact_sync();
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
            avatar: None,
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
            updated_at: now,
        });
        if !request.nickname.is_empty() {
            friend.nickname = request.nickname.clone();
        }
        if let Some(avatar) = &request.avatar {
            friend.avatar = Some(avatar.clone());
        }
        if request.peer.is_some() {
            friend.peer = request.peer.clone();
        }
        friend.updated_at = now;
        ContactService::upsert_friend(self.require_storage_mut()?, &friend)?;
        if let Some(peer) = &request.peer {
            // 入站申请记录 id 为复合形式 `{from}:{原 requestId}`（防跨发送者撞 id，
            // 见 inbound_dm handle_friend_request）；回发必须带原 requestId——对方
            // outbox 按原始 id 落库，复合 id 查无记录会被拒（对方状态永不更新）
            let original_request_id = request
                .id
                .strip_prefix(&format!("{}:", request.root_id))
                .unwrap_or(&request.id);
            let mut body = serde_json::json!({
                "requestId": original_request_id,
                "nickname": self.my_nickname(my_root_id),
            });
            if let Some(avatar) = self.my_avatar(my_root_id) {
                body["avatar"] = serde_json::Value::from(avatar);
            }
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

    /// 好友申请寻址：前端解析名片上行的 peerId/addresses → 节点名片（raw）
    /// → 组织成员 nodeInfo；均失败报错。
    fn resolve_request_peer(&self, input: &SendFriendRequestInput) -> Result<PeerRef> {
        let now = system_now_ms();
        if let Some(addresses) = input.addresses.as_ref().filter(|list| !list.is_empty()) {
            return Ok(PeerRef {
                peer_id: input.peer_id.clone().unwrap_or_default(),
                addresses: addresses.clone(),
            });
        }
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

    /// 创建/更新 Bot 联系人：将 bot 虚拟联系人注册为好友记录，使其出现在
    /// 通讯录列表中。若已存在则保留已有资料（不覆盖备注、分组等用户自定义
    /// 字段），只刷新 nickname。
    pub fn contact_ensure_bot(&mut self, bot_root_id: &str, display_name: &str) -> Result<()> {
        Ok(ensure_bot_shared(&self.plugin_host, bot_root_id, display_name)?)
    }
}
