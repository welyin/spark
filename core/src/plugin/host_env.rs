//! 插件后台运行时的宿主能力面：内核共享句柄镜像 + capability 分发。
//!
//! 插件线程**不持** `Mutex<Kernel>`——capability 所需句柄（存储镜像、事件
//! 广播、p2p 节点、签名私钥、当前身份、runtime handle）全部为内核既有
//! `Arc` 共享格的克隆，插件线程只经 [`PluginHostShared`] 访问。存储为镜像
//! 格：身份切换换库时内核更新镜像，插件线程每次调用现取（sled 克隆共享
//! 同一底层库，不会持有旧库句柄）。
//!
//! 锁序与 kernel 门面一致：先 `io_lock`（长持，落库互斥），镜像锁只在
//! 读取句柄瞬间持有，二者不构成环。

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::contact::ContactService;
use crate::message::{MessageError, MessageService, generate_message_id};
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode};
use crate::storage::SledStorage;

use super::error::{PluginError, Result};

/// 插件运行时的宿主共享句柄（全部由 kernel 门面的共享格克隆而来）。
///
/// `runtime` 为 kernel tokio runtime 的句柄：能力实现里需要异步投递时
/// spawn 到内核 runtime（插件线程是普通 OS 线程，spawn 安全；不得
/// `block_on`——与 kernel 线程模型口径一致）。
#[derive(Clone)]
pub(crate) struct PluginHostShared {
    /// 当前身份的存储镜像（`open_storage` 回填、`shutdown` 清空）。
    pub(crate) storage: Arc<Mutex<Option<SledStorage>>>,
    /// 存储读写互斥锁（与 kernel 门面同一把）。
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 内核事件广播（bot 回复落库后发 ChatReceived，与真人消息同口径）。
    pub(crate) event_tx: broadcast::Sender<P2pEvent>,
    /// 当前身份 rootId 共享格（= kernel `current_root_id_shared`）。
    pub(crate) my_root_id: Arc<Mutex<Option<String>>>,
    /// p2p 节点句柄共享格（= kernel `p2p_node_shared`）。
    pub(crate) p2p_node: Arc<Mutex<Option<Arc<P2pNode>>>>,
    /// 解锁期签名私钥共享格（= kernel `signing_key_shared`，自设备回同步
    /// 信封自签用）。
    pub(crate) signing_key: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    /// kernel tokio runtime 句柄（投递任务 spawn 目标）。
    pub(crate) runtime: tokio::runtime::Handle,
}

impl PluginHostShared {
    /// 读当前存储镜像（ sled 克隆，共享底层库；未打开返回 StorageNotReady）。
    pub(crate) fn require_storage(&self) -> Result<SledStorage> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(PluginError::StorageNotReady)
    }

    /// host 调用入口（JS `__spark_host_call` 的 Rust 侧）：分发 capability，
    /// 结果/错误统一序列化为 JSON 字符串返回 JS（错误形如 `{"error": "..."}`，
    /// 由 JS prelude 转为异常抛出）。
    pub(crate) fn call(&self, plugin_id: &str, capability: &str, payload_json: &str) -> String {
        match self.dispatch(plugin_id, capability, payload_json) {
            Ok(value) => value.to_string(),
            Err(error) => serde_json::json!({ "error": error.to_string() }).to_string(),
        }
    }

    fn dispatch(&self, plugin_id: &str, capability: &str, payload_json: &str) -> Result<Value> {
        let payload: Value = serde_json::from_str(payload_json)?;
        match capability {
            "log" => {
                let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
                eprintln!("[plugin:{plugin_id}] {message}");
                Ok(Value::Null)
            }
            "contact.ensureBot" => self.ensure_bot(plugin_id, &payload),
            "message.reply" => self.reply(plugin_id, &payload),
            other => Err(PluginError::InvalidCall(format!(
                "unknown capability: {other}"
            ))),
        }
    }

    /// `contact.ensureBot` 能力：注册/刷新本插件的 bot 联系人（出现在通讯录）。
    ///
    /// bot rootId 由内核拼定为 `bot:{pluginId}:{botId}`（插件不可伪造他插件
    /// 的 bot）；`botId` 拒空串与冒号（保 rootId 三段式可解析）。
    fn ensure_bot(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let bot_id = required_str(payload, "botId")?;
        let display_name = required_str(payload, "displayName")?;
        if bot_id.is_empty() || bot_id.contains(':') {
            return Err(PluginError::InvalidCall(format!("invalid botId: {bot_id}")));
        }
        let bot_root_id = format!("bot:{plugin_id}:{bot_id}");
        crate::kernel::ensure_bot_shared(self, &bot_root_id, display_name)?;
        Ok(serde_json::json!({ "botRootId": bot_root_id }))
    }

    /// `message.reply` 能力：向插件**自己**的 bot 会话写入一条 bot 回复。
    ///
    /// bot 身份不从载荷取（防伪造）：由会话落库的权威 `peer_root_id` 推导，
    /// 并强制归属校验——目标会话必须是本插件的 bot 会话
    /// （`bot:{pluginId}:{botId}` 前缀）。
    fn reply(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = required_str(payload, "spaceKey")?;
        let conv_id = required_str(payload, "convId")?;
        let text = required_str(payload, "text")?;
        // 查询类不加 io_lock（kernel 门面同口径）；归属判定后 append 路径
        // 在 bot_reply_shared 内持锁并重新读会话，会话 peer 不可变，无竞态
        let storage = self.require_storage()?;
        let conv = MessageService::get_conversation(&storage, space, conv_id)?
            .ok_or(MessageError::ConversationNotFound)?;
        let prefix = format!("bot:{plugin_id}:");
        if !conv.peer_root_id.starts_with(&prefix) {
            return Err(PluginError::ConversationNotOwned(conv_id.to_string()));
        }
        let bot_root_id = conv.peer_root_id.clone();
        let bot_name = ContactService::get_friend(&storage, &bot_root_id)?
            .map(|friend| friend.nickname)
            .filter(|nickname| !nickname.is_empty())
            .unwrap_or_else(|| bot_root_id.clone());
        let message_id = generate_message_id(system_now_ms());
        let view = crate::kernel::bot_reply_shared(
            self,
            space,
            conv_id,
            &bot_root_id,
            &bot_name,
            &message_id,
            text,
        )?;
        Ok(serde_json::to_value(view)?)
    }
}

/// 取载荷必填字符串字段（缺失/非字符串为非法调用）。
fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::InvalidCall(format!("missing string field: {field}")))
}
