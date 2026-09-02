//! `/spark/dm/1.0.0` dm（direct message）直连协议帧。
//!
//! dm 信封（chat/read/recall/friend-request/friend-accept）由 kernel 层构造与验签，
//! p2p 层只透明搬运 JSON：请求帧 = 信封本体序列化，响应帧 = 宿主应用层应答序列化，
//! 均不解析字段。

use serde_json::Value;

/// 构造 dm 直连请求帧（payload 为 dm 信封 JSON，透明搬运）。
pub fn build_dm_request(payload: &Value) -> String {
    serde_json::to_string(payload).expect("JSON Value serialization is infallible")
}

/// 解析 dm 直连请求帧：合法 JSON 即放行（字段校验在 kernel 层）。
pub fn parse_dm_request(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(|v| v.is_object())
}

/// 构造 dm 应答侧错误响应帧（宿主拒绝/未实现时）。
pub fn build_dm_error_response(reason: &str) -> String {
    serde_json::json!({"ok": false, "reason": reason}).to_string()
}

/// 解析 dm 直连响应：合法 JSON 对象即视为有应答。
pub fn parse_dm_response(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(|v| v.is_object())
}

/// 应答侧限流的豁免判定：控制类 kind（read/recall/friend-accept）与
/// 自设备同步类 kind（pdsync-hello/need/data、contact-sync、conv-sync、
/// profile-sync、device-sync）豁免，内容型 kind（chat/friend-request/
/// friend-reply/org-invite 等）与未知 kind 保持最小间隔限流。
///
/// 豁免原因：
/// - 控制类信封由「发消息」动作派生连发（如 chat 紧跟 read 回执），
///   共享同一限流桶会让第二条必吃 rate-limited 被对端误标 failed；
/// - 自设备同步是背靠背多信封往返（pdsync 反熵 hello→need→多批 data；
///   配对回发 contact-sync 紧跟 conv-sync），1s 窗口会确定性丢弃第 2 个
///   起的信封，多 category diff 残缺丢失；
/// - **orgsync 成员级豁免**（F8）：orgsync-hello/need/data 是**复制组成员**
///   之间的反熵背靠背信封（hello→need→多批 data，与 pdsync 同构），且：
///   信封由**复制组成员** Ed25519 签名、入站验签失败即被 kernel 丢弃，
///   后续有软限流挂账（防单个成员长期高频淹没复制组）。故同列豁免，否则
///   多集合 diff 的第 2 批起被确定性丢弃、反熵永不收敛。
/// - **orgkey-deliver 豁免**（O5）：encrypted 集合 owner 在 grant/revoke 时向
///   读者定向投递密钥，一次 grant 对 N 读者 × M epoch 连发多条；revoke 对剩余
///   读者各发一条。与同步类同档（控制类派生连发）：共享 1s 限流桶会让第 2 条
///   起必被限流、新读者缺历史 epoch 密钥、revoke 后旧读者收不到新 epoch——
///   密钥是同步收敛的依赖，断一发即不可用。入站验签失败即被 kernel 丢弃
///   （非 owner 无法投毒），软限流挂账同 orgsync。
///
/// 安全取舍：p2p 层限流判定在验签**之前**（验签在 kernel 层入站处理中），
/// 故豁免是全局 kind 豁免而非「验签通过的自设备/好友/成员」豁免——伪造 kind
/// 的未验签信封也能绕过限流进入宿主处理。可接受：绕过后每条仍要付
/// spawn_blocking + ed25519 验签代价并被 kernel 丢弃，限流的防滥用本意
/// （保护宿主处理不被同 peer 高频淹没）对未知/内容类 kind 依旧生效；
/// 且既有 read/recall/friend-accept 豁免已是同口径先例。
///
/// kind 字符串与 kernel 层 `dm_envelope` 的 KIND_* 常量对齐（p2p 层不能
/// 反向依赖 kernel，字面量保持同步）。
pub fn dm_kind_is_rate_limit_exempt(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "read"
                | "recall"
                | "friend-accept"
                | "pdsync-hello"
                | "pdsync-need"
                | "pdsync-data"
                | "pdsync-attachment-req"
                | "pdsync-attachment-resp"
                | "orgsync-hello"
                | "orgsync-need"
                | "orgsync-data"
                | "orgkey-deliver"
                | "contact-sync"
                | "conv-sync"
                | "profile-sync"
                | "device-sync"
                | "feed-blob-req"
                | "feed-blob-resp"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_kinds_are_exempt_chat_and_unknown_are_not() {
        for kind in ["read", "recall", "friend-accept"] {
            assert!(dm_kind_is_rate_limit_exempt(Some(kind)), "{kind} 应豁免");
        }
        for kind in ["chat", "friend-request", "friend-reply", "unknown-kind"] {
            assert!(!dm_kind_is_rate_limit_exempt(Some(kind)), "{kind} 不应豁免");
        }
        assert!(!dm_kind_is_rate_limit_exempt(None), "缺失 kind 不应豁免");
    }

    /// orgkey-deliver 豁免（O5）：owner 向读者逐 epoch 定向投递密钥，一次
    /// grant/revoke 连发多条（同同步类背靠背），断一发即新读者缺历史密钥。
    #[test]
    fn orgkey_deliver_is_rate_limit_exempt() {
        assert!(
            dm_kind_is_rate_limit_exempt(Some("orgkey-deliver")),
            "orgkey-deliver 应豁免（同步类同档）"
        );
    }

    /// 自设备同步类 kind 豁免：pdsync 反熵 hello→need→多批 data 是背靠背
    /// 连发（同一 1s 窗口内 3+ 条），逐条判定都必须豁免，否则第 2 条起被
    /// 限流丢弃、多 category diff 残缺丢失。
    #[test]
    fn self_device_sync_kinds_are_exempt() {
        for kind in [
            "pdsync-hello",
            "pdsync-need",
            "pdsync-data",
            "pdsync-attachment-req",
            "pdsync-attachment-resp",
            "contact-sync",
            "conv-sync",
            "profile-sync",
            "device-sync",
        ] {
            assert!(dm_kind_is_rate_limit_exempt(Some(kind)), "{kind} 应豁免");
        }
    }

    /// S6 feed 三信封限流口径（p2p-dm §19.5/§19.6）：`feed` 计入按 peer 限流
    /// （内容型），`feed-blob-req/resp` 豁免（跨联系人分块传输通道，多信封
    /// 往返，避免被 1s 桶确定性误限流）。
    #[test]
    fn feed_kinds_rate_limit_semantics() {
        assert!(!dm_kind_is_rate_limit_exempt(Some("feed")), "feed 计入限流");
        assert!(
            dm_kind_is_rate_limit_exempt(Some("feed-blob-req")),
            "feed-blob-req 豁免"
        );
        assert!(
            dm_kind_is_rate_limit_exempt(Some("feed-blob-resp")),
            "feed-blob-resp 豁免"
        );
    }
}
