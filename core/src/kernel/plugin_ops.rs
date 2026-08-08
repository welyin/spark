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
use crate::plugin::{PluginError, is_valid_plugin_id, spawn_plugin_runtime};

use super::{Kernel, Result};

impl Kernel {
    /// 启动插件的后台运行时（专用线程 + QuickJS 沙箱）。
    ///
    /// `script` 为插件后台入口的完整 JS 源码（manifest 加载与 .spkg 读取
    /// 在后续阶段接入；当前由调用方给全量源码）。同一插件重复启动报
    /// [`PluginError::AlreadyRunning`]。
    pub fn plugin_start_background(&mut self, plugin_id: &str, script: &str) -> Result<()> {
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
        drop(handle);
        if let Some(join) = self.plugin_joins.remove(plugin_id) {
            let _ = join.join();
        }
        Ok(())
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
    pub fn plugin_host_query(
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

    /// 停止全部插件后台运行时（身份切换/关停前调用）。
    pub(crate) fn plugin_stop_all_background(&mut self) {
        for plugin_id in self.plugin_registry.running_ids() {
            let _ = self.plugin_stop_background(&plugin_id);
        }
    }

    /// 启动事件路由任务：订阅内核事件广播，把 bot 会话的 ChatReceived
    /// 转发给归属插件（覆盖多设备回同步 echo 路径）。init 时启动，
    /// shutdown 时 abort。
    pub(crate) fn spawn_plugin_router(&mut self) {
        let registry = self.plugin_registry.clone();
        let mut rx = self.event_tx.subscribe();
        self.plugin_router = Some(self.runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(P2pEvent::ChatReceived(payload)) => registry.dispatch_chat(&payload),
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
