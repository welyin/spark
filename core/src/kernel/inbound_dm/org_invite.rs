//! dm 入站编排（org-invite 系）：org-invite / org-invite-reply 组织邀请。
//!
//! 从 `inbound_dm` 拆出的子模块（文件长度约束），共享父模块的
//! [`InboundContext`]/应答助手/[`is_blocked`] 等。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, Result, done, fail_response, is_blocked, ok_response,
};
use crate::kernel::message_ops::{conversation_view, message_view};
use crate::message::{
    ConversationKind, ConversationRecord, LinkPreview, MessageRecord, MessageService, MessageType,
};
use crate::org::{
    OrgInviteDirection, OrgInviteRecord, OrgInviteStatus, OrganizationService,
};
use crate::p2p::P2pEvent;
use crate::storage::StorageBackend;

/// personal 空间系统通知会话 id（固定单条；kind=System、peer_root_id="system"）。
const SYSTEM_CONV_ID: &str = "sys:notice";

/// 找到或创建 personal 空间的系统通知会话（固定 id，已存在即复用）。
fn ensure_system_conversation<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
) -> Result<ConversationRecord> {
    if let Some(existing) = MessageService::get_conversation(storage, "personal", SYSTEM_CONV_ID)? {
        return Ok(existing);
    }
    let record = ConversationRecord {
        id: SYSTEM_CONV_ID.to_string(),
        kind: ConversationKind::System,
        title: "系统通知".to_string(),
        peer_root_id: "system".to_string(),
        peer: None,
        unread_count: 0,
        pinned_at: 0,
        muted: false,
        draft: String::new(),
        updated_at: now_ms,
        meta_updated_at: 0,
    };
    MessageService::upsert_conversation(storage, "personal", &record)?;
    Ok(record)
}

/// org-invite：管理员经 DM 发来的组织邀请。校验（from != 我、未被拉黑、
/// 必填字段 inviteId/inviteCode/orgId/orgName/inviterNickname 非空）→ 幂等
/// upsert 入站记录（键 `org:inv:in:{orgId}:{from}`：已有记录仅刷新展示字段
/// 与 inviteCode，保留首次 createdAt，已有终态不重置）→ personal 空间系统
/// 通知会话 append 一条 link 组织卡片（消息 id `org-invite-{inviteId}` 按
/// id 去重：重复投递不重复 append/未读，对齐 handle_chat 的幂等口径）→
/// 未读 +1 → ChatReceived（系统会话 + 卡片消息）与 OrgInviteReceived
/// （落库后的邀请记录）事件。
///
/// orgName/inviterNickname 等展示字段均为对端自报，仅作展示落库（信任模型
/// 见模块头注释）；成员资格校验始终在后续 accept 编排的拉取侧完成。
pub(super) fn handle_org_invite<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from == ctx.my_root_id {
        return done(fail_response("invalid-body"), Vec::new());
    }
    if is_blocked(storage, "personal", from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    let field = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or_default();
    let invite_id = field("inviteId");
    let invite_code = field("inviteCode");
    let org_id = field("orgId");
    let org_name = field("orgName");
    let inviter_nickname = field("inviterNickname");
    if invite_id.is_empty()
        || invite_code.is_empty()
        || org_id.is_empty()
        || org_name.is_empty()
        || inviter_nickname.is_empty()
    {
        return done(fail_response("invalid-body"), Vec::new());
    }
    let org_avatar = body
        .get("orgAvatar")
        .and_then(Value::as_str)
        .map(str::to_string);

    // 幂等 upsert：同 (orgId, from) 已有记录原地更新（终态不重置）
    let record = match OrganizationService::get_incoming_invite(storage, org_id, from)? {
        Some(mut r) => {
            r.org_name = org_name.to_string();
            if org_avatar.is_some() {
                r.org_avatar = org_avatar;
            }
            r.peer_nickname = inviter_nickname.to_string();
            r.invite_code = Some(invite_code.to_string());
            r.updated_at = ctx.now_ms;
            r
        }
        None => OrgInviteRecord {
            id: invite_id.to_string(),
            org_id: org_id.to_string(),
            org_name: org_name.to_string(),
            org_avatar,
            peer_root_id: from.to_string(),
            peer_nickname: inviter_nickname.to_string(),
            direction: OrgInviteDirection::Incoming,
            status: OrgInviteStatus::Pending,
            invite_code: Some(invite_code.to_string()),
            created_at: ctx.now_ms,
            updated_at: ctx.now_ms,
        },
    };
    // P5：邀请记录写 pmeta，供自设备 pdsync 同步
    OrganizationService::put_invite_record_pdsync(storage, &record, ctx.now_ms, ctx.node_id)?;

    let conv = ensure_system_conversation(storage, ctx.now_ms)?;
    let mut events = Vec::new();
    let msg_id = format!("org-invite-{invite_id}");
    // 按消息 id 去重：重放/重试不重复 append/未读/ChatReceived
    if MessageService::get_message(storage, "personal", &conv.id, &msg_id)?.is_none() {
        let message = MessageRecord {
            id: msg_id,
            sender_id: from.to_string(),
            sender_name: inviter_nickname.to_string(),
            msg_type: MessageType::Link,
            content: org_name.to_string(),
            link: Some(LinkPreview {
                url: format!("spark-org-invite://{invite_id}"),
                title: org_name.to_string(),
                description: format!("{inviter_nickname} 正在邀请你加入"),
                site_name: "组织邀请".to_string(),
                domain: org_id.to_string(),
            }),
            created_at: ctx.now_ms,
            ..Default::default()
        };
        // pdsync 变体：系统会话壳 bump pmeta（随自设备同步传播会话入口）
        MessageService::append_message_pdsync(
            storage,
            "personal",
            &conv.id,
            &message,
            ctx.now_ms,
            Some(ctx.node_id),
        )?;
        MessageService::increment_unread(storage, "personal", &conv.id)?;
        // 事件里的会话取 append/unread 之后的最新快照（与 handle_chat 同口径）
        let conv = MessageService::get_conversation(storage, "personal", &conv.id)?
            .expect("conversation just written");
        events.push(P2pEvent::ChatReceived(json!({
            "spaceKey": "personal",
            "conversation": serde_json::to_value(conversation_view(&conv, ctx.online_peers, Some(ctx.my_root_id), None))?,
            "message": serde_json::to_value(message_view(&message, Some(ctx.my_root_id)))?,
        })));
    }
    events.push(P2pEvent::OrgInviteReceived(serde_json::to_value(&record)?));
    done(ok_response(), events)
}

/// org-invite-reply：被邀请人对管理员发出的邀请做出接受/拒绝应答。
pub(super) fn handle_org_invite_reply<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if is_blocked(storage, "personal", from)? {
        return done(fail_response("blocked"), Vec::new());
    }
    let Some(org_id) = body
        .get("orgId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(accept) = body.get("accept").and_then(Value::as_bool) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let nickname = body
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let record = OrganizationService::get_outgoing_invite(storage, org_id, from)?;
    let valid = record
        .as_ref()
        .is_some_and(|r| r.status == OrgInviteStatus::Pending);
    if !valid {
        return done(fail_response("invalid-body"), Vec::new());
    }

    let status = if accept {
        OrgInviteStatus::Accepted
    } else {
        OrgInviteStatus::Declined
    };
    let mut record = OrganizationService::mark_invite_status_pdsync(
        storage,
        OrgInviteDirection::Outgoing,
        org_id,
        from,
        status,
        ctx.now_ms,
        ctx.node_id,
    )?
    .expect("record checked pending above");
    if !nickname.is_empty() && record.peer_nickname != nickname {
        record.peer_nickname = nickname.to_string();
        OrganizationService::put_invite_record_pdsync(storage, &record, ctx.now_ms, ctx.node_id)?;
    }
    done(
        ok_response(),
        vec![P2pEvent::OrgInviteUpdated(serde_json::to_value(
            &record,
        )?)],
    )
}
