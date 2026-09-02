//! feed 出站投递的共享实现（宿主镜像格句柄，插件后台 capability 复用）。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §6.4/§9] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.5]。
//!
//! 与 [`super::feed_ops`] 门面（持 `&mut Kernel`）语义一致，句柄全来自
//! [`PluginHostShared`]：`feed_deliver_shared` / `feed_pull_shared` /
//! `build_feed_envelope_shared` / `spawn_feed_deliveries_impl` /
//! `enqueue_feed_pending` 等。收件人过滤 [`feed_recipient_filter`]（门面与
//! 共享两路共用）与 feedId 生成 [`generate_feed_id_for_plugin`] 亦归本文件。
//!
//! ## 纪律
//!
//! 纯逻辑层：只操作 `StorageBackend` 泛型与宿主共享句柄，不碰网络、不依赖
//! Tauri。出站 feed 信封 **E2E 加密**（S6 尾修正，2026-08-11 架构师裁决 root
//! 密钥直接转换）：经 `dm_e2e::encrypt_outbound_body` 用「我方 root 私钥 +
//! 对端 root 公钥 X25519」派生临时会话密钥加密 body，携带 `ephPub` 构造签名
//! 信封（对端 root 公钥来自密钥表 `peerRootPub`，入站验签时积累；无记录视为
//! 内部错误）。

use serde_json::Value;
use std::sync::Arc;

use super::Result;
use super::dm_envelope::{self, KIND_FEED};
use super::feed::{FEED_RECIPIENTS_MAX, inbox_pull, plugin_id_of_topic, validate_feed_body};
use super::feed_ops::{FeedDeliverResult, FeedPullResult};
use crate::contact::{ContactService, DmChannel};
use crate::message::PeerRef;
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;
use crate::plugin::PluginHostShared;
use crate::storage::StorageBackend;

/// feed 收件人过滤（纯函数，供 feed_deliver 与测试复用）：去重保序后按
/// feed 通道（`DmChannel::Feed`：拉黑 + 仅聊天 + 非朋友）过滤。返回
/// [`crate::contact::DmRecipientFilter`]（accepted 为放行名单，skipped 为
/// 静默跳过名单）。存储读取失败按空过滤（不阻断投递，错误由下游暴露）。
pub(crate) fn feed_recipient_filter<S: StorageBackend>(
    storage: &S,
    recipients: &[String],
) -> crate::contact::DmRecipientFilter {
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<String> = recipients
        .iter()
        .filter(|r| seen.insert((*r).clone()))
        .cloned()
        .collect();
    ContactService::filter_dm_recipients(storage, &unique, DmChannel::Feed).unwrap_or_default()
}

/// 生成 feedId（缺省时插件后台生成全局唯一 id：`feed_{ts}_{seq}`，1–64 字符；
/// 与壳层命令 generate_feed_id 同口径）。跨线程单调即可，无需跨进程持久。
pub(crate) fn generate_feed_id_for_plugin() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("feed_{ts}_{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 解析 feed 收件人对端的共享实现（存储来自宿主镜像格，与门面版同语义）。
/// 无可寻址端点 → `None`（该 recipient 的投递静默跳过，靠 dm:pending 兜底）。
/// `pub(crate)`：供 feed_ops 内部与 `plugin/host_env`（B3 feed-blob 请求方
/// 发起链路解析来源对端）复用。
pub(crate) fn resolve_feed_recipient_peer_shared<S: StorageBackend>(
    storage: &S,
    root_id: &str,
) -> Result<Option<PeerNodeInfo>> {
    let Some(friend) = ContactService::get_friend(storage, root_id)? else {
        return Ok(None);
    };
    let to_node_info = |p: &PeerRef| PeerNodeInfo {
        peer_id: (!p.peer_id.is_empty()).then(|| p.peer_id.clone()),
        addresses: p.addresses.clone(),
    };
    Ok(friend
        .peers
        .iter()
        .find(|p| !p.addresses.is_empty())
        .or_else(|| friend.peers.first())
        .map(to_node_info))
}

/// 构造 feed 出站 E2E 信封的共享实现（句柄来自宿主镜像格：签名私钥 = 解锁期
/// 共享格，存储 = 镜像格）。与 [`super::feed_ops::Kernel::build_feed_envelope`]
/// 语义一致。
fn build_feed_envelope_shared(
    host: &PluginHostShared,
    from: &str,
    to: &str,
    topic: &str,
    feed_id: &str,
    payload: &Value,
    reply_to: Option<&str>,
) -> crate::plugin::Result<Value> {
    let mut body = serde_json::json!({
        "topic": topic,
        "feedId": feed_id,
        "payload": payload,
    });
    if let Some(r) = reply_to {
        body["replyTo"] = Value::from(r);
    }
    let my_signing_key = host
        .signing_key
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or(crate::plugin::PluginError::InvalidInput(
            "identity locked".to_string(),
        ))?;
    let ts = system_now_ms();
    let node_id = host.sync_node_id();
    let mut storage = host.require_storage()?;
    let (encrypted, eph_pub_b64) = crate::dm_e2e::encrypt_outbound_body(
        &mut storage,
        &my_signing_key,
        from,
        to,
        KIND_FEED,
        ts,
        &body,
        &node_id,
        ts,
    )
    .map_err(|e| {
        crate::plugin::PluginError::InvalidInput(format!("feed e2e encrypt failed: {e}"))
    })?;
    Ok(dm_envelope::build_envelope_with_eph(
        KIND_FEED,
        from,
        to,
        ts,
        encrypted,
        Some(&eph_pub_b64),
        &my_signing_key,
    ))
}

/// feed 投递 spawn 的共享实现：句柄来源（p2p 节点 / 存储 / runtime handle）由
/// 调用方注入——Kernel 门面传 `self.p2p`/`self.storage`，插件后台 capability
/// 传 `PluginHostShared` 对应格。逐条 dm_direct 投递，失败入 `dm:pending:`
/// 离线队列补投（social-feed §6.4）。
pub(crate) fn spawn_feed_deliveries_impl(
    node: Option<Arc<crate::p2p::P2pNode>>,
    storage: Option<crate::kernel::KernelStorage>,
    runtime: tokio::runtime::Handle,
    deliveries: Vec<(PeerNodeInfo, Value, String, String)>,
    node_id: &str,
) {
    let Some(node) = node else {
        // p2p 未启动：全部入离线队列，等上线补投
        if let Some(mut storage) = storage {
            let node_id = node_id.to_string();
            for (_peer, envelope, to, feed_id) in deliveries {
                enqueue_feed_pending(&mut storage, &to, &feed_id, &envelope, &node_id);
            }
        }
        return;
    };
    let mut storage = storage;
    let node_id = node_id.to_string();
    runtime.spawn(async move {
        for (peer, envelope, to, feed_id) in deliveries {
            let resp = node.dm_direct(&peer, envelope.clone()).await.ok().flatten();
            let ok = resp
                .as_ref()
                .and_then(|r| r.get("ok").and_then(Value::as_bool))
                .unwrap_or(false);
            if !ok {
                // 终态拒绝（对端在线但语义性拒绝：blocked/invalid-body 等）：
                // 重试无意义，直接丢弃不入离线队列（I3，对齐 flush 补投口径）。
                let reason = resp
                    .as_ref()
                    .and_then(|r| r.get("reason").and_then(Value::as_str));
                if crate::kernel::dm_delivery::is_terminal_rejection(reason) {
                    continue;
                }
                // 非终态（不可达/超时/rate-limited）：入离线队列补投
                if let Some(s) = storage.as_mut() {
                    enqueue_feed_pending(s, &to, &feed_id, &envelope, &node_id);
                }
            }
        }
    });
}

/// `feed.deliver` 的共享实现（QuickJS 后台 capability 用）：插件线程不持
/// `&mut Kernel`，句柄全来自 [`PluginHostShared`]。与
/// [`super::feed_ops::Kernel::feed_deliver`] 语义一致：body 校验 → **调用级
/// 限流**（同一 `feed_limiter` 实例，与门面共享）→ 收件人过滤 → 逐 accepted
/// 构造 E2E 信封 → spawn 投递。
pub(crate) fn feed_deliver_shared(
    host: &PluginHostShared,
    plugin_id: &str,
    topic: &str,
    feed_id: &str,
    payload: &Value,
    recipients: &[String],
    reply_to: Option<&str>,
) -> crate::plugin::Result<FeedDeliverResult> {
    let _io = host.io_lock.lock().unwrap_or_else(|e| e.into_inner());
    if recipients.is_empty() || recipients.len() > FEED_RECIPIENTS_MAX {
        return Err(crate::plugin::PluginError::InvalidInput(
            "feed recipients out of range".to_string(),
        ));
    }
    // body 校验（出站同入站口径）
    if let Err(_) = validate_feed_body(topic, feed_id, payload, reply_to) {
        return Err(crate::plugin::PluginError::InvalidInput(
            "invalid feed body".to_string(),
        ));
    }
    // 调用级限流（§9.3）：与 Kernel 门面共享同一 `feed_limiter`——iframe 桥
    // 与 QuickJS 后台共用一份配额。topic 前缀即插件归属，取前缀作 pluginId 键。
    {
        let mut limiter = host.feed_limiter.lock().unwrap_or_else(|e| e.into_inner());
        if !limiter.check("personal", plugin_id, system_now_ms()) {
            return Err(crate::plugin::PluginError::RateLimited);
        }
    }
    // 身份：feed 出站需要解锁期 rootId（信封 from）
    let my_root_id = host_my_root(host)?;
    // 收件人过滤（feed 通道：拉黑 + 仅聊天 + 非朋友静默跳过，去重保序）
    let storage = host.require_storage()?;
    let filter = feed_recipient_filter(&storage, recipients);
    let node_id = host.sync_node_id();
    // B1 逐 recipient 隔离：单收件人 E2E 加密失败（无对端 root 公钥等）→
    // 跳过该人（warn）、不计 accepted、不影响其余收件人（try-continue）。
    let mut deliveries = Vec::with_capacity(filter.accepted.len());
    let mut accepted = 0usize;
    for root_id in &filter.accepted {
        if let Some(peer) = resolve_feed_recipient_peer_shared(&storage, root_id)
            .map_err(|e| crate::plugin::PluginError::InvalidInput(e.to_string()))?
        {
            match build_feed_envelope_shared(
                host,
                &my_root_id,
                root_id,
                topic,
                feed_id,
                payload,
                reply_to,
            ) {
                Ok(envelope) => {
                    deliveries.push((peer, envelope, root_id.clone(), feed_id.to_string()));
                    accepted += 1;
                }
                Err(e) => {
                    log::warn!("[feed] skip recipient {root_id}: E2E encrypt failed: {e}");
                }
            }
        }
    }
    let p2p_node = host
        .p2p_node
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let host_storage = host
        .storage
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    spawn_feed_deliveries_impl(
        p2p_node,
        host_storage,
        host.runtime.clone(),
        deliveries,
        &node_id,
    );
    Ok(FeedDeliverResult {
        requested: recipients.len(),
        accepted,
    })
}

/// 读宿主当前 rootId（未解锁/未打开 → 错误，feed 出站需要身份）。
fn host_my_root(host: &PluginHostShared) -> crate::plugin::Result<String> {
    host.my_root_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| crate::plugin::PluginError::InvalidInput("identity locked".to_string()))
}

/// `feed.pull` 的共享实现（QuickJS 后台 capability 用；接收侧免权限）。
pub(crate) fn feed_pull_shared(
    host: &PluginHostShared,
    topic: &str,
    cursor: Option<&str>,
    limit: usize,
) -> crate::plugin::Result<FeedPullResult> {
    let plugin_id = plugin_id_of_topic(topic);
    let storage = host.require_storage()?;
    let (items, next_cursor) = inbox_pull(&storage, plugin_id, cursor, limit.max(1))?;
    Ok(FeedPullResult { items, next_cursor })
}

/// feed 投递失败时把信封入 `dm:pending:` 离线队列（个人空间，复用 feed
/// 统一模型，social-feed §6.4）。任一步失败静默跳过（feed 是尽力而为）。
///
/// `feed_id` 为投递前保存的**明文** feedId（与信封并列由调用方显式传入）——
/// E2E 加密后信封 body 是密文，不能从 body 提取；`message_id` 用它区分同一
/// recipient 的多条 feed pending（修复：多条不再互相覆盖）。
fn enqueue_feed_pending<S: StorageBackend>(
    storage: &mut S,
    to: &str,
    feed_id: &str,
    envelope: &Value,
    node_id: &str,
) {
    use crate::dm_offline::{PendingRecord, PendingSpace, enqueue};
    let now = system_now_ms();
    let record = PendingRecord {
        to: to.to_string(),
        message_id: format!("feed:{feed_id}"),
        kind: KIND_FEED.to_string(),
        space_key: "personal".to_string(),
        conv_id: None,
        envelope: envelope.clone(),
        created_at: now,
    };
    if let Err(e) = enqueue(storage, PendingSpace::Personal, to, &record, node_id, now) {
        eprintln!("[feed-delivery] offline enqueue failed: {e}");
    }
}
