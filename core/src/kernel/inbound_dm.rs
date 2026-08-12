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
mod feed;
mod feed_blob;
mod friend;
mod notice;
mod org_invite;
mod orgkey;
mod orgq;
mod orgsync;
mod pdsync;
mod recovery;
mod sync;

use super::dm_envelope::{
KIND_CHAT, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_DEVICE_NOTICE, KIND_DEVICE_SYNC,
KIND_FEED, KIND_FEED_BLOB_REQ, KIND_FEED_BLOB_RESP, KIND_FRIEND_ACCEPT, KIND_FRIEND_REPLY,
KIND_FRIEND_REQUEST, KIND_ORG_INVITE, KIND_ORG_INVITE_REPLY, KIND_ORGKEY_DELIVER,
KIND_ORGSYNC_DATA, KIND_ORGSYNC_HELLO, KIND_ORGSYNC_NEED, KIND_ORGQ_REQ, KIND_ORGQ_RESP,
KIND_PDSYNC_ATTACHMENT_REQ, KIND_PDSYNC_ATTACHMENT_RESP, KIND_PDSYNC_DATA, KIND_PDSYNC_HELLO,
KIND_PDSYNC_NEED, KIND_PROFILE_SYNC, KIND_READ, KIND_RECALL, KIND_RECOVERY, verify_envelope,
};

/// O3 filtered 集合权限钩子（orgq-req 数据账号侧裁决契约，见 [`orgq`]）。
pub use orgq::OrgqPermHook;
/// O4 orgkey-deliver 解包指令（reader 侧合法投递，host 用 seed 解包落库）。
pub use orgkey::OrgkeyUnbox;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use crate::contact::{ContactError, ContactService, FriendRecord};
use crate::dm_e2e::{
    decrypt_body, decrypt_body_with_key, derive_session_key_from_eph_pub,
    record_inbound_peer_root_pub,
};
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
    /// 存储后端错误（O3 orgq 成员侧缓存/在途记录读写）。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// M5 延迟恢复模块错误（seen/veto/pending 落库）。
    #[error(transparent)]
    Recovery(#[from] crate::recovery::RecoveryError),
    /// M3 口令校验器模块错误（pwv/pwack 入站应用与锚定）。
    #[error("pw error: {0}")]
    Pw(String),
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<crate::pw::PwError> for InboundDmError {
    fn from(e: crate::pw::PwError) -> Self {
        Self::Pw(e.to_string())
    }
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
    /// 是否触发设备加入通知广播（M1：friend-accept 自身份分支触发 24h 补发窗口）。
    pub device_notice_broadcast: bool,
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
    /// orgsync 出站指令（连接层对端）：收到 orgsync-hello 的 diff 回发、或
    /// orgsync-need 的增量数据回发。body 已在纯逻辑层构建（io_lock 内），
    /// host 只负责包信封 + dm_direct。kind 为 KIND_ORGSYNC_*、
    /// from=本机 rootId、to=对端成员 rootId。
    pub orgsync_out: Vec<OrgsyncOut>,
    /// 本次 pdsync-data 是否合入了 `profile:self`（远端胜出）。host 据此
    /// 回写身份文件资料（仅解锁态），保证 sled 镜像与身份文件一致。
    pub profile_applied: bool,
    /// O4 orgkey-deliver 解包指令（reader 侧收到合法 orgkey-deliver）：host
    /// 用本机组织身份私钥（seed）解 box 并落 orgkey 表——解包需 recipient
    /// 私钥，纯逻辑层只做资格/验签判定后产出本指令。
    pub orgkey_unbox: Option<orgkey::OrgkeyUnbox>,
    /// feed-blob 出站指令（连接层对端）：跨联系人分块传输的响应/续拉。
    /// body 已在纯逻辑层构建（io_lock 内），host 只负责包信封 + dm_direct。
    pub feed_blob_out: Option<FeedBlobOut>,
}

/// orgsync/orgq 出站信封（body 已构建，host 装配完整信封并经 p2p 节点投递）。
/// kind 为 KIND_ORGSYNC_* / KIND_ORGQ_*、from=本机 rootId、to=对端成员
/// rootId（区别于 pdsync 的自设备 from==to==rootId 语义）。
///
/// **B1**：每个出站指令携带 `to_root_id`（目标成员 rootId）——orgsync 入站
/// 的 `from` 即对端成员 rootId，透传为出站信封的 to，host 不再用本机 rootId
/// 顶替（否则对端 verify_envelope 拒收 need/data 应答）。
#[derive(Clone, Debug)]
pub enum OrgsyncOut {
    /// orgsync-need diff 请求。
    Need {
        /// 目标成员 rootId（信封 to）。
        to_root_id: String,
        body: Value,
    },
    /// orgsync-data 数据传输（单批）。
    Data {
        /// 目标成员 rootId（信封 to）。
        to_root_id: String,
        body: Value,
    },
    /// orgq-req 按需查询/写入请求（成员 → 数据账号）。
    OrgqReq {
        /// 目标数据账号 rootId（信封 to）。
        to_root_id: String,
        body: Value,
    },
    /// orgq-resp 查询应答/写入回执（数据账号 → 成员）。
    OrgqResp {
        /// 目标成员 rootId（信封 to）。
        to_root_id: String,
        body: Value,
    },
}

impl OrgsyncOut {
    pub fn body(&self) -> &Value {
        match self {
            Self::Need { body, .. }
            | Self::Data { body, .. }
            | Self::OrgqReq { body, .. }
            | Self::OrgqResp { body, .. } => body,
        }
    }

    /// 目标成员/数据账号 rootId（信封 to）。
    pub fn to_root_id(&self) -> &str {
        match self {
            Self::Need { to_root_id, .. }
            | Self::Data { to_root_id, .. }
            | Self::OrgqReq { to_root_id, .. }
            | Self::OrgqResp { to_root_id, .. } => to_root_id,
        }
    }
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

/// feed-blob 出站信封（body 已构建，host 装配完整信封并经 p2p 节点投递；
/// 跨联系人分块传输通道，响应/续拉都走 feed-blob-req/resp kind）。
#[derive(Clone, Debug)]
pub enum FeedBlobOut {
    /// `feed-blob-req`：请求方续拉下一块（`{hash, offset}`）。
    BlobReq { body: Value },
    /// `feed-blob-resp`：服务方分块应答（`{hash, offset, data, totalBytes,
    /// missing?}`）。
    BlobResp { body: Value },
}

impl FeedBlobOut {
    /// 出站 body（host 装配信封用）。
    pub fn body(&self) -> &Value {
        match self {
            Self::BlobReq { body } | Self::BlobResp { body } => body,
        }
    }

    /// 出站信封 kind。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BlobReq { .. } => KIND_FEED_BLOB_REQ,
            Self::BlobResp { .. } => KIND_FEED_BLOB_RESP,
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
    /// 当前会话派生的 `Kverify`（32B）；未解锁或无 V 时为 `None`。
    /// 用于 D2 入站 pwack 锚定与 epoch 门控兜底，不落入存储。
    pub kverify: Option<&'a [u8; 32]>,
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
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: None,
        feed_blob_out: None,
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
        peers: Vec::new(),
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
        // 多设备寻址：合并写入首台设备（握手 nodeInfo）；已有同 peerId 不重复
        if !friend.peers.iter().any(|p| p.peer_id == safe_peer.peer_id) {
            friend.peers.push(safe_peer);
        }
    }
    // 接受产生的朋友记录是本机状态变更：刷新 LWW 时间（随 contact-sync
    // 传播到其他自设备）
    friend.updated_at = now_ms;
    ContactService::upsert_friend_pdsync(storage, &friend, now_ms, node_id)?;
    // pdsync/好友接受等所有好友合并路径统一回填优先集合（§4.4；已拉黑不加入）
    if !friend.blocked {
        let mut priority = crate::p2p::priority_peers::PriorityPeerStore::new(storage);
        for p in &friend.peers {
            if !p.peer_id.trim().is_empty() {
                if let Err(e) = priority.add(&p.peer_id) {
                    eprintln!("[dm] merge friend priority peer add failed: {e}");
                }
            }
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
    // 多设备寻址：仅当 peers 中存在自指污染项时才改写——过滤掉指向本机的
    // 项，并补入连接层权威值（本帧可达性已证实）。无自指项不改写（多设备
    // 合法指向第三台设备，来回抖动场景见注释）。
    let self_pointing = friend
        .peers
        .iter()
        .any(|p| !p.peer_id.is_empty() && p.peer_id == ctx.node_id);
    if !self_pointing {
        return;
    }
    friend.peers.retain(|p| p.peer_id != ctx.node_id);
    if !friend.peers.iter().any(|p| p.peer_id == ctx.remote_peer_id) {
        friend.peers.push(PeerRef {
            peer_id: ctx.remote_peer_id.to_string(),
            addresses: Vec::new(),
        ..Default::default()});
    }
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
///
/// filtered 集合的 orgq-req 以 fail-closed（无权限钩子）处理——宿主如需在
/// 插件后台运行时执行 `canRead`/`canWrite` 钩子，走
/// [`handle_inbound_dm_with_orgq_hooks`]。
///
/// 本入口不带 E2E 域身份（`my_domain = None`）：带 `ephPub` 的加密信封回
/// `internal-error`（无已解锁身份无法派生临时会话密钥），其余照常分发。
/// 宿主解锁态请走 [`handle_inbound_dm_with_e2e`]（传入本机 dm-e2e 域身份
/// 以支持入站 E2E 解密）。
pub fn handle_inbound_dm<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    my_nickname: &str,
    payload: Value,
    remote_peer_id: &str,
    online_peers: &HashSet<String>,
    now_ms: i64,
    node_id: &str,
    kverify: Option<&[u8; 32]>,
) -> Result<InboundDmResult> {
    handle_inbound_dm_inner(
        storage,
        my_root_id,
        my_nickname,
        payload,
        remote_peer_id,
        online_peers,
        now_ms,
        node_id,
        kverify,
        None,
        None,
    )
}

/// 同 [`handle_inbound_dm`]，但允许注入 filtered 集合的权限钩子（O3 工作项 2
/// 的宿主接线点）：数据账号侧处理 orgq-req 时，`hook` 在插件后台运行时
/// QuickJS 中执行 `canRead`/`canWrite`。`None` = fail-closed（插件未运行降级）。
pub fn handle_inbound_dm_with_orgq_hooks<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    my_nickname: &str,
    payload: Value,
    remote_peer_id: &str,
    online_peers: &HashSet<String>,
    now_ms: i64,
    node_id: &str,
    kverify: Option<&[u8; 32]>,
    hook: Option<&dyn orgq::OrgqPermHook>,
) -> Result<InboundDmResult> {
    handle_inbound_dm_inner(
        storage,
        my_root_id,
        my_nickname,
        payload,
        remote_peer_id,
        online_peers,
        now_ms,
        node_id,
        kverify,
        None,
        hook,
    )
}

/// S6 入站 E2E 入口（宿主解锁态）：`my_signing_key` 为本机 **root** 签名私钥
/// （2026-08-11 架构师裁决，root 密钥直接转换）——带 `ephPub` 的加密信封按
/// 「我方 root 私钥 + 对端 ephPub」派生临时会话密钥解密；无 `ephPub` 走密钥
/// 表回退（不需 root 私钥）。锁定态传 `None`（带 ephPub 的加密信封回
/// `internal-error`）。验签通过后顺手把信封 `pubKey`（对端 root 公钥）记入
/// 密钥表 `peerRootPub`（供本方后续出站 E2E 读取）。
pub fn handle_inbound_dm_with_e2e<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    my_nickname: &str,
    payload: Value,
    remote_peer_id: &str,
    online_peers: &HashSet<String>,
    now_ms: i64,
    node_id: &str,
    kverify: Option<&[u8; 32]>,
    my_signing_key: Option<&ed25519_dalek::SigningKey>,
) -> Result<InboundDmResult> {
    handle_inbound_dm_inner(
        storage,
        my_root_id,
        my_nickname,
        payload,
        remote_peer_id,
        online_peers,
        now_ms,
        node_id,
        kverify,
        my_signing_key,
        None,
    )
}

/// 入站 body 解密结果：明文 body 交各 handler，或直接拒绝（reason）。
enum DecryptedBody {
    Plain(Value),
    Rejected(&'static str),
}

/// S6 E2E 入站统一解密（先验签后解密，p2p-dm §19.1.1）：
///
/// - body 无 `encrypted` 标记（非加密信封）→ 原样 `Plain`（兼容旧对端/同步类
///   kind 的明文 body）；
/// - body `encrypted: true` 且信封带 `ephPub` → 用「我方 root 私钥 + 对端
///   ephPub」经 [`derive_session_key_from_eph_pub`] 派生临时会话密钥解密；
/// - body `encrypted: true` 且无 `ephPub`（对端未升级 root 直接转换 DH）→ 走
///   密钥表回退 [`decrypt_body`]（按 ts 选 current/历史密钥）。
///
/// `to` = 本机 rootId（`my_root_id`，信封 `to` 已验为指向本机）。解密失败
/// （密文/密钥/AAD 不符）→ `Rejected("invalid-body")`；需 ephPub 派生但
/// `my_signing_key` 为 `None`（锁定态无已解锁身份）→ `Rejected("internal-error")`。
fn decrypt_inbound_body<S: StorageBackend>(
    storage: &mut S,
    envelope: &crate::kernel::dm_envelope::VerifiedDm,
    my_root_id: &str,
    my_signing_key: Option<&ed25519_dalek::SigningKey>,
) -> Result<DecryptedBody> {
    let body = &envelope.body;
    let encrypted = body.get("encrypted").and_then(Value::as_bool).unwrap_or(false);
    if !encrypted {
        // 明文 body（同步类 kind / 兼容对端）：原样分发
        return Ok(DecryptedBody::Plain(body.clone()));
    }
    let dec_result: std::result::Result<Value, crate::dm_e2e::DmE2eError> =
        match &envelope.eph_pub {
            // ephPub 路径：我方 root 私钥 + 对端 ephPub 派生临时会话密钥
            Some(eph_b64) => {
                let Some(my_signing_key) = my_signing_key else {
                    // 锁定态无已解锁身份：无法派生 ephPub 会话密钥
                    return Ok(DecryptedBody::Rejected("internal-error"));
                };
                let eph_raw: [u8; 32] =
                    match B64.decode(eph_b64).ok().and_then(|v| v.try_into().ok()) {
                        Some(e) => e,
                        None => return Ok(DecryptedBody::Rejected("invalid-body")),
                    };
                match derive_session_key_from_eph_pub(
                    my_signing_key,
                    &eph_raw,
                    &envelope.from,
                    my_root_id,
                ) {
                    Ok(key) => decrypt_body_with_key(
                        &key,
                        &envelope.from,
                        my_root_id,
                        &envelope.kind,
                        envelope.ts,
                        body,
                    ),
                    Err(e) => Err(e),
                }
            }
            // 无 ephPub：密钥表回退（root 直接转换 DH 会话密钥）
            None => decrypt_body(
                storage,
                &envelope.from,
                my_root_id,
                &envelope.kind,
                envelope.ts,
                body,
            ),
        };
    match dec_result {
        Ok(plain) => Ok(DecryptedBody::Plain(plain)),
        Err(_) => Ok(DecryptedBody::Rejected("invalid-body")),
    }
}

fn handle_inbound_dm_inner<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    my_nickname: &str,
    payload: Value,
    remote_peer_id: &str,
    online_peers: &HashSet<String>,
    now_ms: i64,
    node_id: &str,
    kverify: Option<&[u8; 32]>,
    my_signing_key: Option<&ed25519_dalek::SigningKey>,
    orgq_hook: Option<&dyn orgq::OrgqPermHook>,
) -> Result<InboundDmResult> {
    let envelope = match verify_envelope(&payload, my_root_id, now_ms) {
        Ok(v) => v,
        Err(reason) => return done(fail_response(&reason), Vec::new()),
    };
    // 入站验签通过：把信封 `pubKey`（对端 root 公钥）记入密钥表 `peerRootPub`
    // （2026-08-11 架构师裁决，供本方后续出站 E2E 读取对端 root 公钥做 X25519
    // 转换）。仅当与已存值不同才写盘；记录不存在时创建占位记录。best-effort
    // （记录失败不阻断分发——密钥积累不影响本次入站处理）。
    //
    // **自设备信封（from==to==本机）跳过**：pdsync 自同步（hello/data/need）
    // 的 `pubKey` 是本机 root 公钥，记入 `dm:e2e:key:{self}` 属自污染——会话
    // 密钥表是**对端** root 公钥的槽位，自记录会触发 pdsync `dm:e2e` category
    // 窗口批次误报（S11 回归，见 kernel_pdsync_inbound 窗口收集）。
    if envelope.from != my_root_id
        && let Some(pub_key) = payload.get("pubKey").and_then(Value::as_str)
    {
        if let Err(e) = record_inbound_peer_root_pub(storage, &envelope.from, pub_key, node_id, now_ms) {
            log::warn!("[dm] record peer root pub failed: {e}");
        }
    }
    let ctx = InboundContext {
        my_root_id,
        my_nickname,
        remote_peer_id,
        online_peers,
        node_id,
        now_ms,
        kverify,
    };
    // 自设备信封触发的自记录寻址自愈（best-effort，失败不影响分发）
    if envelope.from == my_root_id {
        heal_self_friend_peer(storage, &ctx);
    }
    // S6 E2E 入站统一解密（先验签后解密）：body.encrypted 判定 → ephPub
    // 路径（我方 root 私钥 + 对端 ephPub 派生临时会话密钥）或密钥表回退
    // （无 ephPub，root 直接转换 DH 会话密钥）。解密失败回 `invalid-body`；
    // 无已解锁身份且需 ephPub 派生 → `internal-error`。非加密信封原样分发。
    let body = match decrypt_inbound_body(storage, &envelope, my_root_id, my_signing_key)? {
        DecryptedBody::Plain(body) => body,
        DecryptedBody::Rejected(reason) => {
            return done(fail_response(reason), Vec::new())
        }
    };
    log::info!(
        "[INBOUND_DM] routing kind={} from={}",
        envelope.kind,
        &envelope.from[..std::cmp::min(16, envelope.from.len())]
    );
    match envelope.kind.as_str() {
        KIND_CHAT => chat::handle_chat(storage, &ctx, &envelope.from, &body),
        KIND_READ => sync::handle_read(storage, &ctx, &envelope.from, &body),
        KIND_RECALL => sync::handle_recall(storage, &ctx, &envelope.from, &body),
        KIND_FRIEND_REQUEST => {
            friend::handle_friend_request(storage, &ctx, &envelope.from, &body)
        }
        KIND_FRIEND_ACCEPT => {
            friend::handle_friend_accept(storage, &ctx, &envelope.from, &body)
        }
        KIND_FRIEND_REPLY => {
            friend::handle_friend_reply(storage, &ctx, &envelope.from, &body)
        }
        KIND_PROFILE_SYNC => {
            sync::handle_profile_sync(storage, &ctx, &envelope.from, &body)
        }
        KIND_DEVICE_SYNC => {
            sync::handle_device_sync(storage, &ctx, &envelope.from, &body)
        }
        KIND_DEVICE_NOTICE => {
            notice::handle_device_notice(&ctx, &envelope.from, &envelope.body)
        }
        KIND_RECOVERY => {
            recovery::handle_recovery(storage, &ctx, &envelope.from, envelope.ts, &envelope.body)
        }
        KIND_CONTACT_SYNC => {
            sync::handle_contact_sync(storage, &ctx, &envelope.from, &body)
        }
        KIND_CONV_SYNC => sync::handle_conv_sync(storage, &ctx, &envelope.from, &body),
        KIND_PDSYNC_HELLO => {
            pdsync::handle_pdsync_hello(storage, &ctx, &envelope.from, &body)
        }
        KIND_PDSYNC_NEED => {
            pdsync::handle_pdsync_need(storage, &ctx, &envelope.from, &body)
        }
        KIND_PDSYNC_DATA => {
            pdsync::handle_pdsync_data(storage, &ctx, &envelope.from, &body)
        }
        KIND_PDSYNC_ATTACHMENT_REQ => {
            attachment::handle_attachment_req(storage, &ctx, &envelope.from, &body)
        }
        KIND_PDSYNC_ATTACHMENT_RESP => {
            attachment::handle_attachment_resp(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORG_INVITE => {
            org_invite::handle_org_invite(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORG_INVITE_REPLY => {
            org_invite::handle_org_invite_reply(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORGSYNC_HELLO => {
            orgsync::handle_orgsync_hello(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORGSYNC_NEED => {
            orgsync::handle_orgsync_need(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORGSYNC_DATA => {
            orgsync::handle_orgsync_data(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORGQ_REQ => {
            orgq::handle_orgq_req(storage, &ctx, &envelope.from, &body, orgq_hook)
        }
        KIND_ORGQ_RESP => {
            orgq::handle_orgq_resp(storage, &ctx, &envelope.from, &body)
        }
        KIND_ORGKEY_DELIVER => {
            orgkey::handle_orgkey_deliver(storage, &ctx, &envelope.from, &body)
        }
        KIND_FEED => feed::handle_feed(storage, &ctx, &envelope.from, &body, envelope.ts),
        KIND_FEED_BLOB_REQ => {
            feed_blob::handle_feed_blob_req(storage, &ctx, &envelope.from, &body)
        }
        KIND_FEED_BLOB_RESP => {
            feed_blob::handle_feed_blob_resp(storage, &ctx, &envelope.from, &body)
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
        ..Default::default()});
        assert!(
            reject_self_pointing_peer(node_id, self_peer).is_none(),
            "自指 peer 必须被拒绝"
        );

        // 正常（peer 指向对端设备）→ 原样放行
        let normal = Some(PeerRef {
            peer_id: "peer-other-device".to_string(),
            addresses: vec!["addr".to_string()],
        ..Default::default()});
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
        ..Default::default()});
        assert!(
            reject_self_pointing_peer(node_id, empty).is_some(),
            "空 peer_id 放行"
        );
    }
}
