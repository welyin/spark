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

use std::collections::HashMap;
use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::collection::CollectionConfig;
use crate::contact::ContactService;
use crate::message::{MessageError, MessageService, generate_message_id};
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode};

use super::error::{PluginError, Result};
use super::runtime::PluginEvent;

/// 插件运行时的宿主共享句柄（全部由 kernel 门面的共享格克隆而来）。
///
/// `runtime` 为 kernel tokio runtime 的句柄：能力实现里需要异步投递时
/// spawn 到内核 runtime（插件线程是普通 OS 线程，spawn 安全；不得
/// `block_on`——与 kernel 线程模型口径一致）。
#[derive(Clone)]
pub(crate) struct PluginHostShared {
    /// 当前身份的存储镜像（`open_storage` 回填、`shutdown` 清空）。
    /// P6 起指向**版本化句柄**（写库即同步）：插件数据（`pdoc:`/`pdecl:`）
    /// 经中间件自动完成 pdsync 记账；存量 `doc:`/`idx:`/`meta:` 等键不在
    /// 受管前缀内，原样透传（行为不变）。
    pub(crate) storage: Arc<Mutex<Option<crate::kernel::KernelStorage>>>,
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
    /// 解锁期 BIP39 种子（= kernel `seed_shared`；lock 时清除）。
    pub(crate) seed_shared: Arc<Mutex<Option<[u8; 64]>>>,
    /// 集合配置缓存（= kernel `collection_configs`；docs.put/delete/query 时
    /// 写入兜底声明，已持久化的集合声明优先）。
    pub(crate) collection_configs: Arc<Mutex<HashMap<(String, String), CollectionConfig>>>,
    /// 宿主查询在途表（query_id → 应答通道；`plugin_host_query` 插入，
    /// JS 侧 `query.respond` 回流取出）。
    pub(crate) pending_queries: Arc<Mutex<HashMap<u64, StdSender<Value>>>>,
    /// filtered 集合权限钩子注册表（O3 工作项 2）：collection 名（`name@v` 之前的
    /// name）→ 注册的过滤种类（"read"/"write"/"read-write"）。由插件 prelude
    /// 调 `data.onReadFilter`/`data.onWriteFilter` 时更新——数据账号侧 orgq-req
    /// 的 host 钩子据此判定该集合能否服务（插件未运行即无条目 → fail-closed）。
    pub(crate) filter_caps: Arc<Mutex<HashMap<String, String>>>,
    /// feed.deliver 调用级限流器（内核单点，social-feed §9.3）：每 (space,
    /// pluginId) 60s 内 10 次。Kernel 门面（iframe 桥经命令层）与 QuickJS 后台
    /// capability 共享同一实例——桥侧不重复做限流。Arc<Mutex> 因本格经
    /// `&self` 访问（与既有 filter_caps 同构）。
    pub(crate) feed_limiter: Arc<Mutex<crate::kernel::FeedDeliverRateLimiter>>,
    /// 应用消息限流器（内核单点，p2p-messages.md §20.5）：每 (space, pluginId)
    /// 60s 内 10 条。`messages.sendAppMessage` 后台 capability 与 Kernel 门面
    /// `message_app_send` 共享同一实例——桥侧不重复做限流。
    pub(crate) app_msg_limiter: Arc<Mutex<crate::message::AppMessageRateLimiter>>,
    /// kernel tokio runtime 句柄（投递任务 spawn 目标）。
    pub(crate) runtime: tokio::runtime::Handle,
}

/// 单插件运行时上下文（每次启动绑定；与跨插件共享的 [`PluginHostShared`]
/// 相对——事件回流通道是每插件一条）。
#[derive(Clone)]
pub(crate) struct PluginRuntimeContext {
    pub(crate) plugin_id: String,
    pub(crate) event_tx: std::sync::mpsc::Sender<PluginEvent>,
    /// 安装时授权的权限清单（市场状态 grantedPermissions 快照：基础权限
    /// 恒在列，高级权限须 manifest 声明并授权——与桥 dispatcher 的数据源
    /// 同格；capability 分发按 [`capability_permission`] 逐调用强制）。
    pub(crate) permissions: Vec<String>,
}

mod data;
mod docs;
mod misc;
mod online;
mod sys;

impl PluginHostShared {
    /// 该集合是否注册了指定种类的过滤钩子（数据账号侧 orgq-req 服务判定）。
    /// 插件未运行即无条目 → false（fail-closed）。`kind` ∈ {"read","write"}。
    pub(crate) fn has_filter(&self, collection: &str, kind: &str) -> bool {
        self.filter_caps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(collection)
            .is_some_and(|caps| caps.contains(kind))
    }

    /// 读当前存储镜像（版本化句柄克隆，共享底层库与 node_id 格；未打开返回
    /// StorageNotReady）。
    pub(crate) fn require_storage(&self) -> Result<crate::kernel::KernelStorage> {
        self.storage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(PluginError::StorageNotReady)
    }

    /// host 调用入口（JS `__spark_host_call` 的 Rust 侧）：分发 capability，
    /// 结果/错误统一序列化为 JSON 字符串返回 JS（错误形如 `{"error": "..."}`，
    /// 由 JS prelude 转为异常抛出）。
    pub(crate) fn call(
        &self,
        rtx: &PluginRuntimeContext,
        capability: &str,
        payload_json: &str,
    ) -> String {
        match self.dispatch(rtx, capability, payload_json) {
            Ok(value) => value.to_string(),
            Err(error) => serde_json::json!({ "error": error.to_string() }).to_string(),
        }
    }

    fn dispatch(
        &self,
        rtx: &PluginRuntimeContext,
        capability: &str,
        payload_json: &str,
    ) -> Result<Value> {
        let plugin_id = rtx.plugin_id.as_str();
        let payload: Value = serde_json::from_str(payload_json)?;
        if capability.starts_with("message.replyStream") || capability == "sys.fetchStream.start" {
            eprintln!("[stream-dbg] host call {capability} plugin={plugin_id}");
        }
        // 权限强制（对齐桥 dispatcher 的 CALL_PERMISSIONS 中间件）：未授权
        // 以错误回流 JS——同步能力由 prelude 抛异常、sys.* 经 startAsync
        // 转为 Promise 拒绝；不 panic、不终止插件线程
        if let Some(required) = capability_permission(capability) {
            if !rtx.permissions.iter().any(|p| p == required) {
                return Err(PluginError::PermissionDenied(format!(
                    "permission \"{required}\" is not granted for plugin {plugin_id}"
                )));
            }
        }
        match capability {
            "log" => {
                let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
                eprintln!("[plugin:{plugin_id}] {message}");
                Ok(Value::Null)
            }
            "contact.ensureBot" => self.ensure_bot(plugin_id, &payload),
            "message.reply" => self.reply(plugin_id, &payload),
            // 文档能力：域恒为插件 id（插件不可读写他域数据）；同步 sled 操作
            "docs.get" => self.doc_get(plugin_id, &payload),
            "docs.put" => self.doc_put(plugin_id, &payload),
            "docs.delete" => self.doc_delete(plugin_id, &payload),
            "docs.query" => self.doc_query(plugin_id, &payload),
            "docs.defineCollection" => self.doc_define_collection(plugin_id, &payload),
            // P6 声明式数据 API（personal scope）：声明一次 + 读写零同步参数
            "data.declareCollection" => self.data_declare_collection(plugin_id, &payload),
            // O3 filtered 权限钩子注册（prelude onReadFilter/onWriteFilter 调）：
            // 记录该集合注册的过滤种类到 filter_caps——数据账号侧 orgq-req 据此
            // 判定能否服务（插件未运行 = 无条目 = fail-closed 只存不服务）。
            "data.onReadFilter" => self.data_on_read_filter(plugin_id, &payload),
            "data.onWriteFilter" => self.data_on_write_filter(plugin_id, &payload),
            "data.save" => self.data_save(plugin_id, &payload),
            "data.delete" => self.data_delete(plugin_id, &payload),
            "data.get" => self.data_get(plugin_id, &payload),
            "data.query" => self.data_query(plugin_id, &payload),
            "data.dropVersion" => self.data_drop_version(plugin_id, &payload),
            // batch3 §3：在线 orgq 查询/写入（增量 ops；异步配对模式同
            // sys.exec.start——启动即返，结果经 data-online-result 回流）
            "data.onlineGet" => self.data_online_get(rtx, &payload),
            "data.onlineQuery" => self.data_online_query(rtx, &payload),
            "data.onlineSave" => self.data_online_save(rtx, &payload),
            "data.onlineDelete" => self.data_online_delete(rtx, &payload),
            // 内建 blob：内容哈希寻址；拉取（eager/lazy 调和）由 pdsync 链路完成
            "data.saveBlob" => self.data_save_blob(plugin_id, &payload),
            "data.readBlob" => self.data_read_blob(plugin_id, &payload),
            // 系统能力：长时操作异步化——启动即返，结果经事件队列回流
            // （JS Promise 由 prelude 配对 callId）
            "sys.exec.start" => self.sys_exec_start(rtx, &payload),
            "sys.execStream.start" => self.sys_exec_stream_start(rtx, &payload),
            "sys.fetch.start" => self.sys_fetch_start(rtx, &payload),
            "sys.fetchStream.start" => self.sys_fetch_stream_start(rtx, &payload),
            // 流式回复（主聊天窗口 AI 逐字上屏）：start 落占位 → chunk 追加 →
            // end 收尾；归属校验与 message.reply 同口径（本插件 bot 会话）。
            "message.replyStreamStart" => self.reply_stream_start(plugin_id, &payload),
            "message.replyStreamChunk" => self.reply_stream_chunk(plugin_id, &payload),
            "message.replyStreamEnd" => self.reply_stream_end(plugin_id, &payload),
            // 宿主查询应答回流（plugin_host_query 的另一半）
            "query.respond" => self.query_respond(&payload),
            // 社交定向投递（social-feed §9.1 spark.feed）：deliver 权限在
            // capability_permission 强制（feed:deliver）+ 出站 topic 前缀校验；
            // pull 接收侧免权限。插件身份用运行时绑定 plugin_id，不信 JS 自报。
            "feed.deliver" => self.feed_deliver(plugin_id, &payload),
            "feed.pull" => self.feed_pull(plugin_id, &payload),
            // 身份验签（纯函数，免状态）与域身份签名（identity:sign 高级权限
            // 使用时询问——对齐桥 dispatcher 口径）。sign 域缺省 = 插件根域
            // `plugin:{pluginId}`，不信 JS 自报（防越权签其它域）。
            "identity.verify" => self.identity_verify(plugin_id, &payload),
            "identity.sign" => self.identity_sign(plugin_id, &payload),
            // 互动通知写应用会话（§20）：`app:{pluginId}` 会话由运行时绑定
            // plugin_id 确定性派生，不信 JS 自报（归属不变量 §20.4）；限流在
            // 共享的 app_msg_limiter 内强制（与 Kernel 门面同实例）。
            "messages.sendAppMessage" => self.messages_send_app_message(plugin_id, &payload),
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

    /// `message.replyStreamStart`：流式回复开始——归属校验后落 streaming 占位
    /// 消息并广播，返回消息 id 供 chunk/end 按 id 定位。
    fn reply_stream_start(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = required_str(payload, "spaceKey")?;
        let conv_id = required_str(payload, "convId")?;
        eprintln!(
            "[stream-dbg] replyStreamStart enter plugin={plugin_id} space={space} conv={conv_id}"
        );
        let (bot_root_id, bot_name) =
            crate::kernel::require_owned_bot_conv(self, plugin_id, space, conv_id)?;
        eprintln!("[stream-dbg] replyStreamStart owned bot={bot_root_id}");
        let message_id = crate::kernel::bot_reply_stream_start_shared(
            self,
            space,
            conv_id,
            &bot_root_id,
            &bot_name,
        )?;
        eprintln!("[stream-dbg] replyStreamStart done messageId={message_id}");
        Ok(serde_json::json!({ "messageId": message_id }))
    }

    /// `message.replyStreamChunk`：追加一段流式文本（重发 ChatReceived 逐字上屏）。
    fn reply_stream_chunk(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = required_str(payload, "spaceKey")?;
        let conv_id = required_str(payload, "convId")?;
        let message_id = required_str(payload, "messageId")?;
        let text = required_str(payload, "text")?;
        let (bot_root_id, bot_name) =
            crate::kernel::require_owned_bot_conv(self, plugin_id, space, conv_id)?;
        crate::kernel::bot_reply_stream_chunk_shared(
            self,
            space,
            conv_id,
            &bot_root_id,
            &bot_name,
            message_id,
            text,
        )?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// `message.replyStreamEnd`：流式回复终态（delivered/failed），并按普通
    /// reply 口径补一次自设备回同步（中间态不扩散，终态定稿后同步）。
    fn reply_stream_end(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = required_str(payload, "spaceKey")?;
        let conv_id = required_str(payload, "convId")?;
        let message_id = required_str(payload, "messageId")?;
        let error = payload.get("error").and_then(Value::as_str);
        let (bot_root_id, bot_name) =
            crate::kernel::require_owned_bot_conv(self, plugin_id, space, conv_id)?;
        crate::kernel::bot_reply_stream_end_shared(
            self,
            space,
            conv_id,
            &bot_root_id,
            &bot_name,
            message_id,
            error,
        )?;
        // 终态定稿后回同步自设备（与 bot_reply_shared 的 deliver_to_devices 同口径）
        if let Some(my_root_id) = self
            .my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let storage = self.require_storage()?;
            if let Some(msg) = MessageService::get_message(&storage, space, conv_id, message_id)? {
                let msg_view = crate::kernel::message_view(&msg, Some("__bot_sender__"));
                self.deliver_to_devices(
                    &my_root_id,
                    crate::kernel::dm_envelope::KIND_CHAT,
                    serde_json::json!({
                        "spaceKey": space,
                        "convId": conv_id,
                        "message": msg_view
                    }),
                );
            }
        }
        Ok(serde_json::json!({ "ok": true }))
    }
}

/// capability → 所需权限（逐字对齐壳层桥 dispatcher 的 CALL_PERMISSIONS
/// 映射；内核侧 capability 名对应桥的 callKey：contact.ensureBot /
/// message.reply 对应桥的 messages.registerAsContact / sendResponse）。
/// 不在表内 = 免权限基础调用（log、query.respond）。
fn capability_permission(capability: &str) -> Option<&'static str> {
    match capability {
        "docs.get" | "docs.query" => Some("storage:read"),
        "docs.put" | "docs.delete" | "docs.defineCollection" => Some("storage:write"),
        "data.get" | "data.query" | "data.readBlob" => Some("storage:read"),
        // batch3 §3：在线 ops 沿用 storage:read/write 权限轴
        "data.onlineGet" | "data.onlineQuery" => Some("storage:read"),
        "data.save"
        | "data.delete"
        | "data.declareCollection"
        | "data.dropVersion"
        | "data.saveBlob" => Some("storage:write"),
        "data.onlineSave" | "data.onlineDelete" => Some("storage:write"),
        "contact.ensureBot" | "message.reply" => Some("message:app"),
        "sys.exec.start" | "sys.execStream.start" => Some("system:exec"),
        "sys.fetch.start" | "sys.fetchStream.start" => Some("network:fetch"),
        "message.replyStreamStart" | "message.replyStreamChunk" | "message.replyStreamEnd" => {
            Some("message:app")
        }
        // 社交定向投递（social-feed §9.3）：deliver 需 feed:deliver（高级 + 内核
        // 限流）；pull 接收侧免权限——不在本表即放行。
        "feed.deliver" => Some("feed:deliver"),
        // 身份验签（基础权限 identity:verify，免使用时询问——对齐桥 dispatcher
        // 的 message-card 视图白名单口径）；域身份签名 identity:sign 高级 + 使用时
        // 询问（对齐桥 CALL_PERMISSIONS）。
        "identity.verify" => Some("identity:verify"),
        "identity.sign" => Some("identity:sign"),
        // 互动通知写应用会话（高级 + 内核限流，§20.5）。
        "messages.sendAppMessage" => Some("message:app"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    /// 最小宿主面：无存储（权限前置拒绝在 dispatch 权限过滤即拦，不触存储）。
    fn bare_host() -> PluginHostShared {
        PluginHostShared {
            storage: Arc::new(Mutex::new(None)),
            io_lock: Arc::new(Mutex::new(())),
            event_tx: tokio::sync::broadcast::channel(16).0,
            my_root_id: Arc::new(Mutex::new(None)),
            p2p_node: Arc::new(Mutex::new(None)),
            signing_key: Arc::new(Mutex::new(None)),
            seed_shared: Arc::new(Mutex::new(None)),
            collection_configs: Arc::new(Mutex::new(Default::default())),
            pending_queries: Arc::new(Mutex::new(Default::default())),
            filter_caps: Arc::new(Mutex::new(Default::default())),
            feed_limiter: Arc::new(Mutex::new(Default::default())),
            app_msg_limiter: Arc::new(Mutex::new(Default::default())),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// batch3 §3：在线 ops 权限映射——读归 storage:read、写归 storage:write
    ///（与既有 data.* 同轴；未授权插件调用被 dispatch 前置拒绝）。
    #[test]
    fn online_ops_permission_mapping() {
        for cap in ["data.onlineGet", "data.onlineQuery"] {
            assert_eq!(capability_permission(cap), Some("storage:read"), "{cap}");
        }
        for cap in ["data.onlineSave", "data.onlineDelete"] {
            assert_eq!(capability_permission(cap), Some("storage:write"), "{cap}");
        }
    }

    /// S9：feed.deliver 归入 feed:deliver 权限；feed.pull 接收侧免权限（不在
    /// 表内放行）。与桥 dispatcher 的 CALL_PERMISSIONS 逐字对齐（social-feed §9.3）。
    #[test]
    fn feed_deliver_requires_feed_deliver_pull_exempt() {
        assert_eq!(capability_permission("feed.deliver"), Some("feed:deliver"));
        assert_eq!(
            capability_permission("feed.pull"),
            None,
            "pull 接收侧免权限——不在表内即放行"
        );
    }

    /// S9 补：identity.verify 归入基础权限 identity:verify；identity.sign 归入
    /// 高级 identity:sign；messages.sendAppMessage 归入 message:app（高级 + 内核
    /// 限流）。与桥 dispatcher 的 CALL_PERMISSIONS / 基础权限口径逐字对齐。
    #[tokio::test]
    async fn identity_and_app_message_permission_mapping() {
        assert_eq!(
            capability_permission("identity.verify"),
            Some("identity:verify"),
            "verify 归入基础权限 identity:verify（免使用时询问）"
        );
        assert_eq!(
            capability_permission("identity.sign"),
            Some("identity:sign"),
            "sign 归入高级权限 identity:sign（使用时询问）"
        );
        assert_eq!(
            capability_permission("messages.sendAppMessage"),
            Some("message:app"),
            "sendAppMessage 归入 message:app（高级 + 限流）"
        );
        // 零权限（空 permissions）→ 前置强制拒绝（dispatch 的权限过滤在 call()
        // 前置；此处验映射存在且未授权即拒）。
        let rtx = PluginRuntimeContext {
            plugin_id: "test".to_string(),
            event_tx: std::sync::mpsc::channel().0,
            permissions: Vec::new(),
        };
        let host = bare_host();
        let err = host.call(&rtx, "identity.sign", r#"{"payload":"p"}"#);
        assert!(
            err.contains("identity:sign") && err.contains("not granted"),
            "未授权 identity:sign 应拒绝：{err}"
        );
        let err = host.call(&rtx, "messages.sendAppMessage", r#"{"summary":"x"}"#);
        assert!(
            err.contains("message:app") && err.contains("not granted"),
            "未授权 message:app 应拒绝：{err}"
        );
    }
}

/// 取载荷必填字符串字段（缺失/非字符串为非法调用）。
fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::InvalidCall(format!("missing string field: {field}")))
}

/// 取载荷必填 callId/queryId（u64）。
fn required_call_id(payload: &Value) -> Result<u64> {
    payload
        .get("callId")
        .or_else(|| payload.get("queryId"))
        .and_then(Value::as_u64)
        .ok_or_else(|| PluginError::InvalidCall("missing u64 field: callId".to_string()))
}
