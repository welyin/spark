//! 宿主注入接口：组织/同步业务状态全部由宿主提供（p2p 模块不直接操作业务 db）。
//!
//! 全部方法为同步调用（与 [`crate::storage::StorageBackend`] 的同步口径一致），
//! 在节点事件循环内被调用——实现应保持轻量（KV 读写级别），禁止阻塞。
//! 重 IO 的 dm 入站处理经 [`P2pHost::dm_handler`] 返回的 [`DmHandler`] 句柄
//! 由事件循环 spawn 到阻塞线程池执行，不占用事件循环线程。

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use crate::org::gateway::OrgMemberHint;
use crate::org::recovery::RecoveryViewItem;

/// org-share 接收结果（accepted 时携带 ack 载荷）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgShareAck {
    /// 发送方 syncId（pubsub 回发 ack / 直连响应回显）。
    pub sync_id: Option<String>,
    pub org_id: String,
    pub target_root_id: String,
    pub receiver_root_id: String,
}

/// 宿主业务回调（对齐 TS P2PRuntimeOptions + pubsub-message-handler 的分支）。
///
/// 所有方法均有默认空实现，宿主按需覆盖。
pub trait P2pHost: Send {
    /// 当前登录身份 rootId（未登录返回 None）。
    fn current_root_id(&mut self) -> Option<String> {
        None
    }

    /// 本地存证头 hash（无存证 → None，信封 `evidenceHeadHash` 序列化为 null）。
    fn evidence_head_hash(&mut self) -> Option<String> {
        None
    }

    /// `applyRemoteUpdate`：update/delete/history-response 落库
    /// （pubsub-message-handler.ts:74-101）。
    fn apply_remote_update(
        &mut self,
        _domain: &str,
        _collection: &str,
        _id: &str,
        _payload: Value,
        _meta: Value,
        _schema: Option<Value>,
    ) -> std::result::Result<(), String> {
        Ok(())
    }

    /// org-share 接收（org.md §7；pubsub 与直连共用，`source` 为 "pubsub"/"direct"）。
    /// 返回 `Ok(Some(ack))` 表示接受。
    fn apply_incoming_org_share(
        &mut self,
        _payload: Value,
        _source: &'static str,
    ) -> std::result::Result<Option<OrgShareAck>, String> {
        Ok(None)
    }

    /// org-pull-list 响应生成（org.md §9.2）：返回完整响应帧 JSON
    /// （`{"ok":...,"type":"org-pull-list-response",...}`）。`remote_peer_id` 为连接层对端。
    fn handle_org_pull_list(
        &mut self,
        _payload: Value,
        _remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        Err("org-pull-list not implemented".to_string())
    }

    /// org-pull-org 响应生成（org.md §9.3）。
    fn handle_org_pull_org(
        &mut self,
        _payload: Value,
        _remote_peer_id: Option<String>,
    ) -> std::result::Result<Value, String> {
        Err("org-pull-org not implemented".to_string())
    }

    /// org-recovery 恢复视图（org.md §10）。
    fn recovery_view(&mut self) -> Vec<RecoveryViewItem> {
        Vec::new()
    }

    /// dm 直连接收（`/spark/dm/1.0.0`）：payload 为 dm 信封 JSON（透明搬运，
    /// 验签/落库由 kernel 层负责），`remote_peer_id` 为连接层对端；
    /// 返回值序列化为直连响应帧回传发送方。
    fn handle_dm(
        &mut self,
        _payload: Value,
        _remote_peer_id: &str,
    ) -> std::result::Result<Value, String> {
        Err("dm not supported".into())
    }

    /// 重 IO dm 入站处理器：返回 `Some` 时事件循环把 dm 请求 spawn 到阻塞
    /// 线程池调用该句柄（完成后再回到事件循环 send_response），事件循环
    /// 线程不再执行存储 IO；返回 `None` 退化为事件循环内同步 `handle_dm`。
    fn dm_handler(&self) -> Option<Arc<dyn DmHandler>> {
        None
    }

    /// 对端版本观察上报（`/spark/version/1.0.0`）。
    fn on_peer_version(&mut self, _version: &str, _peer_id: &str) {}

    /// 新对端建连（首个连接确认；事件循环线程内调用，保持轻量、禁止阻塞）。
    fn on_peer_connected(&mut self, _peer_id: &str) {}

    /// org-share-ack 唤醒（按 payload.syncId 匹配发送方等待器）。
    fn on_org_share_ack(&mut self, _payload: Value) {}

    /// 组织私有 DHT 命中的成员提示回填（p2p-messages.md §15）：网关提供的
    /// `{peerId, addresses}` 条目，业务层按未验证口径入邻居池（`verified=false`）；
    /// 组织校验仍走 pull/claim 链路，信任边界不变。
    fn on_org_member_hints(&mut self, _hints: &[OrgMemberHint]) {}
}

/// 可在事件循环线程外执行的 dm 入站处理器（实现须 `Send + Sync`，
/// 通常由宿主字段的 Arc 克隆组装）。
pub trait DmHandler: Send + Sync {
    /// 语义同 [`P2pHost::handle_dm`]；`online_peers` 为当前已连接的
    /// libp2p peerId 集合（事件循环在分发请求时快照）。
    fn handle_dm(
        &self,
        payload: Value,
        remote_peer_id: &str,
        online_peers: &HashSet<String>,
    ) -> std::result::Result<Value, String>;
}

/// 空宿主（测试/最小装配）。
#[derive(Default)]
pub struct NoopHost;

impl P2pHost for NoopHost {}
