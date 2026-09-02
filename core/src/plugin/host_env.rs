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

use crate::collection::{
    CollectionConfig, DocumentCollection, FilterOp, QueryFilter, QueryOptions,
};
use crate::contact::ContactService;
use crate::message::{MessageError, MessageService, generate_message_id};
use crate::p2p::constants::SYNC_TOPIC;
use crate::p2p::node::system_now_ms;
use crate::p2p::{P2pEvent, P2pNode, build_delete_body, build_update_body};
use crate::schema::{CollectionSchemaDeclaration, SyncStrategy, declare_collection_schema};

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
    /// 解锁期 BIP39 种子（O4 grantAccess/revokeAccess 组织域身份派生用，=
    /// kernel `seed_shared`；lock 时清除）。
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
            // O4 插件 API 访问控制（encrypted 授权名单，§5.2）：owner 维护名单，
            // 内核按 owner 验签（无额外权限项）。data_access 方法在
            // host_env_access 模块。
            "data.grantAccess" => self.data_grant_access(plugin_id, &payload),
            "data.revokeAccess" => self.data_revoke_access(plugin_id, &payload),
            "data.listAccess" => self.data_list_access(plugin_id, &payload),
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
        "data.save"
        | "data.delete"
        | "data.declareCollection"
        | "data.dropVersion"
        | "data.saveBlob" => Some("storage:write"),
        // R2：encrypted 授权名单三方法归入 storage:write（grant/revoke 落 acl
        // + 轮换密钥，list 只读但同属 encrypted 能力面——owner 侧管控）。
        "data.grantAccess" | "data.revokeAccess" | "data.listAccess" => Some("storage:write"),
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

    /// R2：encrypted 授权名单三方法已纳入权限映射（storage:write）——
    /// 零权限插件（无 storage:write）调用 grantAccess 等将因
    /// `capability_permission` 返回 Some 而由 dispatch 前置强制拒绝。
    #[test]
    fn access_api_requires_storage_write_permission() {
        for cap in ["data.grantAccess", "data.revokeAccess", "data.listAccess"] {
            assert_eq!(
                capability_permission(cap),
                Some("storage:write"),
                "{cap} 应归入 storage:write 权限"
            );
        }
        // 与桥 dispatcher 的 CALL_PERMISSIONS 逐字对齐（R2：两侧一致）
        for cap in ["data.grantAccess", "data.revokeAccess", "data.listAccess"] {
            // 零权限（空 permissions）→ 前置拒绝（此处仅验映射存在；dispatch
            // 的权限过滤在 call() 前置，permissions 不含 storage:write 即拒）。
            let required = capability_permission(cap).expect("映射存在");
            assert_eq!(required, "storage:write");
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

// ------------------------------------------------------------------
// 文档能力（docs.*）：域缺省为插件 id；显式域经合法性约束（见
// resolve_doc_domain）。语义对齐 kernel doc_* 门面
// ------------------------------------------------------------------

/// docs 能力的访问别：空间根域按只读收窄（见 resolve_doc_domain）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocAccess {
    Read,
    Write,
}

/// 空间根域（`space:personal` / `space:org`）放开只读的遗留集合白名单：
/// 旧 UI 桥的历史缺陷把 bot 文档沉在空间根域（ai-chat botDataSpaces 的
/// 兜底读只涉及这些集合），仅放行读；空间根域的写一律拒绝——共享空间
/// 数据面不属任一插件，放开写即可伪造他方配置（如 ai_chat_bots 的
/// cliPath 指向恶意二进制）。
const SPACE_DOMAIN_READABLE_COLLECTIONS: [&str; 1] = ["ai_chat_bots"];

/// 解析 docs 能力的目标域：缺省（null/空）为插件自身域；显式指定的域必须
/// 属于本插件的数据面——自身域、`plugin:{pluginId}` 根域（UI 桥历史数据面，
/// 存量 bot 文档沉在那里）可读写；空间根域（`space:personal` / `space:org`）
/// 仅限 [`SPACE_DOMAIN_READABLE_COLLECTIONS`] 遗留集合的只读。其余域
/// （其他插件 id / 组织域等）拒绝——插件不可读写他方数据。
fn resolve_doc_domain<'a>(
    plugin_id: &'a str,
    payload: &'a Value,
    access: DocAccess,
) -> Result<&'a str> {
    let plugin_root = format!("plugin:{plugin_id}");
    match payload.get("domain").and_then(Value::as_str) {
        None | Some("") => Ok(plugin_id),
        Some(domain) if domain == plugin_id => Ok(plugin_id),
        Some(domain) if domain == plugin_root => Ok(domain),
        Some(domain) if domain == "space:personal" || domain == "space:org" => {
            let collection = payload
                .get("collection")
                .and_then(Value::as_str)
                .unwrap_or("");
            if access == DocAccess::Read && SPACE_DOMAIN_READABLE_COLLECTIONS.contains(&collection)
            {
                Ok(domain)
            } else {
                Err(PluginError::InvalidCall(format!(
                    "domain not allowed for plugin {plugin_id}: {domain}"
                )))
            }
        }
        Some(domain) => Err(PluginError::InvalidCall(format!(
            "domain not allowed for plugin {plugin_id}: {domain}"
        ))),
    }
}

impl PluginHostShared {
    /// 集合配置载荷（camelCase，对齐壳层 CollectionConfigDto）。
    fn parse_config(payload: &Value) -> Result<CollectionConfig> {
        let sync_strategy = match payload.get("syncStrategy").and_then(Value::as_str) {
            None => None,
            Some("append-only") => Some(SyncStrategy::AppendOnly),
            Some("lww") => Some(SyncStrategy::Lww),
            Some(other) => {
                return Err(PluginError::InvalidCall(format!(
                    "syncStrategy must be 'append-only' or 'lww', got {other:?}"
                )));
            }
        };
        let indexed_fields = payload
            .get("indexedFields")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(CollectionConfig {
            indexed_fields,
            enable_evidence: payload.get("enableEvidence").and_then(Value::as_bool),
            sync_strategy,
            governance: payload.get("governance").and_then(Value::as_bool),
        })
    }

    /// 本地写入节点 id：p2p 运行中为 peerId，否则 `local-node`（对齐内核门面）。
    pub(crate) fn sync_node_id(&self) -> String {
        self.p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| "local-node".to_string())
    }

    /// 集合配置缓存写入（对齐 `Kernel::make_collection`）。
    fn remember_collection_config(
        &self,
        domain: &str,
        collection: &str,
        config: &CollectionConfig,
    ) {
        self.collection_configs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((domain.to_string(), collection.to_string()), config.clone());
    }

    /// 广播同步消息：p2p 未启动直接跳过；失败降级为事件流告警。与门面
    /// `broadcast_sync_body` 的语义差异仅投递方式——插件线程不 block_on
    /// （与内核线程模型口径一致），改为 spawn fire-and-forget。
    fn spawn_broadcast_sync_body(&self, body: serde_json::Map<String, Value>) {
        let node = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(node) = node else { return };
        let event_tx = self.event_tx.clone();
        self.runtime.spawn(async move {
            if let Err(error) = node.broadcast(SYNC_TOPIC, body).await {
                let _ = event_tx.send(P2pEvent::Warning(format!("sync broadcast failed: {error}")));
            }
        });
    }

    fn doc_get(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Read)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let storage = self.require_storage()?;
        let coll = DocumentCollection::new(domain, collection, CollectionConfig::default());
        let doc = coll.get(&storage, id)?;
        Ok(match doc {
            Some(value) => value,
            None => Value::Null,
        })
    }

    fn doc_put(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Write)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let doc = payload.get("doc").cloned().unwrap_or(Value::Null);
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let coll = DocumentCollection::new(domain, collection, config);
        let node_id = self.sync_node_id();
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let write = coll.put(&mut storage, id, &doc, &node_id, system_now_ms())?;
        let body = build_update_body(
            domain,
            collection,
            id,
            doc,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        drop(_io);
        self.spawn_broadcast_sync_body(body);
        Ok(Value::Null)
    }

    fn doc_delete(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Write)?;
        let collection = required_str(payload, "collection")?;
        let id = required_str(payload, "id")?;
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let coll = DocumentCollection::new(domain, collection, config);
        let node_id = self.sync_node_id();
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        // 与门面一致：删除不存在文档为空操作
        let Some(write) = coll.delete(&mut storage, id, &node_id, system_now_ms())? else {
            return Ok(Value::Null);
        };
        let body = build_delete_body(
            domain,
            collection,
            id,
            serde_json::to_value(&write.meta)?,
            Some(serde_json::to_value(&write.schema)?),
        );
        drop(_io);
        self.spawn_broadcast_sync_body(body);
        Ok(Value::Null)
    }

    fn doc_query(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let domain = resolve_doc_domain(plugin_id, payload, DocAccess::Read)?;
        let collection = required_str(payload, "collection")?;
        let config = Self::parse_config(payload.get("config").unwrap_or(&Value::Null))?;
        self.remember_collection_config(domain, collection, &config);
        let options = Self::parse_query_options(payload.get("options").unwrap_or(&Value::Null))?;
        let coll = DocumentCollection::new(domain, collection, config);
        let storage = self.require_storage()?;
        let result = coll.query(&storage, &options)?;
        // 对齐前端 QueryResult 形状：{items:[{id,data}], nextCursor?}
        let mut value = serde_json::json!({
            "items": result.items.iter().map(|item| serde_json::json!({
                "id": item.id, "data": item.data
            })).collect::<Vec<_>>(),
        });
        if let Some(cursor) = result.next_cursor {
            value["nextCursor"] = Value::String(cursor);
        }
        Ok(value)
    }

    fn doc_define_collection(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let collection = required_str(payload, "collection")?;
        let declaration: CollectionSchemaDeclaration =
            serde_json::from_value(payload.get("schema").cloned().unwrap_or(Value::Null))?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        declare_collection_schema(
            &mut storage,
            plugin_id,
            collection,
            &declaration,
            system_now_ms(),
        )?;
        Ok(Value::Null)
    }

    // ------------------------------------------------------------------
    // P6 声明式数据 API（data.*）：personal scope。策略随声明走，读写零
    // 同步参数；存储镜像是版本化句柄，写库即同步（pdsync 记账全自动）。
    // ------------------------------------------------------------------

    /// 解析 data.* 载荷的目标声明（name + 可选 version → 最新代际）。
    fn resolve_data_declaration(
        &self,
        plugin_id: &str,
        payload: &Value,
    ) -> Result<(
        crate::plugindata::CollectionDeclaration,
        crate::kernel::KernelStorage,
    )> {
        let name = required_str(payload, "name")?;
        // 插件只能触达自己前缀的集合（与声明校验同口径）
        if !name.starts_with(&format!("{plugin_id}:")) {
            return Err(PluginError::InvalidCall(format!(
                "collection {name:?} does not belong to plugin {plugin_id}"
            )));
        }
        let version = payload.get("version").and_then(Value::as_str);
        let storage = self.require_storage()?;
        // B5：org 空间数据读写路由到 orgd: 键——payload 携带 orgId 时走
        // org 声明域，否则走 personal 声明域。
        let decl = match payload.get("orgId").and_then(Value::as_str) {
            Some(org_id) => crate::plugindata::resolve_org(&storage, org_id, name, version)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?,
            None => crate::plugindata::resolve(&storage, name, version)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?,
        };
        Ok((decl, storage))
    }

    fn data_declare_collection(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let mut input = Self::parse_declare_input(payload)?;
        // F10：declaredBy 防伪造——kernel 侧强制覆盖为调用方 rootId，不信任
        // 插件自报（声明记录属审计面）。
        let my_root = self
            .my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        input.declared_by = Some(my_root.clone());
        // org space：payload 中可携带 orgId（由内核按插件运行空间传入）
        let org_id = payload.get("orgId").and_then(Value::as_str);
        // B5：org 空间声明须校验调用方确为该组织成员（orgId 来源可信）——
        // 防止插件以任意 org_id 越权声明组织集合。校验失败以 InvalidCall 拒绝。
        if input.space == Some(crate::plugindata::Space::Org) {
            let Some(oid) = org_id else {
                return Err(PluginError::InvalidCall(
                    "org space declaration requires orgId".to_string(),
                ));
            };
            let my_root = self
                .my_root_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let storage = self.require_storage()?;
            let is_member = crate::org::OrganizationService::get_record(&storage, oid)
                .ok()
                .flatten()
                .is_some_and(|rec| rec.find_member(&my_root).is_some());
            if !is_member {
                return Err(PluginError::InvalidCall(format!(
                    "caller {my_root} is not a member of org {oid}"
                )));
            }
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let decl =
            crate::plugindata::declare(&mut storage, plugin_id, input, system_now_ms(), org_id)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        serde_json::to_value(&decl).map_err(Into::into)
    }

    /// O3 写路径是否应走 orgq 路由（F1）：本机**非数据账号**写 org data-accounts
    /// 集合 → 不得直接落 `orgd:` 孤儿副本，须经 orgq 离线入队 / 在线经桥转发。
    /// 插件线程不能 `block_on`，故 QuickJS 通路采用「离线入队」路径：数据账号
    /// 上线后经统一同步链路冲刷（与 Kernel `data_org_write_route::Enqueued`
    /// 同收敛点）。本机数据账号 / all-members 集合 → 本地落库。
    fn orgq_route_write(&self, decl: &crate::plugindata::CollectionDeclaration) -> bool {
        if decl.space != Some(crate::plugindata::Space::Org)
            || decl.accounts != crate::plugindata::Accounts::DataAccounts
        {
            return false;
        }
        // 非驻留（非数据账号）→ 走 orgq；驻留 → 本地
        !self.org_local_resident(decl).unwrap_or(false)
    }

    /// F1：org data-accounts 且本机非数据账号 → orgq 离线入队（不落 orgd 副本）。
    /// 入队用相对 key，value 为 null 即删除（与 Kernel 侧 data_org_enqueue 同口径）。
    fn orgq_enqueue_write(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        value: &Value,
    ) {
        let Some(oid) = decl.org_id.as_deref() else {
            return;
        };
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let relative = key
            .strip_prefix(&crate::plugindata::org_data_prefix(
                oid,
                &decl.name,
                &decl.version,
            ))
            .unwrap_or(key);
        if let Ok(storage) = self.require_storage().map(|s| s.clone()) {
            let _ = crate::sync::orgsync::orgq_queue_put(
                &mut storage.clone(),
                oid,
                &col_full,
                relative,
                value,
            );
        }
    }

    fn data_save(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let value = payload.get("value").cloned().unwrap_or(Value::Null);
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // O4 工作项 4：encrypted 集合透明加解密——save 以当前 epoch 密钥加密
        // 后落 `orgd:` 密文（复制组流量只有密文）。在**路由前**加密：本地 /
        // orgq 离线入队两条写路径统一携带密文。无当前 epoch 密钥（非 reader）
        // → KeyUnavailable（AEAD 语义：密钥持有者集合 = 写权限集合）。
        let mut value = value;
        if decl.confidentiality == crate::plugindata::Confidentiality::Encrypted {
            if let Some(oid) = decl.org_id.as_deref() {
                let ct = crate::sync::orgsync::encrypt_orgd_value(
                    &storage,
                    oid,
                    &decl.name,
                    &decl.version,
                    key,
                    &value.to_string(),
                )
                .map_err(|e| match e {
                    // H3：密钥不可达 → 独立错误码（非 InvalidCall）。
                    crate::sync::orgsync::AccessDataError::KeyUnavailable(m) => {
                        PluginError::KeyUnavailable(m)
                    }
                    other => PluginError::InvalidCall(format!("encrypt {key}: {other}")),
                })?;
                value = serde_json::from_str(&ct).unwrap_or(Value::String(ct));
            }
        }
        // F1：org data-accounts 非数据账号 → 走 orgq 离线入队（不落孤儿副本）
        if self.orgq_route_write(&decl) {
            self.orgq_enqueue_write(&decl, key, &value);
            return Ok(Value::Null);
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = storage;
        crate::plugindata::save(&mut storage, &decl, key, &value.to_string())
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    fn data_delete(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // F1：org data-accounts 非数据账号 → 删除走 orgq 离线入队（value:null 墓碑）
        if self.orgq_route_write(&decl) {
            self.orgq_enqueue_write(&decl, key, &Value::Null);
            return Ok(Value::Null);
        }
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = storage;
        crate::plugindata::del(&mut storage, &decl, key)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    fn data_get(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let key = required_str(payload, "key")?;
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        // O3 读路径透明路由（QuickJS 通路）：org data-accounts 集合对本机
        // 非数据账号 → 走成员侧缓存（数据账号离线回缓存；在线 orgq 由宿主
        // 接线）。本机数据账号 / all-members 集合直接读本地。
        if decl.space == Some(crate::plugindata::Space::Org) && !self.org_local_resident(&decl)? {
            if let Some(v) = self.orgq_cached_get(&storage, &decl, key)? {
                return Ok(v);
            }
            // 无缓存 → UnavailableOffline 语义（回 Null，UI 标注不可得）
            return Ok(Value::Null);
        }
        let raw = crate::plugindata::get(&storage, &decl, key)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(match raw {
            // O4 工作项 4：encrypted 集合本地读解密——`orgd:` 密文按记录 epoch
            // 取密钥解密为插件明文。解密失败（非 reader/密钥未达）→ 回 Null。
            Some(text) => {
                if decl.confidentiality == crate::plugindata::Confidentiality::Encrypted {
                    if let Some(oid) = decl.org_id.as_deref() {
                        match crate::sync::orgsync::decrypt_orgd_value(
                            &storage,
                            oid,
                            &decl.name,
                            &decl.version,
                            key,
                            &text,
                        ) {
                            Ok(plain) => {
                                serde_json::from_str(&plain).unwrap_or(Value::String(plain))
                            }
                            Err(_) => Value::Null,
                        }
                    } else {
                        serde_json::from_str(&text).unwrap_or(Value::String(text))
                    }
                } else {
                    serde_json::from_str(&text).unwrap_or(Value::String(text))
                }
            }
            None => Value::Null,
        })
    }

    fn data_query(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let (decl, storage) = self.resolve_data_declaration(plugin_id, payload)?;
        let prefix = payload.get("prefix").and_then(Value::as_str);
        let limit = payload
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let cursor = payload.get("cursor").and_then(Value::as_str);
        // O3 读路径透明路由（QuickJS 通路）：同 data_get 的 org 缓存路由。
        let page = if decl.space == Some(crate::plugindata::Space::Org)
            && !self.org_local_resident(&decl)?
        {
            self.orgq_cached_query(&storage, &decl, prefix, limit, cursor)?
        } else {
            crate::plugindata::query(&storage, &decl, prefix, limit, cursor)
                .map_err(|e| PluginError::InvalidCall(e.to_string()))?
        };
        let is_encrypted = decl.confidentiality == crate::plugindata::Confidentiality::Encrypted;
        let oid = decl.org_id.as_deref().unwrap_or("");
        let mut value = serde_json::json!({
            "items": page.items.iter().map(|(key, raw)| {
                // O4 工作项 4：encrypted 集合 query 解密——密文按记录 epoch 取
                // 密钥解密为明文；失败（非 reader/密钥未达）→ null。
                let val = if is_encrypted {
                    match crate::sync::orgsync::decrypt_orgd_value(
                        &storage, oid, &decl.name, &decl.version, key, raw,
                    ) {
                        Ok(plain) => serde_json::from_str::<Value>(&plain)
                            .unwrap_or(Value::String(plain)),
                        Err(_) => Value::Null,
                    }
                } else {
                    serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone()))
                };
                serde_json::json!({
                    "key": key,
                    "value": val,
                })
            }).collect::<Vec<_>>(),
        });
        if let Some(next) = page.next_cursor {
            value["nextCursor"] = Value::String(next);
        }
        Ok(value)
    }

    fn data_drop_version(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        required_str(payload, "version")?; // drop 必须显式指定代际（不容许误清最新）
        let (decl, mut storage) = self.resolve_data_declaration(plugin_id, payload)?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        crate::plugindata::drop_version(&mut storage, &decl)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(Value::Null)
    }

    /// blob 保存：base64 入、内容哈希出（引用对象由插件自行写入记录：
    /// `{ "$blob": hash, name, size, mime }`）。
    fn data_save_blob(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
        let data_b64 = required_str(payload, "data")?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| PluginError::InvalidCall(format!("invalid base64: {e}")))?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        let info = crate::plugindata::blob::save_blob(&mut storage, &bytes)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        serde_json::to_value(&info).map_err(Into::into)
    }

    /// blob 读取：命中 → `{status:"ready", data(base64)}`；未命中 → 置 want
    /// 标记（lazy 拉取意图，pdsync hello 调和时向在线自设备拉取）并回
    /// `{status:"pending"}`——调用方稍后重读（前端建议轮询/重试）。
    ///
    /// **B3 feed-blob 请求方发起链路**：未命中时若该 hash 有 feed 来源登记
    /// （`feed::blob_source` → fromRootId），向来源 rootId 发首块 `feed-blob-req`
    /// （offset 0，经 `blob::throttle_request` 节流）；无来源登记维持现状
    /// （pdsync 自设备拉取）。
    fn data_read_blob(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
        let hash = required_str(payload, "hash")?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut storage = self.require_storage()?;
        if let Some(data) = crate::plugindata::blob::read_blob(&storage, hash)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?
        {
            return Ok(serde_json::json!({ "status": "ready", "data": data }));
        }
        // B3：查 feed 来源登记 → 有则出站首块 feed-blob-req（经节流）
        let now = system_now_ms();
        let from_source = crate::kernel::blob_source(&storage, hash, now)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        if let Some(from_root_id) = from_source {
            self.request_feed_blob(&mut storage, hash, &from_root_id);
        }
        crate::plugindata::blob::mark_want(&mut storage, hash)
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        Ok(serde_json::json!({ "status": "pending" }))
    }

    /// B3：向 feed 来源 rootId 出站首块 `feed-blob-req`（offset 0，跨联系人
    /// 分块传输通道）。经 `blob::throttle_request` 逐 hash 节流（服务方同口径），
    /// 命中节流跳过；无寻址对端/未解锁等静默跳过（由 pdsync want 兜底）。
    /// spawn 到内核 runtime 投递，失败静默（请求方由 `feed-blob-resp` 续拉/
    /// 下轮调和重试）。
    fn request_feed_blob(
        &self,
        storage: &mut crate::kernel::KernelStorage,
        hash: &str,
        from_root_id: &str,
    ) {
        use crate::kernel::dm_envelope::{KIND_FEED_BLOB_REQ, build_envelope};
        // 逐 hash 节流（§19.6）：距上次请求不足 BLOB_REQ_THROTTLE_MS 跳过
        let now = system_now_ms();
        if !crate::plugindata::blob::throttle_request(storage, hash, now).unwrap_or(false) {
            return;
        }
        // 解析来源 rootId 的对端（friend 记录择优；无可寻址端点跳过）
        let Some(peer) = crate::kernel::resolve_feed_recipient_peer_shared(storage, from_root_id)
            .ok()
            .flatten()
        else {
            return;
        };
        // 本机 rootId + 签名私钥
        let (Some(my_root_id), Some(signing_key)) = (
            self.my_root_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            self.signing_key
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        ) else {
            return;
        };
        let Some(node) = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let envelope = build_envelope(
            KIND_FEED_BLOB_REQ,
            &my_root_id,
            from_root_id,
            now,
            serde_json::json!({ "hash": hash, "offset": 0 }),
            &signing_key,
        );
        self.runtime.spawn(async move {
            let _ = node.dm_direct(&peer, envelope).await;
        });
    }

    /// 查询参数载荷（camelCase，对齐壳层 QueryOptionsDto）。
    fn parse_query_options(payload: &Value) -> Result<QueryOptions> {
        let parse_op = |op: Option<&str>| -> Result<FilterOp> {
            match op.unwrap_or("eq") {
                "eq" => Ok(FilterOp::Eq),
                "startsWith" => Ok(FilterOp::StartsWith),
                "gt" => Ok(FilterOp::Gt),
                "lt" => Ok(FilterOp::Lt),
                "gte" => Ok(FilterOp::Gte),
                "lte" => Ok(FilterOp::Lte),
                other => Err(PluginError::InvalidCall(format!(
                    "filter op must be one of eq/startsWith/gt/lt/gte/lte, got {other:?}"
                ))),
            }
        };
        let filter = payload
            .get("filter")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        Ok(QueryFilter {
                            field: required_str(item, "field")?.to_string(),
                            value: item.get("value").cloned().unwrap_or(Value::Null),
                            op: parse_op(item.get("op").and_then(Value::as_str))?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(QueryOptions {
            index_name: payload
                .get("indexName")
                .and_then(Value::as_str)
                .map(str::to_string),
            index_value: payload.get("indexValue").cloned(),
            index_prefix: payload
                .get("indexPrefix")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            start_after_id: payload
                .get("startAfterId")
                .and_then(Value::as_str)
                .map(str::to_string),
            limit: payload
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            reverse: payload
                .get("reverse")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            filter,
        })
    }
}

// ------------------------------------------------------------------
// 系统能力（sys.*）：启动即返，结果经事件队列异步回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `sys.exec.start`：spawn 到内核 runtime（内部 spawn_blocking），完成
    /// 后向本插件事件队列回 `sys-exec-result`。
    fn sys_exec_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let program = required_str(payload, "program")?.to_string();
        let args: Vec<String> = payload
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let workdir = payload
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::sys::exec_blocking(&program, &args, workdir.as_deref())
            })
            .await;
            let payload = match result {
                Ok(Ok(r)) => serde_json::json!({
                    "callId": call_id,
                    "exitCode": r.exit_code,
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                }),
                Ok(Err(error)) => serde_json::json!({ "callId": call_id, "error": error }),
                Err(error) => serde_json::json!({
                    "callId": call_id,
                    "error": format!("exec task join failed: {error}")
                }),
            };
            // 插件线程可能已退出：丢弃结果即可（Promise 随线程销毁失去意义）
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-exec-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `sys.execStream.start`：流式执行外部命令（codebuddy `--output-format
    /// stream-json` 等 NDJSON 流工具）。stdout 按完整行逐块回 `sys-exec-chunk`
    /// （callId 配对，prelude 分发到 onChunk），进程退出回 `sys-exec-result`
    /// 终态兑现 Promise。与非流式 `sys.exec.start` 的区别：多次中间事件 + 一次终态。
    fn sys_exec_stream_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let program = required_str(payload, "program")?.to_string();
        eprintln!(
            "[stream-dbg] execStream start program={program} args={:?}",
            payload.get("args")
        );
        let args: Vec<String> = payload
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let workdir = payload
            .get("workdir")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let chunk_tx = event_tx.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::sys::exec_streaming_blocking(
                    &program,
                    &args,
                    workdir.as_deref(),
                    move |chunk| {
                        eprintln!(
                            "[stream-dbg] execStream chunk done={} text_len={} exit={:?}",
                            chunk.done,
                            chunk.text.len(),
                            chunk.exit_code
                        );
                        let _ = chunk_tx.send(PluginEvent::Dispatch {
                            kind: "sys-exec-chunk".to_string(),
                            payload: serde_json::json!({
                                "callId": call_id,
                                "chunk": {
                                    "text": chunk.text,
                                    "done": chunk.done,
                                    "exitCode": chunk.exit_code,
                                },
                            }),
                        });
                    },
                )
            })
            .await;
            let payload = match result {
                Ok(Ok(r)) => {
                    eprintln!(
                        "[stream-dbg] execStream done exit={} stderr_len={}",
                        r.exit_code,
                        r.stderr.len()
                    );
                    serde_json::json!({
                        "callId": call_id,
                        "exitCode": r.exit_code,
                        "stdout": r.stdout,
                        "stderr": r.stderr,
                    })
                }
                Ok(Err(error)) => {
                    eprintln!("[stream-dbg] execStream error={error}");
                    serde_json::json!({ "callId": call_id, "error": error })
                }
                Err(error) => serde_json::json!({
                    "callId": call_id,
                    "error": format!("exec stream task join failed: {error}")
                }),
            };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-exec-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    fn sys_fetch_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let url = required_str(payload, "url")?.to_string();
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let headers: Option<HashMap<String, String>> = payload
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });
        let body = payload
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let payload =
                match crate::sys::fetch(&url, method.as_deref(), headers.as_ref(), body.as_deref())
                    .await
                {
                    Ok(r) => serde_json::json!({
                        "callId": call_id,
                        "status": r.status,
                        "headers": r.headers,
                        "body": r.body,
                    }),
                    Err(error) => serde_json::json!({ "callId": call_id, "error": error }),
                };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-fetch-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }

    /// `sys.fetchStream.start`：流式 HTTP。每收到一个响应体文本块向本插件
    /// 事件队列回 `sys-stream-chunk`（callId 配对，prelude 分发到 onChunk），
    /// 流结束回 `sys-stream-result`（done 块载荷，prelude 兑现 Promise）。
    /// 与非流式 `sys.fetch.start` 的区别：多次中间事件 + 一次终态事件。
    fn sys_fetch_stream_start(&self, rtx: &PluginRuntimeContext, payload: &Value) -> Result<Value> {
        let call_id = required_call_id(payload)?;
        let url = required_str(payload, "url")?.to_string();
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let headers: Option<HashMap<String, String>> = payload
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            });
        let body = payload
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_tx = rtx.event_tx.clone();
        self.runtime.spawn(async move {
            let chunk_tx = event_tx.clone();
            let result = crate::sys::fetch_stream(
                &url,
                method.as_deref(),
                headers.as_ref(),
                body.as_deref(),
                move |chunk| {
                    let done = chunk.done;
                    let _ = chunk_tx.send(PluginEvent::Dispatch {
                        kind: "sys-stream-chunk".to_string(),
                        payload: serde_json::json!({
                            "callId": call_id,
                            "chunk": {
                                "text": chunk.text,
                                "done": chunk.done,
                                "status": chunk.status,
                                "headers": chunk.headers,
                            },
                        }),
                    });
                    // done 块经 chunk 通道到达后，终态结果由下方 sys-stream-result
                    // 兑现 Promise（prelude 的 settleAsync 消费）
                    let _ = done;
                },
            )
            .await;
            let payload = match result {
                Ok(()) => serde_json::json!({ "callId": call_id, "done": true }),
                Err(error) => serde_json::json!({ "callId": call_id, "error": error }),
            };
            let _ = event_tx.send(PluginEvent::Dispatch {
                kind: "sys-stream-result".to_string(),
                payload,
            });
        });
        Ok(serde_json::json!({ "started": true }))
    }
}

// ------------------------------------------------------------------
// 宿主查询应答回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `query.respond`：JS 侧查询处理完成，结果送回等待中的
    /// `plugin_host_query` 调用方（在途表无记录说明已超时，静默丢弃）。
    fn query_respond(&self, payload: &Value) -> Result<Value> {
        let query_id = required_call_id(payload)?;
        let result = payload.get("result").cloned().unwrap_or(Value::Null);
        let sender = self
            .pending_queries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&query_id);
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
        Ok(Value::Null)
    }
}

// ------------------------------------------------------------------
// 社交定向投递能力（spark.feed.*，social-feed §9.1；QuickJS 后台侧）
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `feed.deliver` 能力：社交定向投递。载荷 camelCase，与 iframe 侧
    /// `sdk.feed.deliver` 同构。插件身份用运行时绑定 `plugin_id`（不信 JS
    /// 自报）；出站 topic 前缀校验（== 插件 id，架构 §8）与内核限流（§9.3，
    /// 与 Kernel 门面共享同一实例）在能力内完成。
    fn feed_deliver(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let topic = required_str(payload, "topic")?;
        // 出站 topic 前缀 == 插件 id（架构 §8「topic 前缀即插件归属」，与桥
        // dispatcher 同口径）
        let prefix = topic.split(':').next().unwrap_or(topic);
        if prefix != plugin_id {
            return Err(PluginError::InvalidCall(format!(
                "InvalidTopic: topic prefix \"{prefix}\" does not match plugin \"{plugin_id}\""
            )));
        }
        let feed_id = payload
            .get("feedId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(crate::kernel::generate_feed_id_for_plugin);
        let input_payload = payload.get("payload").cloned().unwrap_or(Value::Null);
        let recipients: Vec<String> = payload
            .get("recipients")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let reply_to = payload.get("replyTo").and_then(Value::as_str);
        let result = crate::kernel::feed_deliver_shared(
            self,
            plugin_id,
            topic,
            &feed_id,
            &input_payload,
            &recipients,
            reply_to,
        )?;
        serde_json::to_value(&result).map_err(Into::into)
    }

    /// `feed.pull` 能力：收件箱游标补读（接收侧免权限）。topic 前缀须 ==
    /// 本插件 id（架构 §8 前缀即归属，防插件补读他人收件箱域），cursor/limit
    /// 分页由内核 `feed_pull_shared` 完成。
    fn feed_pull(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let topic = required_str(payload, "topic")?;
        // 前缀 == 插件 id（与 deliver 同口径：收件箱键域按 pluginId 划分）
        let prefix = topic.split(':').next().unwrap_or(topic);
        if prefix != plugin_id {
            return Err(PluginError::InvalidCall(format!(
                "InvalidTopic: topic prefix \"{prefix}\" does not match plugin \"{plugin_id}\""
            )));
        }
        let cursor = payload.get("cursor").and_then(Value::as_str);
        let limit = payload
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(20);
        let result = crate::kernel::feed_pull_shared(self, topic, cursor, limit)?;
        serde_json::to_value(&result).map_err(Into::into)
    }
}

// ------------------------------------------------------------------
// 身份能力（spark.identity.*）
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `identity.verify` 能力：ed25519 分离签名验签（纯函数，免内核状态）。
    /// 载荷 `{payload, sig, pubKey}`（三者均字符串），验签口径与
    /// `crate::identity::verify_ed25519_signature` 一致（payload 按 UTF-8 字节、
    /// 签名 64B 与公钥 32B 为 base64；解码/长度失败一律 false，不报错）。返回
    /// `{valid: bool}`——对齐 iframe 侧 `plugin-identity-verify` 的 VerifyResultDto。
    fn identity_verify(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
        let pl = required_str(payload, "payload")?;
        let sig = required_str(payload, "sig")?;
        let pub_key = required_str(payload, "pubKey")?;
        let valid = crate::identity::verify_ed25519_signature(pl, sig, pub_key);
        Ok(serde_json::json!({ "valid": valid }))
    }

    /// `identity.sign` 能力：以**域身份**私钥签名（`sign_with_domain_identity`
    /// 口径，域密钥由根种子即时派生、不落盘）。域缺省 = 插件根域
    /// `plugin:{pluginId}`（不信 JS 自报越权签其它域；对齐桥 dispatcher 按绑定
    /// 身份注入域）。返回 `DomainSignatureInfo`（domain/domainId/publicKey/
    /// signature/payloadHash，camelCase）。需解锁期种子（`seed_shared`），未解锁
    /// 报 InvalidInput。
    fn identity_sign(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let pl = required_str(payload, "payload")?;
        // 域缺省 = 插件根域；显式指定的域必须 == 插件根域（防越权签他域）
        let domain = payload.get("domain").and_then(Value::as_str).unwrap_or("");
        let root_domain = format!("plugin:{plugin_id}");
        let domain = if domain.is_empty() {
            root_domain.as_str()
        } else if domain == root_domain {
            domain
        } else {
            return Err(PluginError::InvalidCall(format!(
                "domain {domain:?} does not match plugin {plugin_id}"
            )));
        };
        let seed = self
            .seed_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ok_or_else(|| PluginError::InvalidInput("identity locked".to_string()))?;
        let result = crate::kernel::identity_sign_shared(&seed, domain, pl)?;
        serde_json::to_value(&result).map_err(Into::into)
    }
}

// ------------------------------------------------------------------
// 消息能力（spark.messages.*）
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `messages.sendAppMessage` 能力：互动通知写应用会话（p2p-messages.md §20）。
    /// 会话 `app:{pluginId}` 由运行时绑定 plugin_id 确定性派生，不信 JS 自报
    /// （§20.4 归属不变量 2）。载荷 `{summary, card?}`：summary 为纯文本摘要
    /// （必填，trim 后 ≤200 字符）；card 可选（{viewId, data}，内核只透传）。
    /// 限流在共享 `app_msg_limiter` 内强制（与 Kernel 门面 `message_app_send`
    /// 同实例，10 条/60s）。space 缺省 "personal"（插件后台无 org 会话写）。
    fn messages_send_app_message(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let space = payload
            .get("spaceKey")
            .and_then(Value::as_str)
            .unwrap_or("personal");
        let summary = required_str(payload, "summary")?;
        // 组装 payload：summary 为必填，其余插件自描述字段原样透传
        let mut app_payload = payload.clone();
        if let Value::Object(map) = &mut app_payload {
            // spaceKey 是宿主注入的会话路由，不落入应用消息 payload
            map.remove("spaceKey");
            map.remove("card");
        }
        // card 可选：JS 侧 `card || null` 会显式传 null——null 视作无卡片
        // （跳过反序列化，避免 `invalid type: null`）。
        let card: Option<crate::message::AppMessageCard> = match payload.get("card") {
            Some(Value::Null) | None => None,
            Some(value) => Some(
                serde_json::from_value(value.clone())
                    .map_err(|e| PluginError::InvalidCall(format!("invalid card: {e}")))?,
            ),
        };
        let view = crate::kernel::message_app_send_shared(
            self,
            space,
            plugin_id,
            summary,
            app_payload,
            card,
        )?;
        serde_json::to_value(&view).map_err(Into::into)
    }
}

/// 取载荷必填 callId/queryId（u64）。
fn required_call_id(payload: &Value) -> Result<u64> {
    payload
        .get("callId")
        .or_else(|| payload.get("queryId"))
        .and_then(Value::as_u64)
        .ok_or_else(|| PluginError::InvalidCall("missing u64 field: callId".to_string()))
}
