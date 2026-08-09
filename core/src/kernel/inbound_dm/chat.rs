//! dm 入站编排（chat 系）：chat 消息落库。
//!
//! 从 `inbound_dm` 拆出的子模块（文件长度约束），共享父模块的
//! [`InboundContext`]/应答助手/[`is_blocked`]/[`valid_space_key`] 等。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, MAX_FUTURE_SKEW_MS, Result, done, fail_response, is_blocked,
    merge_friend_record, ok_response, valid_space_key,
};
use crate::contact::{ContactService, FriendRequestStatus};
use crate::message::{
    ConversationKind, ConversationRecord, MAX_TEXT_BYTES, MessageRecord, MessageService,
    MessageType, PeerRef,
};
use crate::p2p::P2pEvent;
use crate::storage::StorageBackend;
use crate::kernel::message_ops::{
    conversation_view, direct_conversation_id, message_view, sanitize_link_preview,
};
use crate::org::OrganizationService;

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
        meta_updated_at: 0,
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
/// [`MAX_TEXT_BYTES`]（16 KiB）拒收；link 为对端自报字段，入库前与出站
/// 同口径经 `sanitize_link_preview` 收敛（限长截断、空 url 或非 http(s)
/// scheme 整条丢弃）。
/// from==我（同身份另一台设备同步过来
/// 的自消息）时不增加未读，且落库即置本地已读标记。
pub(super) fn handle_chat<S: StorageBackend>(
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
    // link 为对端自报字段：入库前与出站同口径收敛（限长截断、空 url 整条丢弃）
    message.link = message.link.and_then(sanitize_link_preview);
    // read 初值：解析默认未读；自消息（含回同步）在 sender 绑定后按
    // 「有效发送者」口径置已读（见下）
    message.read = false;
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
        ContactService::mark_outgoing_accepted_pdsync(storage, &request.id, ctx.now_ms, ctx.node_id)?;
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
            ctx.node_id,
        )?;
        events.push(P2pEvent::FriendRequestAccepted(json!({
            "request": request.map(serde_json::to_value).transpose()?,
            "friend": serde_json::to_value(&friend)?,
        })));
    }

    // 自消息回同步（from==我，多设备 echo）：消息属于我发出的目标会话（真人或
    // bot），落库到 body.convId 指定的会话而非 `dm:{from}`（后者会错误塞进自己
    // 的会话）。对端发来的消息（from!=我）无 convId，仍按 from 推导会话。
    let is_self_echo = from == ctx.my_root_id;
    let echo_conv_id = if is_self_echo {
        body.get("convId").and_then(Value::as_str)
    } else {
        None
    };
    let title = resolve_conv_title(storage, from, &message.sender_name)?;
    let conv = if let Some(target_conv_id) = echo_conv_id {
        // 回同步：按目标会话 id 落库（会话由 conv-sync 同步或发送端 ensure 已建）
        match MessageService::get_conversation(storage, space, target_conv_id)? {
            Some(c) => c,
            None => {
                // 会话尚未同步到本机：按 convId 反推 peer（dm:{peer}）建壳落库
                let peer_root = target_conv_id.strip_prefix("dm:").unwrap_or(target_conv_id);
                let record = ConversationRecord {
                    id: target_conv_id.to_string(),
                    kind: ConversationKind::Direct,
                    title: resolve_conv_title(storage, peer_root, &message.sender_name)?,
                    peer_root_id: peer_root.to_string(),
                    peer: None,
                    unread_count: 0,
                    pinned_at: 0,
                    muted: false,
                    draft: String::new(),
                    updated_at: ctx.now_ms,
                    meta_updated_at: 0,
                };
                MessageService::upsert_conversation(storage, space, &record)?;
                record
            }
        }
    } else {
        ensure_inbound_conversation(storage, ctx, space, from, &title)?
    };
    // senderId 归属：常规对端消息绑定信封 from（防伪造渲染成「我」）；
    // 自消息回同步（from==我）按目标会话区分——真人会话绑定 from（我自己发的），
    // bot 会话信任 body 里的 sender_id（bot 是发送者，如 bot 回复回同步到其他设备）。
    // bot 判定取会话 peer_root_id（权威，落库时已确定），不信任对端自报。
    let is_bot_conv = conv.peer_root_id.starts_with("bot:");
    if !is_self_echo || !is_bot_conv {
        message.sender_id = from.to_string();
    }
    // 未读口径按「有效发送者」判定：我发的消息（含另一台设备回同步）落库即
    // 已读、不产生未读；bot 会话回同步的有效发送者是 bot，正常计未读
    let from_me = message.sender_id == ctx.my_root_id;
    message.read = from_me;
    // 按消息 id 去重：重放/重试幂等返回 ok，不重复落库/未读/事件
    if MessageService::get_message(storage, space, &conv.id, &message.id)?.is_some() {
        return done(ok_response(), events);
    }
    // pdsync 变体：个人空间会话壳随消息 bump pmeta——「只收不发」的会话由此
    // 进入 pdsync 折叠/增量（裸 append 不产生 pmeta，其他设备拿不到会话入口）
    MessageService::append_message_pdsync(
        storage,
        space,
        &conv.id,
        &message,
        ctx.now_ms,
        Some(ctx.node_id),
    )?;
    // 自己的消息（另一台设备同步）不产生未读（有效发送者口径，见上）
    if !from_me {
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
    println!(
        "[KERNEL] handle_chat -> ChatReceived | msgId={} convId={} space={}",
        message.id, conv.id, space
    );
    events.push(P2pEvent::ChatReceived(json!({
        "spaceKey": space,
        "conversation": serde_json::to_value(conversation_view(&conv, ctx.online_peers, Some(ctx.my_root_id), fallback_peer.as_deref()))?,
        // 与列表水合路径口径一致：自己设备同步来的消息 senderId 映射为 'me'
        "message": serde_json::to_value(message_view(&message, Some(ctx.my_root_id)))?,
    })));
    done(ok_response(), events)
}
