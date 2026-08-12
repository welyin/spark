//! 社交定向投递命令（social-feed S7：`sdk.feed` 壳层薄壳）。
//!
//! 薄壳透传直通内核 `feed_ops` 门面（`Kernel::feed_deliver` / `feed_pull`），
//! 做三件薄事：
//!
//! - **topic 前缀校验**（架构 §8「topic 前缀即插件归属」，内核 feed_ops 契约要求
//!   壳层校验）：出站 `deliver` 的 topic 前缀必须 == 调用方插件 id。插件身份由
//!   桥绑定的 pluginId 传入，**不信插件自报**（桥 dispatcher 按绑定身份注入）。
//! - **feedId 缺省生成**（§9.1 deliver.feedId 可省，缺省壳层生成全局唯一 id）；
//! - **err 映射**（`KernelError` → 前端文案）。
//!
//! 权限（`feed:deliver`）与调用级限流在桥 dispatcher 层强制（对齐既有插件命令
//! 惯例：壳层命令薄壳不做权限）。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::{err, lock_kernel};
use crate::KernelState;

/// feed 投递返回的聚合计数（`{requested, accepted}`，camelCase）。
pub(crate) fn feed_deliver_inner(
    kernel: &mut Kernel,
    plugin_id: &str,
    topic: &str,
    payload: &Value,
    recipients: Vec<String>,
    reply_to: Option<&str>,
    feed_id: Option<&str>,
) -> Result<Value, String> {
    // topic 前缀 == 调用方插件 id（架构 §8 出站侧校验；内核只校验形态/字符集/长度）
    assert_topic_owned(plugin_id, topic)?;
    // feedId 缺省时壳层生成（1–64 字符全局唯一 id）
    let feed_id = match feed_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => generate_feed_id(),
    };
    let result = kernel
        .feed_deliver(topic, &feed_id, payload, &recipients, reply_to)
        .map_err(err)?;
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

/// 收件箱游标补读（`{items, nextCursor?}`；接收侧免权限，桥 dispatcher 不强校验）。
pub(crate) fn feed_pull_inner(
    kernel: &Kernel,
    plugin_id: &str,
    topic: &str,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, String> {
    // topic 前缀 == 调用方插件 id（架构 §8 出站侧校验；B2：pull 亦受此约束，
    // 防任一插件读他人收件箱 `sdk.feed.pull({topic:"spark-moments:posts"})`）。
    // 收件箱键域按 pluginId 划分，前缀校验是读侧归属防越权。
    assert_topic_owned(plugin_id, topic)?;
    let result = kernel
        .feed_pull(topic, cursor, limit.unwrap_or(20))
        .map_err(err)?;
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

/// 校验 topic 前缀 == 调用方插件 id（架构 §8：topic 前缀即插件归属，出站侧校验）。
fn assert_topic_owned(plugin_id: &str, topic: &str) -> Result<(), String> {
    // 前缀取 topic 首个 `:` 前段（对齐内核 plugin_id_of_topic）
    let prefix = topic.split(':').next().unwrap_or(topic);
    if prefix != plugin_id {
        return Err(format!(
            "InvalidTopic: topic prefix \"{prefix}\" does not match plugin \"{plugin_id}\""
        ));
    }
    Ok(())
}

/// 生成 feedId（缺省时壳层生成全局唯一 id：`feed_{ts}_{seq}`，1–64 字符）。
fn generate_feed_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SeqCounter::next();
    format!("feed_{ts}_{seq}")
}

/// 全局单调递增序号（feedId 生成去重兜底；跨调用单调即可，无需跨进程持久）。
struct SeqCounter;
impl SeqCounter {
    fn next() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

/// 社交定向投递（`sdk.feed.deliver`）。插件身份由桥绑定的 pluginId 传入，
/// 不信插件自报。权限（`feed:deliver`）与限流在桥 dispatcher 层强制。
#[tauri::command]
pub fn plugin_feed_deliver(
    state: tauri::State<'_, KernelState>,
    plugin_id: String,
    topic: String,
    payload: Value,
    recipients: Vec<String>,
    reply_to: Option<String>,
    feed_id: Option<String>,
) -> Result<Value, String> {
    feed_deliver_inner(
        &mut *lock_kernel(&state)?,
        &plugin_id,
        &topic,
        &payload,
        recipients,
        reply_to.as_deref(),
        feed_id.as_deref(),
    )
}

/// 收件箱游标补读（`sdk.feed.pull`；接收侧免权限）。
#[tauri::command]
pub fn plugin_feed_pull(
    state: tauri::State<'_, KernelState>,
    plugin_id: String,
    topic: String,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<Value, String> {
    feed_pull_inner(
        &*lock_kernel(&state)?,
        &plugin_id,
        &topic,
        cursor.as_deref(),
        limit,
    )
}

// ------------------------------------------------------------------
// 单元测试（tests.rs）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests;
