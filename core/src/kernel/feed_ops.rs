//! feed 门面（`Kernel::feed_deliver` / `feed_pull`）：社交定向投递出站编排。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §5/§8/§9] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.5]。
//!
//! - `feed_deliver`：body 校验 → `filter_dm_recipients`（feed 通道：拉黑 + 仅
//!   聊天 + 非朋友**静默跳过**）→ 逐 recipient 解析对端 + 构造 feed 信封 →
//!   spawn 投递（失败入 `dm:pending:` 统一离线队列补投）→ 返回
//!   `{requested, accepted}` 聚合计数；
//! - `feed_pull`：收件箱游标补读（`feed:inbox:{pluginId}:{ts}:{feedId}` 键域，
//!   插件重启/崩溃恢复路径）。
//!
//! 出站投递的共享实现（插件后台 capability 复用，句柄来自宿主镜像格）在
//! [`super::feed_shared`]：`feed_deliver_shared` / `feed_pull_shared` /
//! `spawn_feed_deliveries_impl` / 收件人过滤等。
//!
//! ## 纪律
//!
//! 权限校验（`feed:deliver`）在壳层，本内核门面只做 body 校验与收件人过滤。
//! 出站 feed 信封 **E2E 加密**（S6 尾修正，2026-08-11 架构师裁决 root 密钥
//! 直接转换）：经 `dm_e2e::encrypt_outbound_body` 用「我方 root 私钥 + 对端
//! root 公钥 X25519」派生临时会话密钥加密 body，携带 `ephPub` 构造签名信封
//! （对端 root 公钥来自密钥表 `peerRootPub`，入站验签时积累；无记录视为内部
//! 错误）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::dm_envelope::{self, KIND_FEED};
use super::feed::{FEED_RECIPIENTS_MAX, FeedInboxRecord, inbox_pull, plugin_id_of_topic, validate_feed_body};
use super::feed_shared::{
    feed_recipient_filter, resolve_feed_recipient_peer_shared, spawn_feed_deliveries_impl,
};
use super::{Kernel, KernelError, Result};
use crate::p2p::node::system_now_ms;
use crate::p2p::PeerNodeInfo;

/// feed 投递返回的聚合计数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedDeliverResult {
    /// 入参 recipients 总数。
    pub requested: usize,
    /// 实际进入投递的收件人数（被拉黑/仅聊天/非朋友静默跳过不计入）。
    pub accepted: usize,
}

/// feed_pull 返回。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPullResult {
    pub items: Vec<FeedInboxRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl Kernel {
    /// 社交定向投递（social-feed §9.1 `deliver`）。权限（`feed:deliver`）与
    /// 调用级限流在壳层；本内核门面做 body 校验 + 收件人过滤 + 信封构造 +
    /// spawn 投递。返回 `{requested, accepted}`（被静默跳过的收件人不计入
    /// accepted，也不逐人暴露原因）。
    ///
    /// - `topic` 须为 `{pluginId}:{sub}`（出站前缀 == 调用方插件 id 由壳层
    ///   校验，内核校验形态/字符集/长度）；
    /// - `feed_id` 发送方生成全局唯一 id（可省时壳层生成）；
    /// - `payload` 紧凑序列化 ≤ 32 KiB；
    /// - `recipients` ≤ [`FEED_RECIPIENTS_MAX`]（超过按错误拒绝）。
    pub fn feed_deliver(
        &mut self,
        topic: &str,
        feed_id: &str,
        payload: &Value,
        recipients: &[String],
        reply_to: Option<&str>,
    ) -> Result<FeedDeliverResult> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let my_root_id = self.require_unlocked_root_id()?;
        if recipients.is_empty() || recipients.len() > FEED_RECIPIENTS_MAX {
            return Err(KernelError::Internal("feed recipients out of range".to_string()));
        }
        // body 校验（出站同入站口径）
        if let Err(_) = validate_feed_body(topic, feed_id, payload, reply_to) {
            return Err(KernelError::Internal("invalid feed body".to_string()));
        }
        // 调用级限流（§9.3，内核单点）：每 (space, pluginId) 60s 内 10 次。
        // feed 是个人空间功能，space 恒为 "personal"。限流器与 QuickJS 后台
        // capability 共享同一实例（self.plugin_host.feed_limiter）。
        {
            let mut limiter = self
                .plugin_host
                .feed_limiter
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !limiter.check("personal", plugin_id_of_topic(topic), system_now_ms()) {
                return Err(KernelError::RateLimited);
            }
        }
        // 收件人过滤（feed 通道：拉黑 + 仅聊天 + 非朋友静默跳过，去重保序）
        let filter = feed_recipient_filter(self.require_storage()?, recipients);
        let node_id = self.sync_node_id();
        // 逐 accepted recipient：解析对端 + 构造 feed 信封，spawn 投递。
        // 明文 feedId 与信封一起入元组——投递失败入 `dm:pending:` 时按它生成
        // message_id 键（E2E 加密后信封 body 是密文，不能从 body 取 feedId）。
        // B1 逐 recipient 隔离：单收件人 E2E 加密失败（无对端 root 公钥等）→
        // 跳过该人（warn）、不计 accepted、不影响其余收件人（try-continue）。
        let mut deliveries = Vec::with_capacity(filter.accepted.len());
        let mut accepted = 0usize;
        for root_id in &filter.accepted {
            if let Some(peer) = self.resolve_feed_recipient_peer(root_id)? {
                match self.build_feed_envelope(&my_root_id, root_id, topic, feed_id, payload, reply_to) {
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
        // spawn 投递（失败入 dm:pending 统一队列补投）；accepted 按「放行」
        // 口径（过滤通过且成功构造信封即计入 accepted——加密失败/无可寻址
        // 端点的收件人不计，离线补投兜底不改变投递意图）
        self.spawn_feed_deliveries(deliveries, &node_id);
        Ok(FeedDeliverResult {
            requested: recipients.len(),
            accepted,
        })
    }

    /// 收件箱游标补读（social-feed §9.1 `pull`）。`topic` 前缀匹配 pluginId，
    /// `cursor` 为上一页最后一条的 `{pluginId}:{ts}:{feedId}` 键（字典序可比），
    /// `limit` 分页上限。收件箱是本地消费缓冲（`feed:inbox:` 键域，不参与
    /// pdsync），接收侧免权限。
    pub fn feed_pull(
        &self,
        topic: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<FeedPullResult> {
        let plugin_id = plugin_id_of_topic(topic);
        let (items, next_cursor) = inbox_pull(
            self.require_storage()?,
            plugin_id,
            cursor.as_deref(),
            limit.max(1),
        )?;
        Ok(FeedPullResult { items, next_cursor })
    }

    /// 解析 feed 收件人对端（个人空间朋友 peer 择优：带地址优先，否则取首个
    /// peerId——已连接时 dm_direct 短路直发）。无可寻址端点 → `None`（该
    /// recipient 的投递静默跳过，靠收件人上线后由 dm:pending 补投兜底——
    /// feed 是尽力而为 + 最终一致）。共享实现见 [`resolve_feed_recipient_peer_shared`]。
    fn resolve_feed_recipient_peer(&self, root_id: &str) -> Result<Option<PeerNodeInfo>> {
        resolve_feed_recipient_peer_shared(self.require_storage()?, root_id)
    }

    /// 构造 feed 出站信封（S6 尾→S7：**E2E 加密**）。body 线形
    /// `{topic, feedId, payload, replyTo?}`（p2p-dm §19.5，加密前）——
    /// 经 [`crate::dm_e2e::encrypt_outbound_body`] 用「我方 root 私钥 + 对端
    /// root 公钥 X25519」派生临时会话密钥加密，携带 `ephPub` 构造签名信封。
    ///
    /// 对端 root 公钥来自密钥表 `peerRootPub`（入站验签时积累）；无记录视为
    /// 内部错误（不静默降级明文）。feed 属个人空间 1:1 定向投递，与 chat 共用
    /// 同一份方向无关会话密钥。
    pub(crate) fn build_feed_envelope(
        &mut self,
        from: &str,
        to: &str,
        topic: &str,
        feed_id: &str,
        payload: &Value,
        reply_to: Option<&str>,
    ) -> Result<Value> {
        let mut body = serde_json::json!({
            "topic": topic,
            "feedId": feed_id,
            "payload": payload,
        });
        if let Some(r) = reply_to {
            body["replyTo"] = Value::from(r);
        }
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        // 克隆签名私钥，避免 `require_storage_raw_mut` 可变借用与 `self.unlocked`
        // 不可变借用冲突。
        let my_signing_key = unlocked.identity.signing_key.clone();
        let ts = system_now_ms();
        let node_id = self.sync_node_id();
        // E2E 加密：读对端 root 公钥 → ensure → 临时密钥对 → encrypt。
        // 失败（无对端 root 公钥记录）→ 内部错误，不静默降级明文。
        let (encrypted, eph_pub_b64) = crate::dm_e2e::encrypt_outbound_body(
            self.require_storage_raw_mut()?,
            &my_signing_key,
            from,
            to,
            KIND_FEED,
            ts,
            &body,
            &node_id,
            ts,
        )
        .map_err(|e| KernelError::Internal(format!("feed e2e encrypt failed: {e}")))?;
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
}

// ── feed 投递 spawn（失败入 dm:pending 统一离线队列）────────────────────

impl Kernel {
    /// spawn feed 投递任务（尽力而为 + 最终一致，不捕获 `&Kernel`）：逐条
    /// dm_direct 投递，失败（不可达/超时）把密文信封入 `dm:pending:`
    /// 离线队列（个人空间，feed 复用）补投——任一台在线设备上线 flush。
    /// feed 无消息状态可回写（区别于 chat），投递成功即出队由 flush 处理。
    fn spawn_feed_deliveries(&self, deliveries: Vec<(PeerNodeInfo, Value, String, String)>, node_id: &str) {
        let node = self.p2p.clone();
        let storage = self.storage.clone();
        spawn_feed_deliveries_impl(node, storage, self.runtime.handle().clone(), deliveries, node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::{ContactService, DmRecipientSkipReason, FriendRecord};
    use crate::storage::{MemoryStorage, StorageBackend};

    fn friend(root_id: &str, permission: &str) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            permission: permission.to_string(),
            ..Default::default()
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// feed 通道过滤：拉黑/仅聊天/非朋友静默跳过（不计 accepted），正常朋友
    /// 放行；去重保序。返回 accepted 名单（feed_deliver 的 accepted 语义）。
    #[test]
    fn feed_filter_skips_blocked_chatonly_and_nonfriend() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("root-open", "open")).unwrap();
        ContactService::upsert_friend(&mut s, &friend("root-chatonly", "chatOnly")).unwrap();
        s.put("ct:blocked:root-blocked", "1").unwrap();
        let recipients = ids(&["root-open", "root-chatonly", "root-blocked", "root-stranger", "root-open"]);
        let filter = feed_recipient_filter(&s, &recipients);
        // 只有 open 放行（去重后）；其余静默跳过
        assert_eq!(filter.accepted, vec!["root-open".to_string()]);
        let reasons: Vec<DmRecipientSkipReason> = filter.skipped.iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&DmRecipientSkipReason::ChatOnly));
        assert!(reasons.contains(&DmRecipientSkipReason::Blocked));
        assert!(reasons.contains(&DmRecipientSkipReason::NotFriend));
        // feed_deliver 的 accepted 语义 = 放行数（去重后），requested = 入参总数
        assert_eq!(filter.accepted.len(), 1);
        assert_eq!(recipients.len(), 5, "requested 按入参总数");
    }
}
