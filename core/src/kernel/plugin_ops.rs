//! 插件后台运行时的 kernel 门面（plugin-runtime 设计 §三）：
//! 启停、运行状态查询与事件路由接线。
//!
//! 事件到达插件的两条路径：
//! - **本机发送**（`message_send_text` 的 bot 分支）：该路径不发 ChatReceived
//!   广播（前端以返回值刷新），由发送处显式 `dispatch_chat`；
//! - **多设备回同步 echo**（`inbound_dm` 落库后发 ChatReceived 广播）：由
//!   本模块的路由任务订阅广播转发（[`Kernel::spawn_plugin_router`]）。
//!
//! 身份切换（`align_storage`）时全部停机——插件数据（bot 联系人、会话）
//! 不跨身份；`shutdown` 同样先停插件再 flush 存储。

use tokio::sync::broadcast;

use crate::p2p::P2pEvent;
use crate::plugin::{
    PluginError, PluginHostShared, PluginRuntimeRegistry, is_valid_plugin_id, spawn_plugin_runtime,
};

use super::{Kernel, Result};

/// O3 filtered 权限钩子的宿主实现：把 orgq-req 的 canRead/canWrite 裁决投递到
/// 数据账号侧插件的后台运行时（QuickJS）执行（经 [`PluginHostQuery`] 的
/// PluginEvent::Query 同步查询机制）。
///
/// 桥接语义（同步/异步）：
/// - **同步等待**：orgq-req 处理是数据账号侧事件循环内的同步路径，钩子裁决须
///   即时返回过滤/受理结果。本实现经 `PluginHostQuery::query` 投递 Query 事件
///   到插件线程、阻塞等待 JS 应答（上限 2s），与 `plugin_host_query` 同机制；
/// - **超时兜底（fail-closed）**：插件未运行 / 投递失败 / 2s 未应答 → `None`，
///   一律按「拒绝」处理——防恶意插件死循环卡死查询通道；
/// - **钩子抛异常 = 拒绝**：JS 侧异常经 `query.respond` 回 `{error}`，`query()`
///   返回该值而非真布尔 → 解析失败按拒绝兜底。
#[derive(Clone)]
pub(crate) struct QuickJsOrgqHook {
    query: PluginHostQuery,
    /// F2：单请求钩子总预算截止（epoch ms）——一个 orgq-req 内多条记录逐条
    /// 过 canRead/canWrite，累计执行须有界（超预算按 fail-closed 拒绝），
    /// 防恶意/故障插件长期阻塞 io_lock。
    budget_deadline: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

impl QuickJsOrgqHook {
    pub(crate) fn new(query: PluginHostQuery) -> Self {
        let budget_deadline = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(
            crate::p2p::node::system_now_ms() + ORGQ_HOOK_BUDGET_MS,
        ));
        Self {
            query,
            budget_deadline,
        }
    }

    /// 从集合全名 `name@v{version}` 解析宿主钩子注册键：插件集合名 = `@v` 之前、
    /// 且为插件 id 前缀（如 `ai-chat:finance` → 注册键 `ai-chat:finance`，
    /// plugin_id = `ai-chat`）。
    fn collection_name(col_full: &str) -> &str {
        match col_full.find("@v") {
            Some(at) => &col_full[..at],
            None => col_full,
        }
    }

    fn plugin_id(col_full: &str) -> &str {
        let name = Self::collection_name(col_full);
        match name.find(':') {
            Some(i) => &name[..i],
            None => name,
        }
    }

    /// 执行一次 JS 裁决查询并归一化为布尔；超时/异常/未运行 → false（拒绝）。
    fn run_query(&self, plugin_id: &str, kind: &str, payload: serde_json::Value) -> bool {
        // F2：单请求总预算——累计超预算 → fail-closed 拒绝（不执行钩子）。
        use std::sync::atomic::Ordering;
        if crate::p2p::node::system_now_ms() > self.budget_deadline.load(Ordering::Relaxed) {
            log::warn!("[ORGQ] hook budget exceeded, fail-closed reject");
            return false;
        }
        self.query.query(plugin_id, kind, payload).is_some_and(|v| {
            // 应答应为布尔；错误 `{"error":...}` / 非布尔一律拒绝
            v.as_bool().unwrap_or(false)
        })
    }
}

/// F2：单请求 orgq 钩子总预算（毫秒，10s 封顶）。一个 orgq-req 内逐条过
/// canRead/canWrite 的累计执行时间不得超过此值，防恶意/故障插件长期阻塞
/// io_lock（单钩子超时由插件引擎熔断）。
const ORGQ_HOOK_BUDGET_MS: i64 = 10_000;

impl super::inbound_dm::OrgqPermHook for QuickJsOrgqHook {
    fn has_runtime(&self, col_full: &str, kind: &str) -> bool {
        let plugin_id = Self::plugin_id(col_full);
        let name = Self::collection_name(col_full);
        let registered = self.query.plugin_host.has_filter(name, kind);
        // 插件后台运行中（注册表可达）且注册了该种类过滤器
        registered && self.query.plugin_registry.is_running(plugin_id)
    }

    fn can_read(&self, member: &str, col_full: &str, key: &str) -> bool {
        let plugin_id = Self::plugin_id(col_full);
        let name = Self::collection_name(col_full);
        self.run_query(
            plugin_id,
            "data.canRead",
            serde_json::json!({ "collection": name, "member": member, "key": key }),
        )
    }

    fn can_write(
        &self,
        member: &str,
        col_full: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> bool {
        let plugin_id = Self::plugin_id(col_full);
        let name = Self::collection_name(col_full);
        self.run_query(
            plugin_id,
            "data.canWrite",
            serde_json::json!({ "collection": name, "member": member, "key": key, "value": value }),
        )
    }
}

impl Kernel {
    /// 启动插件的后台运行时（专用线程 + QuickJS 沙箱）。
    ///
    /// `script` 为插件后台入口的完整 JS 源码（manifest 加载与 .spkg 读取
    /// 由壳层负责，内核只认「插件 id + 源码 + 授权清单」）。
    /// `permissions` 为安装时授权的权限清单（市场状态 grantedPermissions
    /// 快照：基础权限恒在列，高级权限须 manifest 声明并授权）；capability
    /// 分发逐调用强制（host_env），未授权以错误回流 JS。同一插件重复启动报
    /// [`PluginError::AlreadyRunning`]。
    pub fn plugin_start_background(
        &mut self,
        plugin_id: &str,
        script: &str,
        permissions: &[String],
    ) -> Result<()> {
        if !is_valid_plugin_id(plugin_id) {
            return Err(PluginError::InvalidId(plugin_id.to_string()).into());
        }
        if self.plugin_registry.is_running(plugin_id) {
            return Err(PluginError::AlreadyRunning(plugin_id.to_string()).into());
        }
        let (handle, join) = spawn_plugin_runtime(
            plugin_id,
            script,
            self.plugin_host.clone(),
            self.plugin_registry.clone(),
            permissions.to_vec(),
        )?;
        self.plugin_registry.register(plugin_id, handle);
        self.plugin_joins.insert(plugin_id.to_string(), join);
        Ok(())
    }

    /// 停止插件的后台运行时（幂等：未运行为空操作）。阻塞至线程退出
    /// （上界 ≈ 事件轮询间隔 100ms + interrupt 中断延迟；JS 死循环由
    /// interrupt handler 强制打断）。
    pub fn plugin_stop_background(&mut self, plugin_id: &str) -> Result<()> {
        let Some(handle) = self.plugin_registry.remove(plugin_id) else {
            return Ok(());
        };
        handle.request_stop();
        // F3：插件停止时按其前缀清理 filter_caps 注册表——消除「曾注册→升级后
        // 未注册」的 fail-open 缝隙（has_runtime 对已停插件集合不再误报可服务）。
        self.clear_filter_caps_for(plugin_id);
        drop(handle);
        if let Some(join) = self.plugin_joins.remove(plugin_id) {
            let _ = join.join();
        }
        Ok(())
    }

    /// F3：清理 filter_caps 中归属指定插件的集合条目（键形 `{plugin_id}:{rest}`）。
    fn clear_filter_caps_for(&self, plugin_id: &str) {
        let prefix = format!("{plugin_id}:");
        let mut caps = self
            .plugin_host
            .filter_caps
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        caps.retain(|collection, _| !collection.starts_with(&prefix));
    }

    /// 插件后台运行时是否存活（bot 在线状态的权威来源）。
    pub fn plugin_background_running(&self, plugin_id: &str) -> bool {
        self.plugin_registry.is_running(plugin_id)
    }

    /// 运行中的插件后台 id 列表（壳层对账期望集用）。
    pub fn plugin_background_running_ids(&self) -> Vec<String> {
        self.plugin_registry.running_ids()
    }

    /// 宿主 → 插件反向查询（如前端删除联系人前的「bot 还在吗」询问）。
    ///
    /// 同步阻塞等待应答（上限 2s；调用方须在命令线程或 spawn_blocking 内）。
    /// 插件未运行、投递失败、超时未应答均返回 `None`——调用方按「查询无
    /// 结果」的保守语义处理（删除询问场景即「bot 不存在，放行删除」）。
    ///
    /// 壳层持 `Mutex<Kernel>` 调用时应改走 [`Self::plugin_host_query_handle`]：
    /// 锁内仅克隆句柄，释锁后再等待——避免 2s 超时占住内核全局锁。
    pub fn plugin_host_query(
        &self,
        plugin_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.plugin_host_query_handle()
            .query(plugin_id, kind, payload)
    }

    /// 克隆宿主查询句柄（`PluginHostShared` 与注册表均为 Arc 共享格，克隆
    /// 廉价且脱离内核锁独立可用）。
    pub fn plugin_host_query_handle(&self) -> PluginHostQuery {
        PluginHostQuery {
            plugin_host: self.plugin_host.clone(),
            plugin_registry: self.plugin_registry.clone(),
        }
    }

    /// 停止全部插件后台运行时（身份切换/关停前调用）。
    pub(crate) fn plugin_stop_all_background(&mut self) {
        for plugin_id in self.plugin_registry.running_ids() {
            let _ = self.plugin_stop_background(&plugin_id);
        }
    }

    /// 启动事件路由任务：订阅内核事件广播，把 bot 会话的 ChatReceived
    /// 转发给归属插件（覆盖多设备回同步 echo 路径），把 P6 数据变更
    /// （PluginDataChanged）投递给归属插件的后台运行时。init 时启动，
    /// shutdown 时 abort。
    pub(crate) fn spawn_plugin_router(&mut self) {
        let registry = self.plugin_registry.clone();
        let mut rx = self.event_tx.subscribe();
        self.plugin_router = Some(self.runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(P2pEvent::ChatReceived(payload)) => registry.dispatch_chat(&payload),
                    Ok(P2pEvent::PluginDataChanged(payload)) => {
                        registry.dispatch_data_change(&payload)
                    }
                    Ok(P2pEvent::FeedReceived(payload)) => registry.dispatch_feed(&payload),
                    Ok(_) => {}
                    // 慢消费丢旧事件：路由场景可容忍（插件漏处理的副作用
                    // 仅是当条 bot 消息未响应），继续即可
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
    }
}

/// 宿主 → 插件反向查询句柄（kernel 共享格的 Arc 克隆，见
/// [`Kernel::plugin_host_query_handle`]）。
///
/// 持有者可脱离 `Mutex<Kernel>` 执行 [`Self::query`] 的阻塞等待——壳层命令
/// 在锁内克隆本句柄后立即释锁，2s 等待不再占住内核全局锁。
#[derive(Clone)]
pub struct PluginHostQuery {
    plugin_host: PluginHostShared,
    plugin_registry: PluginRuntimeRegistry,
}

impl PluginHostQuery {
    /// 由宿主共享句柄 + 运行注册表构造（仅供插件运行时单元测试用——
    /// host/dm_handler 实际经 Kernel::plugin_host_query_handle 构造）。
    #[cfg(test)]
    pub(crate) fn new(
        plugin_host: PluginHostShared,
        plugin_registry: PluginRuntimeRegistry,
    ) -> Self {
        Self {
            plugin_host,
            plugin_registry,
        }
    }
}

impl PluginHostQuery {
    /// 投递查询并同步阻塞等待应答（上限 2s；调用方须在命令线程或
    /// spawn_blocking 内）。插件未运行、投递失败、超时未应答均返回 `None`。
    pub fn query(
        &self,
        plugin_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Option<serde_json::Value> {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        static QUERY_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let query_id = QUERY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = channel();
        self.plugin_host
            .pending_queries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(query_id, tx);
        if !self
            .plugin_registry
            .dispatch_query(plugin_id, query_id, kind, payload)
        {
            self.plugin_host
                .pending_queries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&query_id);
            return None;
        }
        let result = rx.recv_timeout(Duration::from_secs(2)).ok();
        if result.is_none() {
            // 超时：清理在途记录（JS 侧迟到的应答发现无记录会静默丢弃）
            self.plugin_host
                .pending_queries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&query_id);
        }
        result
    }
}
