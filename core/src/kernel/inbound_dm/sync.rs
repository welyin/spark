//! dm 入站编排（同步系）：read / recall / profile-sync / device-sync /
//! contact-sync / conv-sync。
//!
//! 从 `inbound_dm` 拆出的子模块（文件长度约束），共享父模块的
//! [`InboundContext`]/应答助手/[`is_blocked`]/[`valid_space_key`] 等。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, ProfileSyncReply, Result, done, fail_response, is_blocked,
    ok_response, valid_space_key,
};
use crate::kernel::message_ops::direct_conversation_id;
use crate::contact::ContactService;
use crate::message::MessageService;
use crate::org::OrganizationService;
use crate::p2p::{P2pEvent, PeerNodeInfo};
use crate::storage::StorageBackend;

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

/// read：对方已读回执——把我在此会话发出的 sent/delivered 消息置 read。
/// 组织空间同样要求 from 是成员（与 handle_chat 对齐）。
pub(super) fn handle_read<S: StorageBackend>(
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
pub(super) fn handle_recall<S: StorageBackend>(
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

/// profile-sync：朋友资料互推 + 自设备全量资料同步。
///
/// - 朋友（from != 自己）：只取 nickname/avatar 有效值更新朋友记录（原语义，
///   清空不传播给朋友侧展示）。
/// - 自设备（from == 自己）：除刷新自 FriendRecord 外，body 带 `updatedAt`
///   的全量快照（nickname/avatar/gender/region/signature）经
///   [`InboundDmResult::self_profile`] 上抛——host 侧以会话口令重封身份文件，
///   完成「我的资料」跨设备同步（旧格式无 updatedAt 的快照不应用身份文件，
///   避免建连互推的旧格式信封回灌）。updatedAt 新覆盖旧裁决在 host 侧按
///   身份文件 updatedAt 执行。
pub(super) fn handle_profile_sync<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let is_self = from == ctx.my_root_id;
    // 自设备快照上抛独立于朋友记录存在与否（新设备恢复后可能尚未创建自
    // FriendRecord，资料同步不应丢失）
    let is_self_snapshot =
        is_self && body.get("updatedAt").and_then(Value::as_i64).is_some();
    let self_profile = if is_self_snapshot {
        Some(body.clone())
    } else {
        None
    };
    // 握手回发候选目标：连接层对端（刚向我投递快照的设备，可达性已由本帧
    // 证实）。host 按 LWW 裁决——本机资料严格更新才回发全量快照，使较旧/
    // 残缺端（如 QR 恢复的新设备）收敛；相等则不互发，无 ping-pong。
    let profile_sync_reply = if is_self_snapshot {
        Some(ProfileSyncReply {
            target: PeerNodeInfo {
                peer_id: Some(ctx.remote_peer_id.to_string()),
                addresses: Vec::new(),
            },
            unconditional: false,
        })
    } else {
        None
    };
    let Some(mut friend) = ContactService::get_friend(storage, from)? else {
        return Ok(InboundDmResult {
            response: ok_response(),
            events: Vec::new(),
            auto_accept: None,
            self_profile,
            device_sync_reply: None,
            profile_sync_reply,
            pdsync_out: Vec::new(),
            orgsync_out: Vec::new(),
            profile_applied: false,
            orgkey_unbox: None,
        });
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
    let mut events = Vec::new();
    if changed {
        ContactService::upsert_friend_pdsync(storage, &friend, ctx.now_ms, ctx.node_id)?;
        let mut data = json!({
            "rootId": friend.root_id,
            "nickname": friend.nickname,
        });
        if let Some(a) = &friend.avatar {
            data["avatar"] = json!(a);
        }
        events.push(P2pEvent::FriendProfileUpdated(data));
    }
    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile,
        device_sync_reply: None,
        profile_sync_reply,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
    })
}

/// device-sync：自设备间交换设备记录（from==自己 rootId 才受理；验签已在
/// 入口完成，from 真实）。落库按 updatedAt 新覆盖旧裁决（device 模块），
/// 内容变化时广播 DeviceUpdated 事件；并给出回发目标（握手式交换——对端
/// 上线推送时本机回推，双方设备清单即双向齐全）。
pub(super) fn handle_device_sync<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Ok(record) = serde_json::from_value::<crate::device::DeviceRecord>(body.clone()) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    if record.peer_id.trim().is_empty() || !crate::device::is_usable_peer_id(&record.peer_id) {
        return done(fail_response("invalid-body"), Vec::new());
    }
    let (applied, changed) =
        crate::device::DeviceService::apply_remote(storage, record, ctx.now_ms, ctx.node_id)?;
    let mut events = Vec::new();
    if changed {
        events.push(P2pEvent::DeviceUpdated(
            serde_json::to_value(&applied).unwrap_or_else(|_| json!({})),
        ));
    }
    // 回发目标：自设备 FriendRecord 的寻址信息（优先匹配连接层对端 peerId）。
    // 仅当对端记录带来新信息（changed）时回发本机记录——无条件回发会让两端
    // 互为回包形成 ping-pong 风暴（对 profile-sync 的 LWW 回发裁决同口径：
    // 收敛后不再互发）。
    let reply = if changed {
        ContactService::get_friend(storage, from)?
            .and_then(|f| f.peer)
            .map(|p| PeerNodeInfo {
                peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                addresses: p.addresses,
            })
    } else {
        None
    };
    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: reply,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
    })
}

/// contact-sync：自设备（from==自己）发来的通讯录全量快照，LWW 合入
/// （contact/service/sync.rs）。有实际写入时发 ContactsSynced 事件通知
/// 前端整页刷新；合入不触发再广播（快照时间戳即事实来源，防互灌循环）。
pub(super) fn handle_contact_sync<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let applied =
        crate::contact::apply_contact_sync_snapshot(storage, ctx.my_root_id, body, ctx.node_id, ctx.now_ms)?;
    let events = if applied > 0 {
        vec![P2pEvent::ContactsSynced(json!({ "applied": applied }))]
    } else {
        Vec::new()
    };
    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
    })
}

/// conv-sync：自设备（from==自己）发来的会话元数据快照，按 peerRootId 匹配
/// LWW 合入（message/sync.rs；消息本体/未读数不同步）。有实际变更时发
/// ConversationsSynced 事件通知前端刷新会话列表。
pub(super) fn handle_conv_sync<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let applied = crate::message::apply_conv_sync_snapshot(storage, body, ctx.now_ms, ctx.node_id)?;
    let events = if applied > 0 {
        vec![P2pEvent::ConversationsSynced(json!({ "applied": applied }))]
    } else {
        Vec::new()
    };
    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
    })
}
