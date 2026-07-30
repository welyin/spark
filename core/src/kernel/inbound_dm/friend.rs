//! dm 入站编排（friend 系）：friend-request / friend-accept / friend-reply。
//!
//! 从 `inbound_dm` 拆出的子模块（文件长度约束），共享父模块的
//! [`InboundContext`]/应答助手/[`is_blocked`]/[`done`]。

use serde_json::{Value, json};

use super::{
    AutoAccept, InboundContext, InboundDmResult, Result, done, fail_response, is_blocked,
    merge_friend_record, ok_response,
};
use crate::contact::{
    ContactService, FriendRecord, FriendRequestRecord, FriendRequestStatus, RequestThreadMessage,
    ThreadFrom,
};
use crate::message::{MAX_TEXT_BYTES, PeerRef};
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::storage::StorageBackend;

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
pub(super) fn handle_friend_request<S: StorageBackend>(
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
            thread: Vec::new(),
            invite_code: None,
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
/// 出站申请记录存在 && 仍为 pending/replied（replied 是对方已回复询问、
/// 尚未最终确认的态，此时接受合法）&& record.rootId == from——否则任何
/// 验签通过的对端都能把指向第三方的申请标记 accepted，或在我从未申请时
/// 直接成为「朋友」。
pub(super) fn handle_friend_accept<S: StorageBackend>(
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
    let valid = request.as_ref().is_some_and(|r| {
        matches!(
            r.status,
            FriendRequestStatus::Pending | FriendRequestStatus::Replied
        ) && r.root_id == from
    });
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

/// friend-reply：好友申请的来回回复（接收方询问 / 申请方回答同 kind，
/// 按本端记录匹配方向）。
///
/// 顺序：拉黑（`blocked`）→ body（requestId/text 齐全、text trim 后非空且不超
/// [`MAX_TEXT_BYTES`]）→ 本端 outbox 命中
/// （`rootId == from` 且 pending/replied：对方=接收方来消息，thread 追加 +
/// status 置 replied + FriendRequestSent 事件）→ 本端 inbox 复合 id
/// `{from}:{requestId}` 命中且 pending（对方=原申请方回答我的询问，
/// thread 追加、status 不变 + FriendRequestReceived 事件）→ 皆不命中
/// 回 `invalid-body`。
///
/// 分期说明：inbox 分支（对方回答我的询问）的应答链路完整，但「接收方主动
/// 发起询问」的出站命令未实装——前端已定稿，收到的申请只有接受/忽略入口
/// （ui-contacts §4 的询问 UI 未做），协议与入站匹配先行支持（其他客户端/
/// 未来 UI 可对接）。
pub(super) fn handle_friend_reply<S: StorageBackend>(
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
    let Some(text) = body.get("text").and_then(Value::as_str) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return done(fail_response("invalid-body"), Vec::new());
    }
    let msg = RequestThreadMessage {
        from: ThreadFrom::Peer,
        text: text.to_string(),
        ts: ctx.now_ms,
    };

    // 我发出的申请：对方（接收方）来询问/继续回复。
    // outbox 有记录但 rootId/status 不匹配时不直接拒——两端同毫秒同序号可能
    // 生成相同申请 id（撞车），此时本记录是「我发出的另一条」，真正的匹配
    // 可能在 inbox 复合 id（对方以原申请方身份回答我的询问），放行继续匹配
    if let Some(record) = ContactService::get_outgoing_request(storage, request_id)? {
        let valid = record.root_id == from
            && matches!(
                record.status,
                FriendRequestStatus::Pending | FriendRequestStatus::Replied
            );
        if valid {
            let record = ContactService::append_outgoing_thread(storage, request_id, msg.clone(), ctx.now_ms)?
                .expect("outgoing request just fetched");
            let event = P2pEvent::FriendRequestSent(json!({
                "request": serde_json::to_value(&record)?,
            }));
            return done(ok_response(), vec![event]);
        }
    }

    // 我收到的申请（复合 id `{from}:{requestId}`）：对方（原申请方）回答
    // 我的询问；已处理（accepted/ignored）的申请不再接受回复
    let inbox_id = format!("{from}:{request_id}");
    if let Some(record) = ContactService::get_incoming_request(storage, &inbox_id)?
        && record.status == FriendRequestStatus::Pending
    {
        let record = ContactService::append_incoming_thread(storage, &inbox_id, msg, ctx.now_ms)?
            .expect("incoming request just fetched");
        let event = P2pEvent::FriendRequestReceived(json!({
            "request": serde_json::to_value(&record)?,
        }));
        return done(ok_response(), vec![event]);
    }

    done(fail_response("invalid-body"), Vec::new())
}
