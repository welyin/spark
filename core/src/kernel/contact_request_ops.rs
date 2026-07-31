//! 通讯录门面（好友申请的来回回复，ui-contacts §4）：申请方答复
//! [`Kernel::contact_reply_request`] 与接收方主动询问
//! [`Kernel::contact_ask_request`]——同一条 friend-reply 信封，按本端记录
//! （outbox / inbox 复合 id）区分方向，入站匹配见 inbound_dm/friend.rs。
//!
//! 自 `contact_ops.rs` 拆出（文件长度上限，§2.1），同属 [`Kernel`] 的
//! 通讯录 API。两个命令都只落本地 thread 后尽力投递（带退避重试，
//! friend-reply 丢失同样静默卡死且无失败 UI），不发额外事件——命令返回
//! 记录由前端本地收敛（同 `contact_resolve_request` 先例）。

use super::dm_delivery::DM_RETRY_DELAYS;
use super::dm_envelope::{KIND_FRIEND_REPLY, friend_reply_body};
use super::{Kernel, KernelError, Result};
use crate::contact::{
    ContactService, FriendRequestRecord, FriendRequestStatus, RequestThreadMessage, ThreadFrom,
};
use crate::message::MAX_TEXT_BYTES;
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 回复对方的询问（好友申请的来回回复，ui-contacts §4）：本地 outbox
    /// thread 追加 from=me（status 回 pending 等待对方），并向对方投递
    /// friend-reply 信封（`spawn_deliveries_with_retry` 带退避重试、无应答
    /// 处理；p2p 未运行则跳过投递不置失败——回复无失败 UI，本地记录已落）。
    /// 不发额外事件（命令返回记录由前端本地收敛）。
    ///
    /// text trim 后为空或超 [`MAX_TEXT_BYTES`]、申请不存在、非 replied 状态
    /// （前端只在 replied 开放回复框）或记录无 peer 寻址（不重解析名片，复用
    /// 已存 peer 先例同重试路径）时报错。
    pub fn contact_reply_request(
        &mut self,
        request_id: &str,
        text: &str,
    ) -> Result<FriendRequestRecord> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        self.require_unlocked_root_id()?;
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_TEXT_BYTES {
            return Err(KernelError::Internal("回复内容为空或过长".to_string()));
        }
        let now = system_now_ms();
        let Some(record) = ContactService::get_outgoing_request(self.require_storage()?, request_id)?
        else {
            return Err(KernelError::Internal("申请不存在".to_string()));
        };
        if record.status != FriendRequestStatus::Replied {
            return Err(KernelError::Internal("当前状态不可回复".to_string()));
        }
        if record.peer.is_none() {
            return Err(KernelError::Internal("无法确定对方节点地址".to_string()));
        }
        let msg = RequestThreadMessage {
            from: ThreadFrom::Me,
            text: text.to_string(),
            ts: now,
        };
        let record =
            ContactService::append_outgoing_thread(self.require_storage_mut()?, request_id, msg, now)?
                .expect("outgoing request just fetched");
        self.deliver_friend_reply(&record, &record.id, text);
        Ok(record)
    }

    /// 向对方（申请方）主动发起询问（接收方侧的来回回复入口）：本地 inbox
    /// 申请 thread 追加 from=me（status 保持 pending——申请仍待我接受/忽略，
    /// 对方答复经 friend-reply 入站续接 thread），并向对方投递 friend-reply
    /// 信封（重试与无事件语义同 [`Self::contact_reply_request`]）。
    ///
    /// text trim 后为空或超 [`MAX_TEXT_BYTES`]、申请不存在、非 pending 状态
    /// （已 accepted/ignored 的申请不再受理询问）或记录无 peer 寻址（复用已
    /// 存 peer，不重解析名片）时报错。
    pub fn contact_ask_request(
        &mut self,
        request_id: &str,
        text: &str,
    ) -> Result<FriendRequestRecord> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        self.require_unlocked_root_id()?;
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_TEXT_BYTES {
            return Err(KernelError::Internal("询问内容为空或过长".to_string()));
        }
        let now = system_now_ms();
        let Some(record) = ContactService::get_incoming_request(self.require_storage()?, request_id)?
        else {
            return Err(KernelError::Internal("申请不存在".to_string()));
        };
        if record.status != FriendRequestStatus::Pending {
            return Err(KernelError::Internal("当前状态不可询问".to_string()));
        }
        if record.peer.is_none() {
            return Err(KernelError::Internal("无法确定对方节点地址".to_string()));
        }
        let msg = RequestThreadMessage {
            from: ThreadFrom::Me,
            text: text.to_string(),
            ts: now,
        };
        let record =
            ContactService::append_incoming_thread(self.require_storage_mut()?, request_id, msg, now)?
                .expect("incoming request just fetched");
        // 入站申请 id 为复合形式 `{from}:{原 requestId}`；信封必须带原 id——
        // 对方 outbox 按原始 id 落库（同 accept_request_side_effects 的回发处理）
        let original_request_id = record
            .id
            .strip_prefix(&format!("{}:", record.root_id))
            .unwrap_or(&record.id);
        self.deliver_friend_reply(&record, original_request_id, text);
        Ok(record)
    }

    /// 投递 friend-reply 信封（复用记录已存 peer；带退避重试——丢失即静默
    /// 卡死且无失败 UI）。信封构造失败跳过投递，本地 thread 已落。
    fn deliver_friend_reply(&self, record: &FriendRequestRecord, wire_request_id: &str, text: &str) {
        let body = friend_reply_body(wire_request_id, text);
        if let Ok(envelope) = self.build_dm_envelope(KIND_FRIEND_REPLY, &record.root_id, body) {
            let peer = record.peer.as_ref().expect("peer checked by caller");
            let target = PeerNodeInfo {
                peer_id: (!peer.peer_id.is_empty()).then(|| peer.peer_id.clone()),
                addresses: peer.addresses.clone(),
            };
            self.spawn_deliveries_with_retry(vec![(target, envelope)], &DM_RETRY_DELAYS);
        }
    }
}
