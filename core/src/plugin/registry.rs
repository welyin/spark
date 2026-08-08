//! 插件后台运行时的注册表与会话消息路由。
//!
//! 路由规则（与前端 relay 同口径，防循环）：
//! - 归属判定取 `conversation.peerId` 的 `bot:{pluginId}:{botId}` 前缀
//!   （会话落库时的权威值，不信任消息自报字段）；
//! - 发送者即 bot 本人（`senderId == peerId`，插件自己的回复）不再回投，
//!   否则 echo 类插件会自激刷屏。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::runtime::{PluginEvent, PluginRuntimeHandle};

/// 运行中插件的注册表（kernel 门面与事件路由任务共享）。
#[derive(Clone, Default)]
pub(crate) struct PluginRuntimeRegistry {
    inner: Arc<Mutex<HashMap<String, PluginRuntimeHandle>>>,
}

impl PluginRuntimeRegistry {
    pub(crate) fn register(&self, plugin_id: &str, handle: PluginRuntimeHandle) {
        self.lock().insert(plugin_id.to_string(), handle);
    }

    /// 注销并返回句柄（停机由调用方经句柄发起）。
    pub(crate) fn remove(&self, plugin_id: &str) -> Option<PluginRuntimeHandle> {
        self.lock().remove(plugin_id)
    }

    /// 按世代注销（插件线程退出自清理）：仅当注册表中仍是该世代的句柄时
    /// 移除——崩溃线程不得误删同名插件新一轮启动的句柄。
    pub(crate) fn remove_if_generation(&self, plugin_id: &str, generation: u64) {
        let mut inner = self.lock();
        if inner.get(plugin_id).is_some_and(|h| h.generation == generation) {
            inner.remove(plugin_id);
        }
    }

    pub(crate) fn is_running(&self, plugin_id: &str) -> bool {
        self.lock().contains_key(plugin_id)
    }

    pub(crate) fn running_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// 把会话消息载荷路由给归属插件的后台运行时（无归属/无运行时静默丢弃；
    /// 插件线程已崩溃则注销残柄，不阻挡下次启动）。
    ///
    /// `payload` 为 ChatReceived 同构结构：`{spaceKey, conversation, message}`。
    pub(crate) fn dispatch_chat(&self, payload: &Value) {
        let Some(plugin_id) = bot_owner_plugin_id(payload) else {
            return;
        };
        let mut inner = self.lock();
        let Some(handle) = inner.get(plugin_id) else {
            return;
        };
        if handle.dispatch(PluginEvent::Chat(payload.clone())).is_err() {
            inner.remove(plugin_id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, PluginRuntimeHandle>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 从消息载荷解析归属插件 id：bot 会话且发送者非 bot 本人时返回 pluginId。
fn bot_owner_plugin_id(payload: &Value) -> Option<&str> {
    let peer = payload.pointer("/conversation/peerId")?.as_str()?;
    let rest = peer.strip_prefix("bot:")?;
    let (plugin_id, bot_id) = rest.split_once(':')?;
    if plugin_id.is_empty() || bot_id.is_empty() {
        return None;
    }
    let sender = payload
        .pointer("/message/senderId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sender == peer {
        return None;
    }
    Some(plugin_id)
}

/// 插件 id 白名单（与前端 PLUGIN_ID_PATTERN 一致；仓库锚定 repo id 形态的
/// 后台运行时支持待 manifest 加载阶段处理）。
pub(crate) fn is_valid_plugin_id(plugin_id: &str) -> bool {
    !plugin_id.is_empty()
        && plugin_id.len() <= 64
        && !plugin_id.starts_with('-')
        && plugin_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chat_payload(peer_id: &str, sender_id: &str) -> Value {
        json!({
            "spaceKey": "personal",
            "conversation": { "id": format!("dm:{peer_id}"), "peerId": peer_id },
            "message": { "senderId": sender_id, "content": "hi" }
        })
    }

    #[test]
    fn routes_user_message_to_owner_plugin() {
        assert_eq!(
            bot_owner_plugin_id(&chat_payload("bot:ai-chat:helper", "me")),
            Some("ai-chat")
        );
    }

    #[test]
    fn skips_bot_own_message_anti_loop() {
        assert_eq!(
            bot_owner_plugin_id(&chat_payload("bot:ai-chat:helper", "bot:ai-chat:helper")),
            None
        );
    }

    #[test]
    fn skips_non_bot_conversation() {
        assert_eq!(bot_owner_plugin_id(&chat_payload("abc123", "abc123")), None);
        assert_eq!(bot_owner_plugin_id(&chat_payload("bot:no-bot-id", "me")), None);
        assert_eq!(bot_owner_plugin_id(&chat_payload("bot:", "me")), None);
    }

    #[test]
    fn dispatch_to_unknown_plugin_is_noop() {
        let registry = PluginRuntimeRegistry::default();
        registry.dispatch_chat(&chat_payload("bot:ghost:helper", "me"));
        assert!(registry.running_ids().is_empty());
    }

    #[test]
    fn plugin_id_whitelist() {
        assert!(is_valid_plugin_id("ai-chat"));
        assert!(is_valid_plugin_id("a"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("-lead-hyphen"));
        assert!(!is_valid_plugin_id("Upper"));
        assert!(!is_valid_plugin_id("with/slash"));
        assert!(!is_valid_plugin_id(&"x".repeat(65)));
    }
}
