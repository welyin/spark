//! query/feed/identity/messages 能力（从 `host_env` 拆出，文件长度硬线）。
//! 零逻辑变化。


use serde_json::Value;

use crate::plugin::error::{PluginError, Result};

use super::{PluginHostShared, required_call_id, required_str};

// ------------------------------------------------------------------
// 宿主查询应答回流
// ------------------------------------------------------------------

impl PluginHostShared {
    /// `query.respond`：JS 侧查询处理完成，结果送回等待中的
    /// `plugin_host_query` 调用方（在途表无记录说明已超时，静默丢弃）。
    pub(super) fn query_respond(&self, payload: &Value) -> Result<Value> {
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
    pub(super) fn feed_deliver(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
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
    pub(super) fn feed_pull(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
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
    pub(super) fn identity_verify(&self, _plugin_id: &str, payload: &Value) -> Result<Value> {
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
    pub(super) fn identity_sign(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
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
    pub(super) fn messages_send_app_message(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
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
