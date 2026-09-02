//! 会话/消息本地状态变更（薄封装）：删除、已读、草稿、置顶、免打扰、
//! 清空、删会话。个人空间的会话元数据变更后向自设备广播 conv-sync 快照
//! （[`Kernel::broadcast_conv_sync`]）；read 信封按会话对象走对端
//! [`Kernel::notify_peer`] 或自设备 [`Kernel::deliver_to_devices`]。

use super::super::dm_envelope::KIND_READ;
use super::{Kernel, Result};
use crate::message::MessageService;
use crate::message::types::ConversationKind;
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 删除单条消息（仅本地）。
    pub fn message_delete(&mut self, space: &str, conv_id: &str, message_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::delete_message(
            self.require_storage_raw_mut()?,
            space,
            conv_id,
            message_id,
        )?;
        Ok(())
    }

    /// 清零会话未读；direct 会话且对端可达时发 read 信封（尽力而为）。
    /// 自己的会话改为向所有已配对设备发 read 信封（已读状态跨设备同步）。
    pub fn message_mark_read(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::mark_read(self.require_storage_raw_mut()?, space, conv_id)?;
        if let Some(conv) =
            MessageService::get_conversation(self.require_storage()?, space, conv_id)?
            && conv.kind == ConversationKind::Direct
        {
            let body = serde_json::json!({ "spaceKey": space });
            let my_root_id = self.current_root_id().ok().flatten();
            if my_root_id.as_deref() == Some(conv.peer_root_id.as_str()) {
                self.deliver_to_devices(&conv.peer_root_id, KIND_READ, body);
            } else {
                self.notify_peer(space, &conv, KIND_READ, body);
            }
        }
        Ok(())
    }

    /// 写入会话草稿。个人空间变更后向自设备广播 conv-sync 快照。
    pub fn message_set_draft(&mut self, space: &str, conv_id: &str, draft: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        // 元数据写经版本化句柄：自动记账 + 触发变更信号（即时 hello）
        MessageService::set_draft_pdsync(
            self.require_storage_mut()?,
            space,
            conv_id,
            draft,
            now,
            &node_id,
        )?;
        if space == "personal" {
            self.broadcast_conv_sync();
        }
        Ok(())
    }

    /// 切换会话置顶。
    pub fn message_toggle_pin(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        MessageService::toggle_pin_pdsync(
            self.require_storage_mut()?,
            space,
            conv_id,
            now,
            &node_id,
        )?;
        if space == "personal" {
            self.broadcast_conv_sync();
        }
        Ok(())
    }

    /// 切换会话免打扰。
    pub fn message_toggle_mute(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        MessageService::toggle_mute_pdsync(
            self.require_storage_mut()?,
            space,
            conv_id,
            now,
            &node_id,
        )?;
        if space == "personal" {
            self.broadcast_conv_sync();
        }
        Ok(())
    }

    /// 清空会话聊天记录（保留会话入口）。
    pub fn message_clear(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        MessageService::clear_messages(self.require_storage_raw_mut()?, space, conv_id)?;
        Ok(())
    }

    /// 删除会话（会话与消息一并删除；个人空间写 tombstone pmeta，删除随
    /// 自设备 pdsync 传播）。
    pub fn message_delete_conversation(&mut self, space: &str, conv_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let node_id = self.sync_node_id();
        // 版本化句柄：conv 键受管 → 墓碑 + 删除日志 + 变更信号自动完成
        // （消息键 msg:item 不受管，裸删透传）
        MessageService::delete_conversation_pdsync(
            self.require_storage_mut()?,
            space,
            conv_id,
            system_now_ms(),
            Some(&node_id),
        )?;
        Ok(())
    }
}
