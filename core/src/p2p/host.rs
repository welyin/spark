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

    /// org-mail 直连接收（`/spark/org-mail/1.0.0`，阶段四E p2p-org-mail §21）：
    /// payload 为 `{op: deliver|fetch, ...}` 请求 JSON；默认实现拒绝
    /// （非网关/轻量宿主不当邮箱——wrong-org）。
    fn handle_org_mail(
        &mut self,
        _payload: &Value,
        _remote_peer_id: &str,
    ) -> std::result::Result<Value, String> {
        Ok(serde_json::json!({ "ok": false, "reason": "wrong-org" }))
    }

    /// 重 IO dm 入站处理器：返回 `Some` 时事件循环把 dm 请求 spawn 到阻塞
    /// 线程池调用该句柄（完成后再回到事件循环 send_response），事件循环
    /// 线程不再执行存储 IO；返回 `None` 退化为事件循环内同步 `handle_dm`。
    fn dm_handler(&self) -> Option<Arc<dyn DmHandler>> {
        None
    }

    /// 对端版本观察上报（`/spark/version/1.0.0`）。
    fn on_peer_version(&mut self, _version: &str, _peer_id: &str) {}

    /// 应用层就绪（版本探测成功，对端是 Spark 节点且应用层协议可通信）。
    ///
    /// 与 [`P2pHost::on_peer_connected`]（transport 层 TCP 连接建立）语义区分：
    /// `on_peer_app_ready` 是**业务投递的唯一触发信号**（profile-sync /
    /// flush_pending / device-notice），在 `resolve_version_response` 解析版本
    /// 成功后调用。事件循环线程内调用，保持轻量、禁止阻塞。
    fn on_peer_app_ready(&mut self, _version: &str, _peer_id: &str) {}

    /// 新对端建连（首个连接确认，transport 层语义；事件循环线程内调用，
    /// 保持轻量、禁止阻塞）。不触发任何业务投递——投递统一由
    /// [`P2pHost::on_peer_app_ready`] 驱动。
    fn on_peer_connected(&mut self, _peer_id: &str) {}

    /// 组织私有 DHT 命中的成员提示回填（p2p-messages.md §15）：网关提供的
    /// `{peerId, addresses}` 条目，业务层按未验证口径入邻居池（`verified=false`）；
    /// 组织成员关系以组织记录/成员条目为准（邀请流 + orgsync 收敛），信任边界不变。
    fn on_org_member_hints(&mut self, _hints: &[OrgMemberHint]) {}

    /// 议题元数据公告入站（`spark-affair-meta` 主题 + type='affair-meta'，
    /// affair-metadata §2/§4）：payload 为 §3 公告线形 JSON（已通过信封规则
    /// 与线形 parse 的 p2p 层校验）。p2p 层不落业务库——暂存区/裁决/收录归
    /// 宿主（C10 indexer 角色或客户端元数据缓存）。
    fn on_affair_meta(&mut self, _announce: Value) {}

    /// indexer 查询应答（`/spark/affairmeta/1.0.0` 入站，affair-metadata §8）：
    /// payload 为 search 语义对象，返回结果/错误 payload（p2p 层只包帧）。
    /// 缺省回 unsupported——未实现角色的宿主显式拒绝。
    fn handle_affair_meta_query(&mut self, _payload: &Value) -> Value {
        serde_json::json!({ "error": "unsupported" })
    }

    /// indexer 角色配置（affair-metadata §7 目录面）：Some(覆盖配置) = 角色
    /// 启用（空覆盖 = 全覆盖），None = 未启用。事件循环 tick 据此周期发布
    /// indexer-card 自公告；缺省 None（未实现角色的宿主不自公告）。
    fn indexer_role(&mut self) -> Option<crate::index::directory::IndexCoverage> {
        None
    }

    /// 判断 peer 是否属于优先类目（自设备 / 好友）。返回 true 时该 peer 断开
    /// 后走并行竞速恢复（peer-rediscovery §4.3）；否则维持组织成员串行兜底。
    /// 事件循环线程内调用，实现须保持轻量（KV 读写级）。
    fn is_priority_peer(&mut self, _peer_id: &str) -> bool {
        false
    }

    /// R3 relay 候选梯队①判定（relay-implementation §2）：peer 是否属自设备
    /// （设备清单）或本组织成员（组织成员表端点）。p2p 层经本谓词注入判定，
    /// 不直接依赖 kernel 业务表。事件循环线程内调用，保持轻量（KV/小扫描级）。
    fn is_self_device_or_org_member(&mut self, _peer_id: &str) -> bool {
        false
    }

    /// 判断 peer 是否已被本机撤销（M2 设备撤销四拦截点）。返回 true 时连接层
    /// 立即断开/拒绝入站/拒绝出站。默认 false 表示宿主未启用撤销检查。
    fn is_revoked_peer(&mut self, _peer_id: &str) -> bool {
        false
    }
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
