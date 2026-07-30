//! dm 入站编排：信封校验 → 按 kind 分发落库 → 生成壳层事件与直连应答。
//!
//! 纯函数（存储泛型），不触碰 p2p——`KernelHost::handle_dm` 在事件循环内
//! 调用本函数并把返回的事件逐个 emit。校验/业务拒绝都体现在 `response`
//! （`{"ok":false,"reason":...}`），`Err`（[`InboundDmError`]）仅用于存储
//! 等内部错误，类型保留模块归属，仅在 host 接线处拍平为 String。
//!
//! 信任模型：信封验签证明 `from` 持有对应根私钥；消息展示名、好友申请
//! 昵称等均为对端自报字段，仅作展示落库。

use std::collections::HashSet;

use serde_json::{Value, json};

use super::dm_envelope::{
    KIND_CHAT, KIND_FRIEND_ACCEPT, KIND_FRIEND_REQUEST, KIND_PROFILE_SYNC, KIND_READ, KIND_RECALL,
    verify_envelope,
};
use super::message_ops::{conversation_view, direct_conversation_id, message_view};
use crate::contact::{
    ContactError, ContactService, FriendRecord, FriendRequestRecord, FriendRequestStatus,
};
use crate::message::{
    ConversationKind, ConversationRecord, MAX_TEXT_BYTES, MessageError, MessageRecord,
    MessageService, MessageType, PeerRef,
};
use crate::org::{OrgError, OrganizationService};
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::storage::StorageBackend;

/// dm 入站编排统一错误（保留来源模块；`KernelHost::handle_dm` 接线处
/// `.to_string()` 拍平为直连应答 reason）。
#[derive(Debug, thiserror::Error)]
pub enum InboundDmError {
    /// 通讯录模块错误。
    #[error(transparent)]
    Contact(#[from] ContactError),
    /// 消息模块错误。
    #[error(transparent)]
    Message(#[from] MessageError),
    /// 组织模块错误。
    #[error(transparent)]
    Org(#[from] OrgError),
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 入站处理结果别名。
pub type Result<T> = std::result::Result<T, InboundDmError>;

/// 设备自动接受/重复申请重确认的回发指令（friend-request 且 from==我：来自
/// 同身份另一台设备的配对请求；或 from 已是朋友：对方未收到我此前的
/// accept 回执又发来申请）。本机已接受，需回发 friend-accept 完成确认。
///
/// 信封构造需要本机节点信息（peerId/监听地址）与签名私钥——两者只有
/// host/kernel 侧拿得到，故纯函数只产出本指令，由 `KernelHost::handle_dm`
/// 装配信封并经 p2p 节点回发。
#[derive(Clone, Debug)]
pub struct AutoAccept {
    /// 回发目标（来自请求方捎带的 nodeInfo）。
    pub target: PeerNodeInfo,
    /// 原申请的 requestId（回发信封 body 原样回显）。
    pub request_id: String,
    /// 回发信封的 to（设备配对=自己 rootId；重确认=请求方 rootId）。
    pub to_root_id: String,
}

/// 入站 dm 处理结果：直连应答帧 + 待广播的壳层事件。
pub struct InboundDmResult {
    /// 直连应答（序列化为响应帧回传发送方）。
    pub response: Value,
    /// 壳层事件（由 host 经 broadcast 通道外发）。
    pub events: Vec<P2pEvent>,
    /// 设备配对自动接受后待回发的 friend-accept（host 装配发送）。
    pub auto_accept: Option<AutoAccept>,
}

/// 入站上下文：本机身份/昵称、连接层对端、在线 peer 快照与时间（各
/// handle_* 共享，避免逐项透传参数）。
struct InboundContext<'a> {
    my_root_id: &'a str,
    my_nickname: &'a str,
    remote_peer_id: &'a str,
    /// 当前在线的 libp2p peerId 集合（事件循环快照；ChatReceived 事件的
    /// 会话视图 online 标志按它计算）。
    online_peers: &'a HashSet<String>,
    now_ms: i64,
}

fn ok_response() -> Value {
    json!({ "ok": true })
}

fn fail_response(reason: &str) -> Value {
    json!({ "ok": false, "reason": reason })
}

fn done(response: Value, events: Vec<P2pEvent>) -> Result<InboundDmResult> {
    Ok(InboundDmResult {
        response,
        events,
        auto_accept: None,
    })
}

/// 拉黑判定：个人空间查独立拉黑集合（陌生人亦可被拉黑），组织空间查成员
/// 附加资料 blocked。
fn is_blocked<S: StorageBackend>(
    storage: &S,
    space: &str,
    root_id: &str,
) -> Result<bool> {
    let blocked = if space == "personal" {
        ContactService::is_blocked(storage, root_id)?
    } else if let Some(org_id) = space.strip_prefix("org:") {
        ContactService::get_org_profile(storage, org_id, root_id)?
            .is_some_and(|p| p.blocked)
    } else {
        false
    };
    Ok(blocked)
}

/// 入站 spaceKey 校验：只允许 `personal` 或 `org:<orgId>`（orgId 为
/// `org_` + 16 位小写 hex，对齐 org.md §16.3；不含额外冒号——`personal:x`
/// 这类值会绕过校验且落在 personal 扫描前缀内）。
fn valid_space_key(space: &str) -> bool {
    if space == "personal" {
        return true;
    }
    let Some(org_id) = space.strip_prefix("org:") else {
        return false;
    };
    let Some(hex_part) = org_id.strip_prefix("org_") else {
        return false;
    };
    hex_part.len() == 16
        && hex_part
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 入站消息时间戳允许的未来偏移（10 分钟）：远未来消息会把会话钉在列表
/// 顶部，拒收（invalid-message）。
const MAX_FUTURE_SKEW_MS: i64 = 10 * 60_000;

/// 解析 body.nodeInfo（`{peerId, addresses}`）为 PeerRef。
fn parse_node_info(body: &Value) -> Option<PeerRef> {
    let info = body.get("nodeInfo")?;
    let peer_id = info.get("peerId").and_then(Value::as_str).unwrap_or_default();
    let addresses: Vec<String> = info
        .get("addresses")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if peer_id.is_empty() && addresses.is_empty() {
        return None;
    }
    Some(PeerRef {
        peer_id: peer_id.to_string(),
        addresses,
    })
}

/// 合并式建/更新朋友：已有记录保留本地资料（备注/标签/分组/照片/addedAt），
/// 仅刷新非空 nickname、Some 的 avatar 与 Some 的 peer；不存在才新建。返回
/// 最终记录。（friend-accept 与 chat 隐含确认共用）
fn merge_friend_record<S: StorageBackend>(
    storage: &mut S,
    root_id: &str,
    nickname: &str,
    avatar: Option<&str>,
    peer: Option<PeerRef>,
    now_ms: i64,
) -> Result<FriendRecord> {
    let mut friend = ContactService::get_friend(storage, root_id)?.unwrap_or(FriendRecord {
        root_id: root_id.to_string(),
        nickname: String::new(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: now_ms,
        peer: None,
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked: false,
    });
    if !nickname.is_empty() {
        friend.nickname = nickname.to_string();
    }
    if let Some(avatar) = avatar {
        friend.avatar = Some(avatar.to_string());
    }
    if peer.is_some() {
        friend.peer = peer;
    }
    ContactService::upsert_friend(storage, &friend)?;
    Ok(friend)
}

/// dm 入站处理：校验信封并按 kind 分发。`remote_peer_id` 为连接层对端
/// （libp2p peerId，随会话 peer 落库供回发寻址）；`online_peers` 为当前
/// 在线的 libp2p peerId 集合（事件循环快照，用于 ChatReceived 事件里
/// 会话视图的 online 标志）。
pub fn handle_inbound_dm<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    my_nickname: &str,
    payload: Value,
    remote_peer_id: &str,
    online_peers: &HashSet<String>,
    now_ms: i64,
) -> Result<InboundDmResult> {
    let envelope = match verify_envelope(&payload, my_root_id, now_ms) {
        Ok(v) => v,
        Err(reason) => return done(fail_response(&reason), Vec::new()),
    };
    let ctx = InboundContext {
        my_root_id,
        my_nickname,
        remote_peer_id,
        online_peers,
        now_ms,
    };
    match envelope.kind.as_str() {
        KIND_CHAT => handle_chat(storage, &ctx, &envelope.from, &envelope.body),
        KIND_READ => handle_read(storage, &ctx, &envelope.from, &envelope.body),
        KIND_RECALL => handle_recall(storage, &ctx, &envelope.from, &envelope.body),
        KIND_FRIEND_REQUEST => {
            handle_friend_request(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_FRIEND_ACCEPT => handle_friend_accept(storage, &ctx, &envelope.from, &envelope.body),
        KIND_PROFILE_SYNC => handle_profile_sync(storage, &envelope.from, &envelope.body),
        _ => done(fail_response("unknown-kind"), Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// chat
// ---------------------------------------------------------------------------

/// 组织空间成员校验：`org:` 空间要求 from 是该组织成员。
fn check_org_membership<S: StorageBackend>(
    storage: &S,
    space: &str,
    from: &str,
) -> Result<bool> {
    let Some(org_id) = space.strip_prefix("org:") else {
        return Ok(true);
    };
    Ok(OrganizationService::get_record(storage, org_id)?
        .is_some_and(|record| record.find_member(from).is_some()))
}

/// 会话标题解析：优先朋友备注/昵称，否则对端自报的 senderName。
fn resolve_conv_title<S: StorageBackend>(
    storage: &S,
    from: &str,
    fallback: &str,
) -> Result<String> {
    let title = ContactService::get_friend(storage, from)?
        .map(|f| if f.remark.is_empty() { f.nickname } else { f.remark })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    Ok(title)
}

/// 找到或创建 `dm:{from}` 会话（peer 取连接层对端 peerId）。会话已存在但
/// peer 为空时（`message_ensure_direct` 先建的会话没有寻址信息）回填
/// `{peer_id: 连接层对端, addresses: []}`，保证后续回发有 peer 可用。
fn ensure_inbound_conversation<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    space: &str,
    from: &str,
    title: &str,
) -> Result<ConversationRecord> {
    if let Some(mut existing) = MessageService::find_direct_conversation(storage, space, from)? {
        if existing.peer.is_none() {
            existing.peer = Some(PeerRef {
                peer_id: ctx.remote_peer_id.to_string(),
                addresses: Vec::new(),
            });
            MessageService::upsert_conversation(storage, space, &existing)?;
        }
        return Ok(existing);
    }
    let record = ConversationRecord {
        id: direct_conversation_id(from),
        kind: ConversationKind::Direct,
        title: title.to_string(),
        peer_root_id: from.to_string(),
        peer: Some(PeerRef {
            peer_id: ctx.remote_peer_id.to_string(),
            addresses: Vec::new(),
        }),
        unread_count: 0,
        pinned_at: 0,
        muted: false,
        draft: String::new(),
        updated_at: ctx.now_ms,
    };
    MessageService::upsert_conversation(storage, space, &record)?;
    Ok(record)
}

/// chat：spaceKey 形状校验 → 组织空间成员校验 → 隐含确认（个人空间：我有
/// 指向 from 的 pending 出站申请时，对方来消息即视为其已接受我，outbox 置
/// accepted + 建朋友 + FriendRequestAccepted 事件）→ ensure `dm:{from}` 会话 →
/// 按消息 id 去重（幂等：同 id 重放不重复 append/未读/事件，也不会覆盖
/// 已投递或已撤回的消息）→ 落库消息 + 未读 +1 → ChatReceived 事件。
/// senderId 强制绑定信封 from（忽略对端自报值，防伪造渲染成「我」）；
/// created_at 必须为正且不超过 now + 10 分钟（负/零值会破坏消息键的字典
/// 序=时间序，远未来消息会把会话钉在列表顶部，均拒收）；文本正文超过
/// [`MAX_TEXT_BYTES`]（16 KiB）拒收。from==我（同身份另一台设备同步过来
/// 的自消息）时不增加未读，且落库即置本地已读标记。
fn handle_chat<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(space) = body.get("spaceKey").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if !valid_space_key(space) {
        return done(fail_response("invalid-body"), Vec::new());
    }
    if is_blocked(storage, space, from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    if !check_org_membership(storage, space, from)? {
        return done(fail_response("not-member"), Vec::new());
    }
    let mut message: MessageRecord = match serde_json::from_value(
        body.get("message").cloned().unwrap_or(Value::Null),
    ) {
        Ok(m) => m,
        Err(_) => return done(fail_response("invalid-message"), Vec::new()),
    };
    // 入站消息不携带发送状态（状态仅为本机发送侧概念）
    message.status = None;
    // senderId 绑定信封 from（忽略对端自报值）
    message.sender_id = from.to_string();
    // 自消息（另一台设备同步）不产生未读，落库即置本地已读标记
    // （delete_message 的未读 -1 判定依据）
    message.read = from == ctx.my_root_id;
    // 消息存储键以 13 位零填充 createdAt 排序，负值带符号位填充后字典序与
    // 数值序相反、且排在所有正值之前——负/零 created_at 一律拒收
    if message.created_at <= 0 {
        return done(fail_response("invalid-message"), Vec::new());
    }
    if message.created_at > ctx.now_ms + MAX_FUTURE_SKEW_MS {
        return done(fail_response("invalid-message"), Vec::new());
    }
    // 文本正文长度上限（16 KiB，UTF-8 字节；对齐出站 message_send_text）
    if message.msg_type == MessageType::Text && message.content.len() > MAX_TEXT_BYTES {
        return done(fail_response("invalid-message"), Vec::new());
    }

    // 我主动发过申请（outbox pending）而对方先开口：对方能发消息即已接受我
    // （accept 回执可能丢失）——隐含确认：outbox 置 accepted、建朋友并发事件，
    // 然后照常收消息（朋友先建，会话标题才能取到其昵称）
    let mut events = Vec::new();
    if space == "personal"
        && from != ctx.my_root_id
        && ContactService::get_friend(storage, from)?.is_none()
        && let Some(request) = ContactService::find_outgoing_by_root(storage, from)?
        && request.status == FriendRequestStatus::Pending
    {
        ContactService::mark_outgoing_accepted(storage, &request.id, ctx.now_ms)?;
        let request = ContactService::get_outgoing_request(storage, &request.id)?;
        let friend = merge_friend_record(
            storage,
            from,
            &message.sender_name,
            None,
            Some(PeerRef {
                peer_id: ctx.remote_peer_id.to_string(),
                addresses: Vec::new(),
            }),
            ctx.now_ms,
        )?;
        events.push(P2pEvent::FriendRequestAccepted(json!({
            "request": request.map(serde_json::to_value).transpose()?,
            "friend": serde_json::to_value(&friend)?,
        })));
    }

    let title = resolve_conv_title(storage, from, &message.sender_name)?;
    let conv = ensure_inbound_conversation(storage, ctx, space, from, &title)?;
    // 按消息 id 去重：重放/重试幂等返回 ok，不重复落库/未读/事件
    if MessageService::get_message(storage, space, &conv.id, &message.id)?.is_some() {
        return done(ok_response(), events);
    }
    MessageService::append_message(storage, space, &conv.id, &message)?;
    // 自己的消息（另一台设备同步）不产生未读
    if from != ctx.my_root_id {
        MessageService::increment_unread(storage, space, &conv.id)?;
    }
    // 事件里的会话取 append/unread 之后的最新快照（避免事件携带过期的
    // unreadCount/updatedAt）
    let conv = MessageService::get_conversation(storage, space, &conv.id)?
        .expect("conversation just written");

    // online 判定与会话列表口径一致：conv.peer 缺失时回退朋友记录的 peerId
    let fallback_peer = ContactService::get_friend(storage, from)?
        .and_then(|f| f.peer)
        .map(|p| p.peer_id);
    events.push(P2pEvent::ChatReceived(json!({
        "spaceKey": space,
        "conversation": serde_json::to_value(conversation_view(&conv, ctx.online_peers, Some(ctx.my_root_id), fallback_peer.as_deref()))?,
        // 与列表水合路径口径一致：自己设备同步来的消息 senderId 映射为 'me'
        "message": serde_json::to_value(message_view(&message, Some(ctx.my_root_id)))?,
    })));
    done(ok_response(), events)
}

// ---------------------------------------------------------------------------
// read / recall
// ---------------------------------------------------------------------------

/// read：对方已读回执——把我在此会话发出的 sent/delivered 消息置 read。
/// 组织空间同样要求 from 是成员（与 handle_chat 对齐）。
fn handle_read<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(space) = body.get("spaceKey").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if !valid_space_key(space) {
        return done(fail_response("invalid-body"), Vec::new());
    }
    if is_blocked(storage, space, from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    if !check_org_membership(storage, space, from)? {
        return done(fail_response("not-member"), Vec::new());
    }
    let conv_id = direct_conversation_id(from);
    let changed =
        MessageService::mark_peer_messages_read(storage, space, &conv_id, ctx.my_root_id)?;
    // 无实际改动（会话不存在/无可回执消息）时不发事件，与 recall 的抑制对齐
    if changed.is_empty() {
        return done(ok_response(), Vec::new());
    }
    let event = P2pEvent::ChatStatus(json!({
        "spaceKey": space,
        "convId": conv_id,
        "peerRead": true,
    }));
    done(ok_response(), vec![event])
}

/// recall：对端撤回——强制置 recalled，但仅当存储消息的发送者就是信封
/// from（否则对端可撤回我方消息）；窗口由发送方本地约束，入站不判。
/// 归属不匹配/消息不存在按幂等处理（ok:true，不发事件）。
/// 组织空间同样要求 from 是成员（与 handle_chat 对齐）。
fn handle_recall<S: StorageBackend>(
    storage: &mut S,
    _ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(space) = body.get("spaceKey").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(message_id) = body.get("messageId").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if !valid_space_key(space) {
        return done(fail_response("invalid-body"), Vec::new());
    }
    if is_blocked(storage, space, from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    if !check_org_membership(storage, space, from)? {
        return done(fail_response("not-member"), Vec::new());
    }
    let conv_id = direct_conversation_id(from);
    let recalled = MessageService::force_recall(storage, space, &conv_id, message_id, from)?;
    let events = if recalled {
        vec![P2pEvent::ChatStatus(json!({
            "spaceKey": space,
            "convId": conv_id,
            "messageId": message_id,
            "recalled": true,
        }))]
    } else {
        Vec::new()
    };
    done(ok_response(), events)
}

// ---------------------------------------------------------------------------
// friend-request / friend-accept
// ---------------------------------------------------------------------------

/// friend-request 的 from==我 分支（同身份另一台设备的配对请求）：自动
/// 接受——落/更新设备 FriendRecord（已有条目保留 addedAt 与缺省字段），
/// 不产生「新的朋友」申请；返回 [`AutoAccept`] 指令由 host 回发
/// friend-accept 完成双向配对（A 扫码加 B 名片 → B 自动接受 → A 收到
/// accept，双方互有设备记录）。
fn handle_self_friend_request<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    request_id: &str,
    nickname: &str,
    peer: Option<PeerRef>,
) -> Result<InboundDmResult> {
    let existing = ContactService::get_friend(storage, ctx.my_root_id)?;
    let mut friend = existing.unwrap_or(FriendRecord {
        root_id: ctx.my_root_id.to_string(),
        nickname: String::new(),
        avatar: None,
        signature: String::new(),
        gender: None,
        added_at: ctx.now_ms,
        peer: None,
        remark: String::new(),
        phones: Vec::new(),
        tag_ids: Vec::new(),
        group_id: String::new(),
        memo: String::new(),
        photos: Vec::new(),
        permission: "open".to_string(),
        blocked: false,
    });
    if !nickname.is_empty() {
        friend.nickname = nickname.to_string();
    }
    if peer.is_some() {
        friend.peer = peer.clone();
    }
    ContactService::upsert_friend(storage, &friend)?;
    // 回发目标取请求方本次捎带的 nodeInfo（无则无法回发，host 跳过）
    let auto_accept = peer.map(|p| AutoAccept {
        target: PeerNodeInfo {
            peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
            addresses: p.addresses,
        },
        request_id: request_id.to_string(),
        // 设备配对：from==to==我（同身份设备间信封，验签侧 to==自己 自然通过）
        to_root_id: ctx.my_root_id.to_string(),
    });
    Ok(InboundDmResult {
        response: json!({ "ok": true, "nickname": ctx.my_nickname }),
        events: Vec::new(),
        auto_accept,
    })
}

/// friend-request：幂等落库收到的申请（同 rootId 已有 pending 则更新内容：
/// nickname/message/source 非空才覆盖、peer 为 None 保留原值——对端重试
/// 不带 nodeInfo 时不抹寻址信息），应答捎带本机昵称；from==我 走
/// [`handle_self_friend_request`] 自动接受；from 已是朋友（对方未收到我
/// 此前的 accept 回执又发来申请）不再生成申请，直接回发 friend-accept
/// 重确认（同样走 [`AutoAccept`] 指令）。
///
/// 新建申请的 id 用复合形式 `{from}:{原requestId}`（存储键随之带发送者
/// 命名空间 `ct:req:in:{from}:{id}`）——两个发送者同毫秒撞 id 时不再
/// 互相覆盖；事件与 overview 暴露给前端的即复合 id，resolve 按键直取。
fn handle_friend_request<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if is_blocked(storage, "personal", from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    let Some(request_id) = body.get("requestId").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let nickname = body
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = body
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // 头像为对端自报字段：校验通过才采纳，非法则忽略该字段（不拒绝整个请求）
    let avatar = body
        .get("avatar")
        .and_then(Value::as_str)
        .filter(|a| crate::identity::validate_avatar(a).is_ok())
        .map(str::to_string);
    let peer = parse_node_info(body);

    if from == ctx.my_root_id {
        return handle_self_friend_request(storage, ctx, request_id, nickname, peer);
    }

    // 已是朋友却又收到其申请：对方多半没收到我此前的 accept 回执（其 outbox
    // 仍 pending）——不再生成申请，直接回发 friend-accept 重确认（对方收到
    // 后 outbox 置 accepted 并建朋友）；重确认捎带的 avatar 同样刷新朋友记录
    if let Some(mut friend) = ContactService::get_friend(storage, from)? {
        if let Some(avatar) = &avatar {
            friend.avatar = Some(avatar.clone());
            ContactService::upsert_friend(storage, &friend)?;
        }
        let auto_accept = peer.map(|p| AutoAccept {
            target: PeerNodeInfo {
                peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                addresses: p.addresses,
            },
            request_id: request_id.to_string(),
            to_root_id: from.to_string(),
        });
        return Ok(InboundDmResult {
            response: json!({ "ok": true, "nickname": ctx.my_nickname }),
            events: Vec::new(),
            auto_accept,
        });
    }

    // 幂等：同 rootId 已有 pending 申请则原地更新内容（保留原 id）
    let existing = ContactService::overview(storage, "personal")?
        .requests
        .into_iter()
        .find(|r| r.root_id == from && r.status == FriendRequestStatus::Pending);
    let record = match existing {
        Some(mut r) => {
            if !nickname.is_empty() {
                r.nickname = nickname.to_string();
            }
            if avatar.is_some() {
                r.avatar = avatar.clone();
            }
            if !message.is_empty() {
                r.message = message.to_string();
            }
            if !source.is_empty() {
                r.source = source.to_string();
            }
            if peer.is_some() {
                r.peer = peer;
            }
            // 内容更新只刷新 updated_at，保留首次到达的 created_at
            r.updated_at = ctx.now_ms;
            r
        }
        None => FriendRequestRecord {
            // 复合 id：{from}:{原 requestId}（防跨发送者撞 id 覆盖）
            id: format!("{from}:{request_id}"),
            root_id: from.to_string(),
            nickname: nickname.to_string(),
            avatar,
            message: message.to_string(),
            source: source.to_string(),
            status: FriendRequestStatus::Pending,
            created_at: ctx.now_ms,
            updated_at: ctx.now_ms,
            peer,
        },
    };
    ContactService::put_incoming_request(storage, &record)?;
    let event = P2pEvent::FriendRequestReceived(json!({
        "request": serde_json::to_value(&record)?,
    }));
    done(
        json!({ "ok": true, "nickname": ctx.my_nickname }),
        vec![event],
    )
}

/// friend-accept：outbox 标记 accepted 并建朋友（peer 取对方捎带的 nodeInfo）。
///
/// 安全校验（任一不满足即拒，不改状态、不发事件）：from 未被拉黑 &&
/// 出站申请记录存在 && 仍为 pending && record.rootId == from——否则任何
/// 验签通过的对端都能把指向第三方的申请标记 accepted，或在我从未申请时
/// 直接成为「朋友」。
fn handle_friend_accept<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    // friend-accept 仅存在于个人空间：被拉黑者的接受直接拒绝（不建朋友）
    if is_blocked(storage, "personal", from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    let Some(request_id) = body.get("requestId").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // 兼容旧版对端：其回发的 requestId 可能误带 `{from}:` 复合前缀（inbound
    // 复合 id 原样回发），归一化为原始 id 再查 outbox
    let request_id = request_id
        .strip_prefix(&format!("{from}:"))
        .unwrap_or(request_id);
    let nickname = body
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 头像为对端自报字段：校验通过才采纳，非法忽略（不拒绝整个 accept）
    let avatar = body
        .get("avatar")
        .and_then(Value::as_str)
        .filter(|a| crate::identity::validate_avatar(a).is_ok())
        .map(str::to_string);
    let peer = parse_node_info(body);

    let request = ContactService::get_outgoing_request(storage, request_id)?;
    let valid = request
        .as_ref()
        .is_some_and(|r| r.status == FriendRequestStatus::Pending && r.root_id == from);
    if !valid {
        return done(fail_response("invalid-body"), Vec::new());
    }

    ContactService::mark_outgoing_accepted(storage, request_id, ctx.now_ms)?;
    let request = ContactService::get_outgoing_request(storage, request_id)?;
    // 合并式 upsert：已有记录保留本地资料（备注/标签/分组/照片/addedAt），
    // 仅刷新非空 nickname、Some 的 avatar 与 Some 的 peer；不存在才新建
    let friend = merge_friend_record(storage, from, &nickname, avatar.as_deref(), peer, ctx.now_ms)?;
    let request_json = request.map(serde_json::to_value).transpose()?;
    let friend_json = serde_json::to_value(&friend)?;
    let event = P2pEvent::FriendRequestAccepted(json!({
        "request": request_json,
        "friend": friend_json,
    }));
    done(ok_response(), vec![event])
}

// ---------------------------------------------------------------------------
// profile-sync
// ---------------------------------------------------------------------------

/// profile-sync：朋友主动推送的资料更新（建连后/资料变更后）。from 必须是
/// 已有朋友（陌生人忽略，按幂等 ok 应答，不暴露关系状态）；nickname 非空才
/// 覆盖、avatar 经 [`crate::identity::validate_avatar`] 校验通过才覆盖；
/// 有实际变更才落库并 emit `FriendProfileUpdated`（重复推送幂等无副作用）。
fn handle_profile_sync<S: StorageBackend>(
    storage: &mut S,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(mut friend) = ContactService::get_friend(storage, from)? else {
        return done(ok_response(), Vec::new());
    };
    let nickname = body
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let avatar = body
        .get("avatar")
        .and_then(Value::as_str)
        .filter(|a| crate::identity::validate_avatar(a).is_ok());
    let mut changed = false;
    if !nickname.is_empty() && friend.nickname != nickname {
        friend.nickname = nickname.to_string();
        changed = true;
    }
    if let Some(a) = avatar
        && friend.avatar.as_deref() != Some(a)
    {
        friend.avatar = Some(a.to_string());
        changed = true;
    }
    if !changed {
        return done(ok_response(), Vec::new());
    }
    ContactService::upsert_friend(storage, &friend)?;
    let mut data = json!({
        "rootId": friend.root_id,
        "nickname": friend.nickname,
    });
    if let Some(a) = &friend.avatar {
        data["avatar"] = json!(a);
    }
    done(ok_response(), vec![P2pEvent::FriendProfileUpdated(data)])
}
