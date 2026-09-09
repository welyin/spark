//! dm 信封（`/spark/dm/1.0.0` 承载的应用层信封）的构造与校验。
//!
//! 线形 JSON：
//! ```json
//! { "kind": "chat|read|recall|friend-request|friend-accept|friend-reply|org-invite|org-invite-reply|org-member-removed",
//!   "from": "<rootId>", "to": "<rootId>", "ts": 123,
//!   "body": { ... },
//!   "ephPub": "<可选，base64 临时 X25519 公钥，见密钥轮换>",
//!   "pubKey": "<base64>", "sig": "<base64>" }
//! ```
//!
//! - 签名载荷 = 固定键序紧凑 JSON 串，签其 UTF-8 字节（serde_json
//!   `preserve_order`：按序构建 `Map` 序列化即确定性）。**无 `ephPub`** 时为
//!   body/from/kind/to/ts（向后兼容纯域身份 DH 信封）；**携带 `ephPub`** 时
//!   插在 body 之后——body/ephPub/from/kind/to/ts（`ephPub` 参与签名，防中间人
//!   替换临时公钥，见 p2p-dm §19.1.1 密钥轮换）。构造用
//!   [`build_envelope_with_eph`] / [`build_signing_payload_with_eph`]；
//! - `pubKey` 为根身份 ed25519 公钥原始 32 字节的 base64（与
//!   [`crate::identity::verify_ed25519_signature`] 口径一致），
//!   `from` = sha256hex(pubKey)（rootId 定义，`Identity::id`）；
//! - 入站校验：字段齐全 → `to` 指向本机 → ts 新鲜度（±10 min，防重放）→
//!   pubKey 与 from 绑定 → 验签。验签载荷重建含 ephPub（若携带），验签通过后
//!   [`VerifiedDm::eph_pub`] 回传供接线层解密。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::identity::verify_ed25519_signature;

/// 信封 kind：聊天消息。
pub const KIND_CHAT: &str = "chat";
/// 信封 kind：已读回执。
pub const KIND_READ: &str = "read";
/// 信封 kind：撤回通知。
pub const KIND_RECALL: &str = "recall";
/// 信封 kind：好友申请。
pub const KIND_FRIEND_REQUEST: &str = "friend-request";
/// 信封 kind：好友申请通过。
pub const KIND_FRIEND_ACCEPT: &str = "friend-accept";
/// 信封 kind：资料同步（朋友建连后/资料变更后互推 nickname/avatar）。
pub(crate) const KIND_PROFILE_SYNC: &str = "profile-sync";
/// 信封 kind：设备信息同步（自设备间交换设备清单记录，from==to==自己 rootId；
/// body 为完整 DeviceRecord 线形）。
pub(crate) const KIND_DEVICE_SYNC: &str = "device-sync";
/// 信封 kind：设备加入/变更通知（自设备间，from==to==自己 rootId；body 携带
/// {kind:"device_joined", deviceId, deviceName, ts}）。
pub(crate) const KIND_DEVICE_NOTICE: &str = "system/device-notice";
/// 信封 kind：M5 延迟恢复通道（自设备间，from==to==自己 rootId；body 分
/// initiated / vetoed / committed 三态，见 recovery 模块与 p2p-dm.md §19）。
pub(crate) const KIND_RECOVERY: &str = crate::recovery::RECOVERY_KIND;
/// 信封 kind：通讯录快照同步（自设备间全量快照 LWW 收敛，from==to==自己
/// rootId；body 为 contact/service/sync.rs 的快照线形：friends/requestsIn/
/// requestsOut 记录级 LWW + tags/groups/blocked 整域版本 LWW）。
pub(crate) const KIND_CONTACT_SYNC: &str = "contact-sync";
/// 信封 kind：会话元数据同步（自设备间，from==to==自己 rootId；body 为
/// message/sync.rs 的快照线形：direct 会话外壳 + 置顶/免打扰/草稿按
/// metaUpdatedAt 记录级 LWW；消息本体与未读数不同步）。
pub(crate) const KIND_CONV_SYNC: &str = "conv-sync";
/// 信封 kind：组织邀请（body 携带 inviteCode 与自报展示字段）。
pub const KIND_ORG_INVITE: &str = "org-invite";
/// 信封 kind：组织邀请回执（被邀请人接受/拒绝）。
pub const KIND_ORG_INVITE_REPLY: &str = "org-invite-reply";
/// 信封 kind：成员移除定向通知（admin → 被移除成员，阶段四A P2 L3；body
/// `{orgId, targetPeerIds}`——targetPeerIds 为发送侧离线补投的寻址快照，
/// 收端不消费）。替代 legacy org-pull `removed` 状态的剔除传播。
pub const KIND_ORG_MEMBER_REMOVED: &str = "org-member-removed";
/// 信封 kind：好友申请回复（接收方询问 / 申请方回答同 kind，接收侧按本端
/// 记录匹配方向）。
pub const KIND_FRIEND_REPLY: &str = "friend-reply";
/// 信封 kind：pdsync 摘要交换（自设备间，from==to==自己 rootId；body 携带
/// `categories` 折叠 vv + `msgWindow` + `attachmentPolicy`，见 pdsync §5.1）。
pub const KIND_PDSYNC_HELLO: &str = "pdsync-hello";
/// 信封 kind：pdsync diff 请求（自设备间；body 携带 `category` + `knownVv`，
/// 请求方本地落后时发出，对端据此补增量，见 pdsync §5.2）。
pub const KIND_PDSYNC_NEED: &str = "pdsync-need";
/// 信封 kind：pdsync 数据传输（自设备间；body 携带 `category` + 逐条
/// `{key,value,meta}` + 批次号，接收方逐条 `apply_personal_remote`，见
/// pdsync §5.3）。
pub const KIND_PDSYNC_DATA: &str = "pdsync-data";
/// 信封 kind：P6 blob 按需拉取请求（自设备间；body `{hash, offset}`，
/// 见 plugin-data-api §4）。
pub const KIND_PDSYNC_ATTACHMENT_REQ: &str = "pdsync-attachment-req";
/// 信封 kind：P6 blob 分块响应（body `{hash, offset, data, totalBytes}`；
/// 块长 3 的倍数，接收侧 base64 直接追加拼接，收齐后 SHA-256 校验提升）。
pub const KIND_PDSYNC_ATTACHMENT_RESP: &str = "pdsync-attachment-resp";
/// 信封 kind：A1 blob 层按需回补请求（自设备间；body `{chunkCid, offset?}`
/// 或 `{cid}` 拉 manifest，见 personal-data-sync §14.5）。
pub const KIND_BLOB_FETCH: &str = "blob-fetch";
/// 信封 kind：A1 blob 层回补响应（body `{chunkCid, data, offset?, totalBytes?}` /
/// `{cid, manifest}` / `{..., missing:true}`；收齐后 SHA-256 校验落库并刷新
/// presence，见 personal-data-sync §14.5–14.6）。
pub const KIND_BLOB_CHUNK: &str = "blob-chunk";

// ── S6 feed 三信封（social-feed §4.2 / p2p-dm §19.5/§19.6）────────────

/// 信封 kind：社交定向投递（body `{topic, feedId, payload, replyTo?}`，
/// 加密前线形见 p2p-dm §19.5；把业务 payload 定向投递给 `to`）。
pub const KIND_FEED: &str = "feed";
/// 信封 kind：跨联系人 blob 拉取请求（body `{hash, offset}`，向 feed 原
/// 作者取件；线形对齐 pdsync-attachment-req，见 p2p-dm §19.6）。
pub const KIND_FEED_BLOB_REQ: &str = "feed-blob-req";
/// 信封 kind：feed-blob 分块响应（body `{hash, offset, data, totalBytes,
/// missing?}`；线形对齐 pdsync-attachment-resp，见 p2p-dm §19.6）。
pub const KIND_FEED_BLOB_RESP: &str = "feed-blob-resp";

// ── O2a orgsync 三信封 ────────────────────────────────────────────────

/// 信封 kind：orgsync 摘要交换（复制组成员间；body 携带 orgId +
/// collections 折叠 vv/dlogAck + roles + deviceClass，见 orgsync §20.3）。
pub const KIND_ORGSYNC_HELLO: &str = "orgsync-hello";
/// 信封 kind：orgsync diff 请求（复制组成员间；body 携带 orgId +
/// collection + knownVv + dlogAck，见 orgsync §20.4）。
pub const KIND_ORGSYNC_NEED: &str = "orgsync-need";
/// 信封 kind：orgsync 数据传输（复制组成员间；body 携带 orgId +
/// collection + 逐条 records + 批次号，见 orgsync §20.4）。
pub const KIND_ORGSYNC_DATA: &str = "orgsync-data";

// ── O3 orgq 按需查询 / 写入受理 ─────────────────────────────────────────

/// 信封 kind：orgq 请求（成员 → 数据账号；按需查询 / 写入受理，见
/// orgsync §20.5）。**不做全豁免**——成员级流量，沿用 dm 按 from 限流。
pub const KIND_ORGQ_REQ: &str = "orgq-req";
/// 信封 kind：orgq 响应（数据账号 → 成员；查询应答 / 写入回执，见
/// orgsync §20.5）。沿用 dm 按 from 限流（数据账号侧应答非背靠背多信封）。
pub const KIND_ORGQ_RESP: &str = "orgq-resp";

// ── C4/C5 affairsync 三信封（事务复制面，关注者反熵）─────────────────────
// 常量本体定义在 sync::affairsync::envelope（与该面 build/parse 同处），此处
// 转引对齐（KIND_RECOVERY 同先例），p2p 限流豁免清单用同族字面量。

/// 信封 kind：affairsync 摘要交换（关注者间；body 携带 affairId + 折叠 vv +
/// DAG 头集合 + deviceClass，见 affair-sync §3.1）。
pub const KIND_AFFAIRSYNC_HELLO: &str = crate::sync::affairsync::KIND_AFFAIRSYNC_HELLO;
/// 信封 kind：affairsync diff 请求（关注者间；body 携带 affairId + knownVv，
/// 见 affair-sync §3.2）。
pub const KIND_AFFAIRSYNC_NEED: &str = crate::sync::affairsync::KIND_AFFAIRSYNC_NEED;
/// 信封 kind：affairsync 数据传输（关注者间；body 携带 affairId + 逐条
/// records + 批次号，无 dseq——affair 域纯 append-only 无墓碑面，见
/// affair-sync §3.3）。
pub const KIND_AFFAIRSYNC_DATA: &str = crate::sync::affairsync::KIND_AFFAIRSYNC_DATA;

/// 签名载荷：固定键序 body/from/kind/to/ts 的紧凑 JSON 串（无 `ephPub`，
/// 向后兼容纯域身份 DH 信封）。内部委托 [`build_signing_payload_with_eph`]
/// 传 `None`，见其注释关于 ephPub 键序的说明。
pub fn build_signing_payload(kind: &str, from: &str, to: &str, ts: i64, body: &Value) -> String {
    build_signing_payload_with_eph(kind, from, to, ts, body, None)
}

/// 签名载荷（支持可选 ephPub）：固定键序紧凑 JSON 串。无 `eph_pub` 时为
/// body/from/kind/to/ts；携带 `eph_pub` 时插在 body 之后——
/// body/ephPub/from/kind/to/ts（`ephPub` 参与签名，防中间人替换临时公钥，
/// 见 p2p-dm §19.1.1 密钥轮换）。
pub fn build_signing_payload_with_eph(
    kind: &str,
    from: &str,
    to: &str,
    ts: i64,
    body: &Value,
    eph_pub: Option<&str>,
) -> String {
    let mut map = Map::new();
    map.insert("body".to_string(), body.clone());
    if let Some(eph) = eph_pub {
        map.insert("ephPub".to_string(), Value::from(eph));
    }
    map.insert("from".to_string(), Value::from(from));
    map.insert("kind".to_string(), Value::from(kind));
    map.insert("to".to_string(), Value::from(to));
    map.insert("ts".to_string(), Value::from(ts));
    serde_json::to_string(&Value::Object(map)).expect("dm signing payload is always serializable")
}

/// 构造并签名完整信封（出站侧，无 `ephPub`，向后兼容纯域身份 DH 信封）。
/// 内部委托 [`build_envelope_with_eph`] 传 `None`。
pub fn build_envelope(
    kind: &str,
    from: &str,
    to: &str,
    ts: i64,
    body: Value,
    signing_key: &SigningKey,
) -> Value {
    build_envelope_with_eph(kind, from, to, ts, body, None, signing_key)
}

/// 构造并签名完整信封（出站侧，支持可选 ephPub）。`eph_pub` 为可选外层字段
/// （临时 X25519 公钥，base64，与 pubKey/sig 并列且参与签名）；`None` 时保持
/// 原 5 键序（向后兼容纯域身份 DH 信封）。
pub fn build_envelope_with_eph(
    kind: &str,
    from: &str,
    to: &str,
    ts: i64,
    body: Value,
    eph_pub: Option<&str>,
    signing_key: &SigningKey,
) -> Value {
    let payload = build_signing_payload_with_eph(kind, from, to, ts, &body, eph_pub);
    let signature = signing_key.sign(payload.as_bytes());
    let mut map = Map::new();
    map.insert("kind".to_string(), Value::from(kind));
    map.insert("from".to_string(), Value::from(from));
    map.insert("to".to_string(), Value::from(to));
    map.insert("ts".to_string(), Value::from(ts));
    map.insert("body".to_string(), body);
    if let Some(eph) = eph_pub {
        map.insert("ephPub".to_string(), Value::from(eph));
    }
    map.insert(
        "pubKey".to_string(),
        Value::from(B64.encode(signing_key.verifying_key().to_bytes())),
    );
    map.insert(
        "sig".to_string(),
        Value::from(B64.encode(signature.to_bytes())),
    );
    Value::Object(map)
}

/// friend-reply 的 body 线形 `{requestId, text}`：出站构造（contact_ops）与
/// 入站测试共用同一构造函数，防两侧键名漂移。
pub fn friend_reply_body(request_id: &str, text: &str) -> Value {
    Value::Object(Map::from_iter([
        ("requestId".to_string(), Value::from(request_id)),
        ("text".to_string(), Value::from(text)),
    ]))
}

/// 信封时间戳新鲜度窗口（±10 分钟，与 node-challenge 窗口口径一致）：
/// ts 参与签名但此前从不校验，重放旧信封可绕过一切内容校验。
pub const ENVELOPE_TS_WINDOW_MS: i64 = 10 * 60_000;

/// 校验通过的入站信封。
#[derive(Clone, Debug)]
pub struct VerifiedDm {
    pub kind: String,
    /// 发送方 rootId（已与 pubKey 绑定校验）。
    pub from: String,
    pub ts: i64,
    pub body: Value,
    /// 外层可选临时 X25519 公钥（base64，32 字节）。携带时已参与验签（防中间人
    /// 替换）；`None` 表示对端未升级（纯域身份 DH 信封）。供接线层解密用。
    pub eph_pub: Option<String>,
}

/// 入站信封校验；任一失败返回 `Err(reason)`（reason 供 `{"ok":false,"reason"}`
/// 应答原样回传）。ts 与 `now_ms` 偏差超过 [`ENVELOPE_TS_WINDOW_MS`] 拒绝
/// （reason `stale`，防重放）。
pub fn verify_envelope(
    payload: &Value,
    my_root_id: &str,
    now_ms: i64,
) -> Result<VerifiedDm, String> {
    let invalid = || "invalid-envelope".to_string();
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let from = payload
        .get("from")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let to = payload
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let ts = payload
        .get("ts")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    let body = payload
        .get("body")
        .filter(|v| v.is_object())
        .ok_or_else(invalid)?;
    let pub_key = payload
        .get("pubKey")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let sig = payload
        .get("sig")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    // 可选外层字段 ephPub（base64 临时 X25519 公钥）：携带时参与签名，防中间人
    // 替换临时公钥。线形键序随 build_signing_payload 处理（body/ephPub/.../ts）。
    let eph_pub = payload
        .get("ephPub")
        .and_then(Value::as_str)
        .map(String::from);

    if to != my_root_id {
        return Err("not-for-me".to_string());
    }
    // ts 对端可控：非正时间戳直接拒绝；窗口比较用饱和算术，避免
    // `now_ms - ts` 在极端值（如 i64::MIN）下溢出 panic / 回绕绕过窗口
    if ts <= 0 {
        return Err("stale".to_string());
    }
    if now_ms.saturating_sub(ts).saturating_abs() > ENVELOPE_TS_WINDOW_MS {
        return Err("stale".to_string());
    }
    let pub_key_bytes = B64.decode(pub_key).map_err(|_| "bad-pubkey".to_string())?;
    if hex::encode(Sha256::digest(&pub_key_bytes)) != from {
        return Err("bad-pubkey".to_string());
    }
    let signing_payload =
        build_signing_payload_with_eph(kind, from, to, ts, body, eph_pub.as_deref());
    if !verify_ed25519_signature(&signing_payload, sig, pub_key) {
        return Err("bad-signature".to_string());
    }
    Ok(VerifiedDm {
        kind: kind.to_string(),
        from: from.to_string(),
        ts,
        body: body.clone(),
        eph_pub,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 固定私钥对应的 rootId（from = sha256hex(pubKey)）。
    fn test_root_id() -> String {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        hex::encode(Sha256::digest(signing_key.verifying_key().to_bytes()))
    }

    /// 构造合法签名信封（from == to == 本机 rootId，走通全部前置校验）。
    fn signed_envelope(ts: i64) -> (Value, String) {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let from = test_root_id();
        let envelope = build_envelope(KIND_CHAT, &from, &from, ts, json!({}), &signing_key);
        (envelope, from)
    }

    #[test]
    fn rejects_non_positive_and_extreme_ts_without_overflow() {
        let now = 1_720_000_000_000i64;
        for ts in [i64::MIN, -1, 0, i64::MAX] {
            let (envelope, from) = signed_envelope(ts);
            let err = verify_envelope(&envelope, &from, now).unwrap_err();
            assert_eq!(err, "stale", "ts={ts} 应以 stale 拒绝且不溢出");
        }
    }

    #[test]
    fn accepts_ts_at_window_edges() {
        let now = 1_720_000_000_000i64;
        for ts in [
            now,
            now - ENVELOPE_TS_WINDOW_MS,
            now + ENVELOPE_TS_WINDOW_MS,
        ] {
            let (envelope, from) = signed_envelope(ts);
            let verified = verify_envelope(&envelope, &from, now)
                .unwrap_or_else(|e| panic!("ts={ts} 窗口边界内应通过，得到 {e}"));
            assert_eq!(verified.ts, ts);
        }
    }

    #[test]
    fn rejects_ts_just_outside_window() {
        let now = 1_720_000_000_000i64;
        for ts in [
            now - ENVELOPE_TS_WINDOW_MS - 1,
            now + ENVELOPE_TS_WINDOW_MS + 1,
        ] {
            let (envelope, from) = signed_envelope(ts);
            let err = verify_envelope(&envelope, &from, now).unwrap_err();
            assert_eq!(err, "stale", "ts={ts} 超出窗口应 stale");
        }
    }
}
