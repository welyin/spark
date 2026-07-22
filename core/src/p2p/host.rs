//! 宿主注入接口：组织/同步业务状态全部由宿主提供（p2p 模块不直接操作业务 db）。
//!
//! 全部方法为同步调用（与 [`crate::storage::StorageBackend`] 的同步口径一致），
//! 在节点事件循环内被调用——实现应保持轻量（KV 读写级别），禁止阻塞。

use serde_json::Value;

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

    /// 对端版本观察上报（`/spark/version/1.0.0`）。
    fn on_peer_version(&mut self, _version: &str, _peer_id: &str) {}

    /// org-share-ack 唤醒（按 payload.syncId 匹配发送方等待器）。
    fn on_org_share_ack(&mut self, _payload: Value) {}
}

/// 空宿主（测试/最小装配）。
#[derive(Default)]
pub struct NoopHost;

impl P2pHost for NoopHost {}
