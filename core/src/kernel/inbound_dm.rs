//! dm 入站编排：信封校验 → 按 kind 分发落库 → 生成壳层事件与直连应答。
//!
//! 纯函数（存储泛型），不触碰 p2p——`KernelHost::handle_dm` 在事件循环内
//! 调用本函数并把返回的事件逐个 emit。校验/业务拒绝都体现在 `response`
//! （`{"ok":false,"reason":...}`），`Err`（[`InboundDmError`]）仅用于存储
//! 等内部错误，类型保留模块归属，仅在 host 接线处拍平为 String。
//!
//! 信任模型：信封验签证明 `from` 持有对应根私钥；消息展示名、好友申请
//! 昵称等均为对端自报字段，仅作展示落库。
//!
//! 代码组织：本文件保留公共类型（[`InboundDmError`]/[`InboundDmResult`]/
//! [`AutoAccept`]/[`ProfileSyncReply`]/[`PdsyncOut`]）、入站上下文
//! [`InboundContext`]、共享应答/校验/自愈助手与总分发 [`handle_inbound_dm`]；
//! 各 kind 处理器按域拆到子模块——`friend`（friend 系）、`chat`（chat）、
//! `sync`（read/recall/profile-sync/device-sync/contact-sync/conv-sync）、
//! `pdsync`（pdsync 三信封）、`org_invite`（org-invite/org-invite-reply）。

use std::collections::HashSet;

use serde_json::{Value, json};

mod attachment;
mod chat;
mod friend;
mod org_invite;
mod pdsync;
mod sync;

use super::dm_envelope::{
    KIND_CHAT, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_DEVICE_SYNC, KIND_FRIEND_ACCEPT,
    KIND_FRIEND_REPLY, KIND_FRIEND_REQUEST, KIND_ORG_INVITE, KIND_ORG_INVITE_REPLY,
    KIND_PDSYNC_ATTACHMENT_REQ, KIND_PDSYNC_ATTACHMENT_RESP, KIND_PDSYNC_DATA,
    KIND_PDSYNC_HELLO, KIND_PDSYNC_NEED, KIND_PROFILE_SYNC, KIND_READ,
    KIND_RECALL, verify_envelope,
};
use crate::contact::{ContactError, ContactService, FriendRecord};
use crate::message::{MessageError, PeerRef};
use crate::org::OrgError;
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
    /// pdsync 个人域同步模块错误。
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),
    /// P6 插件数据模块错误（blob 传输等）。
    #[error(transparent)]
    Plugindata(#[from] crate::plugindata::PlugindataError),
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

/// profile-sync 回发指令：目标 + 是否无条件回发（true=配对握手；false=LWW 裁决）。
#[derive(Clone)]
pub struct ProfileSyncReply {
    pub target: PeerNodeInfo,
    pub unconditional: bool,
}

impl From<ProfileSyncReply> for PeerNodeInfo {
    fn from(reply: ProfileSyncReply) -> Self {
        reply.target
    }
}

/// 入站 dm 处理结果：直连应答帧 + 待广播的壳层事件。
pub struct InboundDmResult {
    /// 直连应答（序列化为响应帧回传发送方）。
    pub response: Value,
    /// 壳层事件（由 host 经 broadcast 通道外发）。
    pub events: Vec<P2pEvent>,
    /// 设备配对自动接受后待回发的 friend-accept（host 装配发送）。
    pub auto_accept: Option<AutoAccept>,
    /// 自设备 profile-sync 全量快照（from==自己 且带 updatedAt）：host 侧据此
    /// 更新本机身份文件（需会话口令重封，入站纯逻辑层不触碰身份文件）。
    pub self_profile: Option<Value>,
    /// 收到自设备 device-sync 后待回发本机设备记录的目标（握手式交换；
    /// host 装配本机 DeviceRecord 回发）。
    pub device_sync_reply: Option<PeerNodeInfo>,
    /// 自设备 profile-sync 回发指令（连接层对端目标）。
    /// - `handle_self_friend_request`（配对握手）：`unconditional=true`，
    ///   host 无条件回发——P2P 启动时的一次性广播可能早于自记录 peer 填入，
    ///   配对是首次可靠的回发时机；
    /// - `handle_profile_sync`（收到自设备快照）：`unconditional=false`，
    ///   host 按 LWW 裁决——本机身份文件 updatedAt 严格大于对端快照才回发
    ///   （对端较旧/残缺时补齐；收敛后相等不再互发，无 ping-pong）。
    pub profile_sync_reply: Option<ProfileSyncReply>,
    /// pdsync 出站指令（连接层对端）：收到 pdsync-hello 的 diff 回发、或
    /// pdsync-need 的增量数据回发。body 已在纯逻辑层构建（io_lock 内），
    /// host 只负责包信封 + dm_direct。
    pub pdsync_out: Vec<PdsyncOut>,
    /// 本次 pdsync-data 是否合入了 `profile:self`（远端胜出）。host 据此
    /// 回写身份文件资料（仅解锁态），保证 sled 镜像与身份文件一致。
    pub profile_applied: bool,
}

/// pdsync 出站信封（body 已构建，host 装配完整信封并经 p2p 节点投递）。
#[derive(Clone, Debug)]
pub enum PdsyncOut {
    /// `pdsync-hello` 的 diff 回发：对端某个 category 落后于本机 → 主动推
    /// `pdsync-data`（即发即忘，对齐 §5.2"对端落后 → 主动推"）。
    Push { body: Value },
    /// `pdsync-hello` 的 diff 回发：本机落后于对端 → 发 `pdsync-need`
    /// （携带本机 knownVv，请求对端补增量）。
    Need { body: Value },
    /// `pdsync-need` 的增量回发：`pdsync-data` 数据（单批）。
    Data { body: Value },
    /// P6 blob 拉取请求：`pdsync-attachment-req`（`{hash, offset}`）。
    AttachReq { body: Value },
    /// P6 blob 分块响应：`pdsync-attachment-resp`（`{hash, offset, data,
    /// totalBytes}`）。
    AttachResp { body: Value },
}

impl PdsyncOut {
    /// 出站 body（host 装配信封 / 集成测试透传用，免对变体逐个 match）。
    pub fn body(&self) -> &Value {
        match self {
            Self::Push { body }
            | Self::Need { body }
            | Self::Data { body }
            | Self::AttachReq { body }
            | Self::AttachResp { body } => body,
        }
    }
}

/// 入站上下文：本机身份/昵称、连接层对端、在线 peer 快照与时间（各
/// handle_* 共享，避免逐项透传参数）。`pub`（模块内）供子模块共享。
pub struct InboundContext<'a> {
    pub my_root_id: &'a str,
    pub my_nickname: &'a str,
    pub remote_peer_id: &'a str,
    /// 当前在线的 libp2p peerId 集合（事件循环快照；ChatReceived 事件的
    /// 会话视图 online 标志按它计算）。
    pub online_peers: &'a HashSet<String>,
    /// 本机节点 id（p2p 运行中为 peerId，否则 `local-node`；个人域 pmeta 用）。
    pub node_id: &'a str,
    pub now_ms: i64,
}

pub fn ok_response() -> Value {
    json!({ "ok": true })
}

pub fn fail_response(reason: &str) -> Value {
    json!({ "ok": false, "reason": reason })
}

pub fn done(response: Value, events: Vec<P2pEvent>) -> Result<InboundDmResult> {
    Ok(InboundDmResult {
        response,
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        profile_applied: false,
    })
}

/// 拉黑判定：个人空间查独立拉黑集合（陌生人亦可被拉黑），组织空间查成员
/// 附加资料 blocked。
pub fn is_blocked<S: StorageBackend>(
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
pub fn valid_space_key(space: &str) -> bool {
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
pub const MAX_FUTURE_SKEW_MS: i64 = 10 * 60_000;

/// 写侧自指防护：即将写入朋友记录的 peer 若指向本机节点 id（`node_id`，
/// p2p 运行中即本机 peerId），即为自指污染——朋友记录的 peer 应指向「对方
/// 设备」，绝不可能是本机自身。打日志并拒绝写入（返回 None，调用侧跳过
/// 赋值，保留原 peer 或留空）；正常的「peer 指向对方设备」不受影响。
/// 覆盖所有写自 FriendRecord 的合并路径（friend-accept / self-friend-request /
/// QR 恢复配对），防止对端 pdsync 互灌的污染形态再次落库。
pub fn reject_self_pointing_peer(node_id: &str, peer: Option<PeerRef>) -> Option<PeerRef> {
    match &peer {
        Some(p) if !p.peer_id.is_empty() && p.peer_id == node_id => {
            eprintln!(
                "[dm] self-pointing peer rejected | node_id={} peer_id={}",
                node_id, p.peer_id
            );
            None
        }
        _ => peer,
    }
}

/// 合并式建/更新朋友：已有记录保留本地资料（备注/标签/分组/照片/addedAt），
/// 仅刷新非空 nickname、Some 的 avatar 与 Some 的 peer；不存在才新建。返回
/// 最终记录。（friend-accept 与 chat 隐含确认共用）
pub fn merge_friend_record<S: StorageBackend>(
    storage: &mut S,
    root_id: &str,
    nickname: &str,
    avatar: Option<&str>,
    peer: Option<PeerRef>,
    now_ms: i64,
    node_id: &str,
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
        updated_at: now_ms,
    });
    if !nickname.is_empty() {
        friend.nickname = nickname.to_string();
    }
    if let Some(avatar) = avatar {
        friend.avatar = Some(avatar.to_string());
    }
    if let Some(safe_peer) = reject_self_pointing_peer(node_id, peer) {
        friend.peer = Some(safe_peer);
    }
    // 接受产生的朋友记录是本机状态变更：刷新 LWW 时间（随 contact-sync
    // 传播到其他自设备）
    friend.updated_at = now_ms;
    ContactService::upsert_friend_pdsync(storage, &friend, now_ms, node_id)?;
    // pdsync/好友接受等所有好友合并路径统一回填优先集合（§4.4；已拉黑不加入）
    if !friend.blocked
        && let Some(p) = friend.peer.as_ref()
        && !p.peer_id.trim().is_empty()
    {
        let mut priority = crate::p2p::priority_peers::PriorityPeerStore::new(storage);
        if let Err(e) = priority.add(&p.peer_id) {
            eprintln!("[dm] merge friend priority peer add failed: {e}");
        }
    }
    Ok(friend)
}

/// 自设备信封触发的自记录寻址自愈：信封验签证明 `from` 持有本身份根私钥
/// （来自同身份另一台设备），连接层 `remote_peer_id` 即该设备的权威 peerId
/// （libp2p 不会自连，remote 必非本机）。
///
/// 触发严格限定**自指污染**：自 FriendRecord 的 peer == 本机节点 id
/// （`ctx.node_id`，p2p 运行中即本机 peerId）——历史 pdsync 互灌的存量
/// 污染形态，改写为连接层值（本帧可达性已被证实，比 DeviceService 更直接）。
/// 记录指向其他值不改写：多设备场景 peer 合法指向第三台设备，按「与发帧
/// 设备不一致即改写」会让记录在自设备间随入站帧来回抖动。
///
/// 走 pdsync 口径 upsert（bump pmeta 属正常演进；该键对称排除于折叠/增量，
/// 重写不传播、无互灌回声）。无自记录时跳过（配对握手会创建，入站侧不越权
/// 新建）。
fn heal_self_friend_peer<S: StorageBackend>(storage: &mut S, ctx: &InboundContext<'_>) {
    let Ok(Some(mut friend)) = ContactService::get_friend(storage, ctx.my_root_id) else {
        return;
    };
    let self_pointing = friend
        .peer
        .as_ref()
        .is_some_and(|p| !p.peer_id.is_empty() && p.peer_id == ctx.node_id);
    if !self_pointing {
        return;
    }
    // 旧 addresses 属于本机监听地址（自指污染值），一并清除
    friend.peer = Some(PeerRef {
        peer_id: ctx.remote_peer_id.to_string(),
        addresses: Vec::new(),
    });
    friend.updated_at = ctx.now_ms;
    if let Err(e) =
        ContactService::upsert_friend_pdsync(storage, &friend, ctx.now_ms, ctx.node_id)
    {
        eprintln!("[dm] self friend peer heal failed: {e}");
    }
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
    node_id: &str,
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
        node_id,
        now_ms,
    };
    // 自设备信封触发的自记录寻址自愈（best-effort，失败不影响分发）
    if envelope.from == my_root_id {
        heal_self_friend_peer(storage, &ctx);
    }
    log::info!(
        "[INBOUND_DM] routing kind={} from={}",
        envelope.kind,
        &envelope.from[..std::cmp::min(16, envelope.from.len())]
    );
    match envelope.kind.as_str() {
        KIND_CHAT => chat::handle_chat(storage, &ctx, &envelope.from, &envelope.body),
        KIND_READ => sync::handle_read(storage, &ctx, &envelope.from, &envelope.body),
        KIND_RECALL => sync::handle_recall(storage, &ctx, &envelope.from, &envelope.body),
        KIND_FRIEND_REQUEST => {
            friend::handle_friend_request(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_FRIEND_ACCEPT => {
            friend::handle_friend_accept(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_FRIEND_REPLY => {
            friend::handle_friend_reply(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_PROFILE_SYNC => {
            sync::handle_profile_sync(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_DEVICE_SYNC => {
            sync::handle_device_sync(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_CONTACT_SYNC => {
            sync::handle_contact_sync(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_CONV_SYNC => sync::handle_conv_sync(storage, &ctx, &envelope.from, &envelope.body),
        KIND_PDSYNC_HELLO => {
            pdsync::handle_pdsync_hello(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_PDSYNC_NEED => {
            pdsync::handle_pdsync_need(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_PDSYNC_DATA => {
            pdsync::handle_pdsync_data(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_PDSYNC_ATTACHMENT_REQ => {
            attachment::handle_attachment_req(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_PDSYNC_ATTACHMENT_RESP => {
            attachment::handle_attachment_resp(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_ORG_INVITE => {
            org_invite::handle_org_invite(storage, &ctx, &envelope.from, &envelope.body)
        }
        KIND_ORG_INVITE_REPLY => {
            org_invite::handle_org_invite_reply(storage, &ctx, &envelope.from, &envelope.body)
        }
        _ => done(fail_response("unknown-kind"), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerRef;

    /// 写侧自指防护三态：自指拒绝 / 正常放行 / 空 peer_id 放行（空 id 不判
    /// 自指，避免把「尚未填充寻址信息」的占位记录误拦）。
    #[test]
    fn reject_self_pointing_peer_guards_self_but_allows_normal_and_empty() {
        let node_id = "my-device";

        // 自指（peer_id == node_id）→ 拒绝，返回 None
        let self_peer = Some(PeerRef {
            peer_id: node_id.to_string(),
            addresses: vec!["addr".to_string()],
        });
        assert!(
            reject_self_pointing_peer(node_id, self_peer).is_none(),
            "自指 peer 必须被拒绝"
        );

        // 正常（peer 指向对端设备）→ 原样放行
        let normal = Some(PeerRef {
            peer_id: "peer-other-device".to_string(),
            addresses: vec!["addr".to_string()],
        });
        assert_eq!(
            reject_self_pointing_peer(node_id, normal.clone())
                .map(|p| p.peer_id)
                .as_deref(),
            Some("peer-other-device"),
            "peer 指向对方设备时放行"
        );

        // 空 peer_id → 放行（非自指判定条件，占位记录不误拦）
        let empty = Some(PeerRef {
            peer_id: String::new(),
            addresses: Vec::new(),
        });
        assert!(
            reject_self_pointing_peer(node_id, empty).is_some(),
            "空 peer_id 放行"
        );
    }
}
