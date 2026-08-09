//! 应用消息（服务号模型，p2p-messages.md §20）：本地生成、本地消费——
//! 无 peer 投递、无 delivered 语义；写入带限流与校验链，查询/已读/删除
//! 与人际会话口径一致。

use super::{AppMessageView, Kernel, Result, app_conversation_id, app_message_view};
use crate::message::{
    AppMessageCard, AppMessageService, MessageError, MessageService, generate_message_id,
};
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 写入应用消息（本地生成、本地消费；无 peer 投递、无 delivered）。
    /// 校验链（按序）：身份解锁 → pluginId 字符集 → payload.summary 非空且
    /// ≤200 字符 → 限流（每 (space, pluginId) 固定窗口 60s 10 条，超限报
    /// [`MessageError::RateLimited`] 并累计拒绝计数）；校验先于限流，
    /// 非法消息不消耗配额。
    /// 内置 system 会话（壳层系统通知写入方，pluginId = "system"）豁免限流：
    /// 限流防的是插件刷会话（§20.5），system 为壳层可信写入方，安装/升级
    /// 等系统通知不应被配额挤掉（豁免不累计拒绝计数）。
    /// 会话由 pluginId 确定性派生（`app:{pluginId}`）并惰性创建——插件只能
    /// 写自己的会话（§20.4 不变量 2）；写入成功未读 +1，状态恒 `local`。
    pub fn message_app_send(
        &mut self,
        space: &str,
        plugin_id: &str,
        payload: serde_json::Value,
        card: Option<AppMessageCard>,
    ) -> Result<AppMessageView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        self.require_unlocked_root_id()?;
        let now = system_now_ms();
        let record = AppMessageService::build_app_message(
            plugin_id,
            payload,
            card,
            generate_message_id(now),
            now,
        )?;
        if plugin_id != "system" && !self.app_msg_limiter.check(space, plugin_id, now) {
            return Err(MessageError::RateLimited.into());
        }
        AppMessageService::ensure_app_conversation(self.require_storage_mut()?, space, plugin_id, now)?;
        AppMessageService::append_app_message(self.require_storage_mut()?, space, &record)?;
        // 应用会话壳纳入 pdsync（仅个人空间生效）：msg:app 消息本体走窗口
        // 同步，会话壳走 msg:conv 类目——bump conv pmeta（ts 保持
        // meta_updated_at，与人消息 append 路径同一助手）
        let node_id = self.sync_node_id();
        MessageService::bump_conv_pmeta_for_message(
            self.require_storage_mut()?,
            space,
            &app_conversation_id(plugin_id),
            now,
            &node_id,
        )?;
        Ok(app_message_view(&record))
    }

    /// 应用会话消息列表（时间升序）。
    pub fn message_app_list(&self, space: &str, plugin_id: &str) -> Result<Vec<AppMessageView>> {
        let messages = AppMessageService::list_app_messages(self.require_storage()?, space, plugin_id)?;
        Ok(messages.iter().map(app_message_view).collect())
    }

    /// 清零应用会话未读并把会话内未读消息批量置已读（语义与人际会话一致）。
    pub fn message_app_mark_read(&mut self, space: &str, plugin_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        AppMessageService::mark_app_read(self.require_storage_mut()?, space, plugin_id)?;
        Ok(())
    }

    /// 删除应用会话（会话与全部应用消息一并删除）。
    pub fn message_app_delete_conversation(&mut self, space: &str, plugin_id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        AppMessageService::delete_app_conversation(self.require_storage_mut()?, space, plugin_id)?;
        Ok(())
    }

    /// 指定应用会话的限流累计拒绝数（熔断观测面；内存态，重启清零）。
    pub fn message_app_rate_rejected(&self, space: &str, plugin_id: &str) -> u64 {
        self.app_msg_limiter.rejected_count(space, plugin_id)
    }
}
