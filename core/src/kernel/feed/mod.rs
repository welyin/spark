//! feed 纯逻辑层（social-feed §9/§8 + p2p-dm §19.5/§19.6）。
//!
//! 对应 [wiki/architecture/plugins/social-feed.md §4.2/§7/§8] 与
//! [wiki/protocol/p2p/p2p-dm.md §19.5/§19.6]。
//!
//! ## 职责
//!
//! - **feed body 校验**（入站与出站共用同一口径）：`topic` 前缀形态/字符集/
//!   长度、`feedId` 1–64、`payload` 紧凑序列化 ≤ 32 KiB、`replyTo` 可省字符串；
//! - **收件箱**：`feed:inbox:{pluginId}:{ts}:{feedId}` 键域——接收方本地
//!   消费缓冲（插件重启/崩溃恢复经 `pull(cursor)` 补读），容量上限
//!   [`FEED_INBOX_CAP`]，超出淘汰最旧（按 ts 升序）；
//! - **查重**：按 `(from, feedId)` 在收件箱内查重（§19.5 应用级幂等）；
//! - **feed-blob 来源登记**：入站 feed 落收件箱时扫描 payload 中
//!   `{$blob: hash}` 引用，写 `feed:blob-src:{hash} → fromRootId`（TTL 30 天，
//!   供请求方判断「该 hash 有 feed 来源」后向来源拉取）。
//!
//! ## 纪律
//!
//! 纯逻辑层：只操作 `StorageBackend` 泛型，`now_ms` 时间注入，不碰网络、
//! 不依赖 Tauri。信封装配与投递在 kernel 编排层（`feed_ops`）。
//!
//! ## 存储键（个人空间，本地消费缓冲，不参与 pdsync 自设备扩散）
//!
//! - 收件箱：`feed:inbox:{pluginId}:{ts}:{feedId}` → [`FeedInboxRecord`]
//! - blob 来源：`feed:blob-src:{hash}` → `fromRootId`（TTL 30 天）

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

// ── 常量 ─────────────────────────────────────────────────────────────

/// feed 入站 `topic` 字段总长上限（p2p-dm §19.5）。
pub const FEED_TOPIC_MAX_CHARS: usize = 128;
/// `feedId` 长度上下限（1–64 字符，发送方生成的全局唯一 id）。
pub const FEED_ID_MIN_CHARS: usize = 1;
pub const FEED_ID_MAX_CHARS: usize = 64;
/// `payload` 紧凑序列化后的体积上限（32 KiB，加密前）。
pub const FEED_PAYLOAD_MAX_BYTES: usize = 32 * 1024;
/// 收件箱容量上限（每 pluginId；超出淘汰最旧——收件箱是缓冲不是队列，
/// 插件 pull 补读前被淘汰的条目以事件广播兜底，不会静默丢失已消费内容）。
pub const FEED_INBOX_CAP: usize = 500;
/// feed-blob 来源登记 TTL（30 天；过期后请求方不再向该来源拉取，hash 即
/// 能力，可由其它持有方或重投兜底）。
pub const FEED_BLOB_SRC_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// feed 信封接收方一次性最大名单（与 social-feed §9.2 `TooManyRecipients`
/// 对齐；壳层在调用级限流，内核做防御性上限）。
pub const FEED_RECIPIENTS_MAX: usize = 500;

// ── feed.deliver 调用级限流（social-feed §9.3）──────────────────────────

/// `feed.deliver` 每 (space, pluginId) 每窗口最多调用次数（§9.3：60s 内 10 次）。
pub const FEED_DELIVER_RATE_LIMIT: u32 = 10;
/// `feed.deliver` 限流窗口（毫秒，固定窗口）。
pub const FEED_DELIVER_RATE_WINDOW_MS: i64 = 60_000;

/// feed.deliver 调用级限流器（内核内存态单点，进程重启清零，§9.3）。
///
/// 每 `(space, pluginId)` 固定窗口 [`FEED_DELIVER_RATE_WINDOW_MS`] 内最多
/// 调用 [`FEED_DELIVER_RATE_LIMIT`] 次；超限拒绝并累计 `rejected`。Kernel
/// 门面（iframe 桥经命令层）与 QuickJS 后台 capability 共享同一实例——桥侧
/// 不重复做限流（决策见 social-feed §13 / S9）。
#[derive(Default)]
pub struct FeedDeliverRateLimiter {
    windows: std::collections::HashMap<String, FeedRateWindow>,
}

struct FeedRateWindow {
    window_start: i64,
    count: u32,
    rejected: u64,
}

impl FeedDeliverRateLimiter {
    /// 判定是否放行本次调用：窗口过期先重置；放行计数 +1，超限拒绝计数 +1。
    pub fn check(&mut self, space: &str, plugin_id: &str, now_ms: i64) -> bool {
        self.evict_if_full(now_ms);
        let window = self
            .windows
            .entry(format!("{space}:{plugin_id}"))
            .or_insert_with(|| FeedRateWindow {
                window_start: now_ms,
                count: 0,
                rejected: 0,
            });
        if now_ms - window.window_start >= FEED_DELIVER_RATE_WINDOW_MS {
            // 固定窗口过期重置（拒绝计数不重置——它是累计观测面）
            window.window_start = now_ms;
            window.count = 0;
        }
        if window.count >= FEED_DELIVER_RATE_LIMIT {
            window.rejected = window.rejected.saturating_add(1);
            return false;
        }
        window.count += 1;
        true
    }

    /// 指定 (space, pluginId) 的累计拒绝数（熔断观测面；测试用）。
    #[allow(dead_code)]
    pub fn rejected_count(&self, space: &str, plugin_id: &str) -> u64 {
        self.windows
            .get(&format!("{space}:{plugin_id}"))
            .map(|w| w.rejected)
            .unwrap_or(0)
    }

    /// 当前窗口内的放行计数（测试观测用）。
    #[allow(dead_code)]
    pub fn count(&self, space: &str, plugin_id: &str) -> u32 {
        self.windows
            .get(&format!("{space}:{plugin_id}"))
            .map(|w| w.count)
            .unwrap_or(0)
    }

    /// 容量守卫：满 1024 键时先回收过期窗口条目，仍满则整体清空（防内存无界）。
    fn evict_if_full(&mut self, now_ms: i64) {
        const RATE_LIMITER_MAX_KEYS: usize = 1024;
        if self.windows.len() < RATE_LIMITER_MAX_KEYS {
            return;
        }
        self.windows
            .retain(|_, w| now_ms - w.window_start < FEED_DELIVER_RATE_WINDOW_MS);
        if self.windows.len() >= RATE_LIMITER_MAX_KEYS {
            self.windows.clear();
        }
    }
}

// ── 存储键 ───────────────────────────────────────────────────────────

/// 收件箱键前缀。
pub const FEED_INBOX_PREFIX: &str = "feed:inbox:";
/// feed-blob 来源登记键前缀。
pub const FEED_BLOB_SRC_PREFIX: &str = "feed:blob-src:";

/// 收件箱键 `feed:inbox:{pluginId}:{ts}:{feedId}`。
///
/// ts 用 13 位零填充（与消息键同口径），保证字典序 == 时间序——cursor 补读
/// 按前缀扫描天然有序。feedId 全局唯一（发送方生成），同 pluginId 下不会
/// 撞键。
pub fn feed_inbox_key(plugin_id: &str, ts: i64, feed_id: &str) -> String {
    format!("{FEED_INBOX_PREFIX}{plugin_id}:{ts:013}:{feed_id}")
}

/// 某 pluginId 的收件箱键前缀（pull 按 topic 前缀过滤用）。
pub fn feed_inbox_plugin_prefix(plugin_id: &str) -> String {
    format!("{FEED_INBOX_PREFIX}{plugin_id}:")
}

/// feed-blob 来源登记键 `feed:blob-src:{hash}`。
pub fn feed_blob_src_key(hash: &str) -> String {
    format!("{FEED_BLOB_SRC_PREFIX}{hash}")
}

/// 从 topic 提取 pluginId（`{pluginId}:{sub}` 形态冒号前段）。
pub fn plugin_id_of_topic(topic: &str) -> &str {
    topic.split(':').next().unwrap_or(topic)
}

// ── 记录 ─────────────────────────────────────────────────────────────

/// 收件箱单条 feed 记录（落库与 pull 返回共用）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedInboxRecord {
    /// 发送方 rootId（信封 from）。
    pub from: String,
    /// topic（`{pluginId}:{sub}`）。
    pub topic: String,
    /// 发送方生成的全局唯一 id。
    pub feed_id: String,
    /// 业务 payload（明文，解密后落库）。
    pub payload: Value,
    /// 回执语义（可选，指向原 feedId）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// 信封时间戳（ms）。
    pub ts: i64,
}

// ── body 校验（入站/出站共用）────────────────────────────────────────

/// 校验 `topic` 形态：`{pluginId}:{sub}`，总长 ≤ 128，字符集 `[a-z0-9:._-]`，
/// 前缀（pluginId）非空。返回 `Err(reason)` 供 `invalid-body` 应答。
pub fn validate_feed_body(
    topic: &str,
    feed_id: &str,
    payload: &Value,
    reply_to: Option<&str>,
) -> Result<(), &'static str> {
    // topic
    if topic.is_empty() || topic.len() > FEED_TOPIC_MAX_CHARS {
        return Err("invalid topic length");
    }
    let plugin_id = plugin_id_of_topic(topic);
    if plugin_id.is_empty() || plugin_id == topic {
        return Err("topic missing pluginId:sub");
    }
    if !topic.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'.' | b'_' | b'-')
    }) {
        return Err("topic invalid charset");
    }
    // feedId
    if feed_id.len() < FEED_ID_MIN_CHARS || feed_id.len() > FEED_ID_MAX_CHARS {
        return Err("invalid feedId length");
    }
    // payload 紧凑序列化 ≤ 32 KiB
    if serde_json::to_string(payload).map_or(true, |s| s.len() > FEED_PAYLOAD_MAX_BYTES) {
        return Err("payload too large");
    }
    // replyTo 可省，纯字符串
    if let Some(r) = reply_to
        && r.is_empty()
    {
        return Err("empty replyTo");
    }
    Ok(())
}

// ── 收件箱 CRUD ──────────────────────────────────────────────────────

/// 收件箱是否已含 `(from, feedId)`（应用级查重，§19.5 幂等）。
/// 已含返回 `true`（调用方按幂等处理：落库重复但不重复事件）。
pub fn inbox_has<S: StorageBackend>(
    storage: &S,
    plugin_id: &str,
    from: &str,
    feed_id: &str,
) -> Result<bool, crate::storage::StorageError> {
    let prefix = feed_inbox_plugin_prefix(plugin_id);
    for (_key, raw) in storage.scan(&ScanOptions::prefix(prefix))? {
        if let Ok(rec) = serde_json::from_str::<FeedInboxRecord>(&raw) {
            if rec.from == from && rec.feed_id == feed_id {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// 落收件箱（幂等：重复 `(from, feedId)` 不重复写）。写入后强制容量收敛
/// （超 `FEED_INBOX_CAP` 淘汰最旧）。
pub fn inbox_put<S: StorageBackend>(
    storage: &mut S,
    plugin_id: &str,
    ts: i64,
    rec: &FeedInboxRecord,
) -> Result<(), crate::storage::StorageError> {
    let key = feed_inbox_key(plugin_id, ts, &rec.feed_id);
    // FeedInboxRecord 全字段可序列化，to_string 不可失败（feedId 等均由调用方
    // 校验过长度/类型）
    storage.put(
        &key,
        &serde_json::to_string(rec).expect("feed inbox record serializable"),
    )?;
    enforce_inbox_cap(storage, plugin_id)
}

/// pull 游标补读（social-feed §9.1 `pull(cursor)`）：按 pluginId 前缀扫描，
/// 以 `(from, feedId)` 排除发送方自己（拉取侧只补读他人投递），按 ts 升序
/// 返回，`cursor` 为上一页最后一条的 `{pluginId}:{ts}:{feedId}` 键（字符串，
/// 字典序直接可比），`limit` 分页上限。返回 `(records, next_cursor)`。
pub fn inbox_pull<S: StorageBackend>(
    storage: &S,
    plugin_id: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<FeedInboxRecord>, Option<String>), crate::storage::StorageError> {
    let prefix = feed_inbox_plugin_prefix(plugin_id);
    let mut rows: Vec<(String, FeedInboxRecord)> = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(prefix))? {
        if let Some(c) = cursor
            && key.as_str() <= c
        {
            continue; // 游标之后的条目
        }
        if let Ok(rec) = serde_json::from_str::<FeedInboxRecord>(&raw) {
            rows.push((key, rec));
        }
    }
    // 键含 13 位零填充 ts，字典序即时间序；游标补读按序返回
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let records: Vec<FeedInboxRecord> = rows.iter().map(|(_, r)| r.clone()).collect();
    let next_cursor = if records.len() > limit {
        rows.get(limit - 1).map(|(k, _)| k.clone())
    } else {
        None
    };
    let page = records.into_iter().take(limit).collect();
    Ok((page, next_cursor))
}

/// 强制容量：超 `FEED_INBOX_CAP` 淘汰最旧（键字典序 = ts 升序，删最早）。
fn enforce_inbox_cap<S: StorageBackend>(
    storage: &mut S,
    plugin_id: &str,
) -> Result<(), crate::storage::StorageError> {
    let prefix = feed_inbox_plugin_prefix(plugin_id);
    let mut keys: Vec<String> = Vec::new();
    for (key, _) in storage.scan(&ScanOptions::prefix(prefix))? {
        keys.push(key);
    }
    keys.sort();
    let excess = keys.len().saturating_sub(FEED_INBOX_CAP);
    for key in keys.into_iter().take(excess) {
        storage.delete(&key)?;
    }
    Ok(())
}

// ── feed-blob 来源登记 ───────────────────────────────────────────────

/// 登记 feed 入站引用的 blob 来源（`feed:blob-src:{hash} → from`，TTL 30 天）。
/// 幂等（同 hash 重复登记覆盖时间戳）。从 payload 递归提取 `{$blob: hash}`。
pub fn register_blob_sources<S: StorageBackend>(
    storage: &mut S,
    from: &str,
    payload: &Value,
    now_ms: i64,
) -> Result<(), crate::storage::StorageError> {
    for hash in crate::plugindata::blob::blob_refs_in(payload) {
        let key = feed_blob_src_key(&hash);
        let raw = storage.get(&key)?.unwrap_or_default();
        let since = raw
            .split_once(':')
            .map(|(_, t)| t.parse::<i64>().unwrap_or(0))
            .unwrap_or(0);
        // LWW：保留最新来源（TTL 以最近登记时间为准）。
        // 本函数对 payload 内每个 `$blob` 引用 hash 都做登记（刷新），故 put 与
        // delete 互斥——绝不因旧 since 判定过期而删除刚写入的记录（I1：严格
        // 过期后重登记会被自杀清理误删的 bug）。过期清理由 `blob_source` 读侧
        // 判定（TTL 过期返回 None），写侧无需在此清空。
        if now_ms >= since {
            storage.put(&key, &format!("{from}:{now_ms}"))?;
        }
    }
    Ok(())
}

/// 查某 hash 的 feed 来源 rootId（未登记/过期 → `None`）。供拉取侧
/// `readBlob(hash)` 未命中时决定向哪个来源 rootId 发 feed-blob-req。
/// 由 kernel 门面（feed-blob 请求方 `readBlob` 接线，S7 壳层）调用——
/// 本模块只提供存储语义。
pub fn blob_source<S: StorageBackend>(
    storage: &S,
    hash: &str,
    now_ms: i64,
) -> Result<Option<String>, crate::storage::StorageError> {
    let Some(raw) = storage.get(&feed_blob_src_key(hash))? else {
        return Ok(None);
    };
    let Some((src, since)) = raw.split_once(':') else {
        return Ok(None);
    };
    let since = since.parse::<i64>().unwrap_or(0);
    if now_ms.saturating_sub(since) > FEED_BLOB_SRC_TTL_MS || src.is_empty() {
        Ok(None)
    } else {
        Ok(Some(src.to_string()))
    }
}

#[cfg(test)]
mod tests;
