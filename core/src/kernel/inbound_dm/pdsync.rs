//! dm 入站编排（pdsync 系）：pdsync-hello / pdsync-need / pdsync-data 三信封
//! 个人域反熵同步。
//!
//! 从 `inbound_dm` 拆出的子模块（文件长度约束），共享父模块的
//! [`InboundContext`]/应答助手/[`done`] 等。`pdsync_message_conv_id`、
//! `pdsync_remote_wins`、`push_category_data` 为本模块私有辅助。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, PdsyncOut, Result, done, fail_response, ok_response,
};
use crate::kernel::message_ops::{conversation_view, message_view};
use crate::contact::ContactService;
use crate::message::{MessageRecord, MessageService};
use crate::p2p::P2pEvent;
use crate::storage::StorageBackend;

/// pdsync-hello：收到自设备（from==自己）的摘要。逐 category 与本地折叠 vv
/// 比对：
/// - 本机落后 → 发 `pdsync-need`（请求对端补增量）；
/// - 本机领先 → 主动推 `pdsync-data`（对端缺本机新数据）；
/// - 相等 → 不动（无 ping-pong）。
///
/// hello 不落库，只作为 diff 触发。回发信封由 host 装配投递（body 已在此
/// 构建，io_lock 内读取本地 vv，防与变更竞态）。
pub(super) fn handle_pdsync_hello<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    // 自 FriendRecord（`ct:friend:{myRootId}`）的 peer 是设备相对值，不可
    // 互灌：折叠/增量对称排除（双设备同账号排除键相同，folded vv 保持一致）
    let self_key = crate::sync::pdsync::self_friend_key(ctx.my_root_id);
    let exclude = Some(self_key.as_str());
    let remote_cats = crate::sync::pdsync::parse_hello_categories(body);
    let mut out = Vec::new();
    for category in crate::sync::pdsync::CATEGORIES {
        // 扫描失败不降级为空 vv（空 vv 会把本机误判为纯落后/纯领先，引发
        // 不必要的全量拉取/推送）：跳过该 category，本轮 diff 不含它
        let Ok(local_vv) = crate::sync::pdsync::collect_category_vv(storage, category, exclude)
        else {
            continue;
        };
        // 对端未声明该 category：视为空 vv（对端可能不支持该 category，等价
        // 于其落后——本机领先即主动推；本机为空则不动）
        let remote_vv = remote_cats.get(category.name).cloned().unwrap_or_default();
        match crate::sync::pdsync::diff_category(&local_vv, &remote_vv) {
            crate::sync::pdsync::DiffOutcome::LocalBehind { local_vv } => {
                // 本机落后：请求对端补增量
                let need_body = crate::sync::pdsync::build_need(category.name, &local_vv);
                out.push(PdsyncOut::Need { body: need_body });
            }
            crate::sync::pdsync::DiffOutcome::LocalAhead => {
                push_category_data(storage, &mut out, category, &remote_vv, exclude);
            }
            crate::sync::pdsync::DiffOutcome::Concurrent => {
                // 双向交换：既请求对端缺的，也主动推本机缺的（data 逐条向量
                // 幂等去重，双发收敛）
                let need_body = crate::sync::pdsync::build_need(category.name, &local_vv);
                out.push(PdsyncOut::Need { body: need_body });
                push_category_data(storage, &mut out, category, &remote_vv, exclude);
            }
            crate::sync::pdsync::DiffOutcome::Equal => {}
        }
    }
    // P4 消息窗口：消息不走折叠（§6.2），收到 hello 即按对端声明的 msgWindow
    // 主动推本机窗口内消息（append-only 幂等，重复推送无害）。对端窗口 = 发送
    // 方裁剪上限。
    {
        let remote_window = crate::sync::pdsync::MessageWindow::from_hello(body);
        if let Ok(window_records) =
            crate::sync::pdsync::collect_message_window(storage, &remote_window)
        {
            // 按 key 前缀分流打批：接收侧白名单按 key 前缀校验记录与声明
            // category 一致（msg:app: → "msg:app"），混批会被整批拒收并连坐
            // 同批的合法 msg:item 记录
            let (app_records, item_records): (Vec<_>, Vec<_>) = window_records
                .into_iter()
                .partition(|r| r.key.starts_with("msg:app:"));
            for (category, records) in
                [("msg:item", item_records), ("msg:app", app_records)]
            {
                let batches = crate::sync::pdsync::split_batches(records, PDSYNC_BATCH_BYTES);
                let total = batches.len();
                for (i, batch) in batches.into_iter().enumerate() {
                    let body =
                        crate::sync::pdsync::build_data_batch(category, &batch, i, total);
                    out.push(PdsyncOut::Data { body });
                }
            }
        }
    }
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: out,
        profile_applied: false,
    })
}

/// pdsync-need：收到自设备的 diff 请求（category + knownVv）。采集该
/// category 中相对 knownVv 的增量，分批发回 `pdsync-data`。
pub(super) fn handle_pdsync_need<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some((category_name, known_vv)) = crate::sync::pdsync::parse_need(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let Some(category) = crate::sync::pdsync::category_by_name(&category_name) else {
        return done(fail_response("unknown-category"), Vec::new());
    };
    // 自记录排除（与 hello 侧同键）：增量采集不推自 FriendRecord（含墓碑）
    let self_key = crate::sync::pdsync::self_friend_key(ctx.my_root_id);
    let Ok(records) =
        crate::sync::pdsync::collect_incremental(storage, category, &known_vv, Some(&self_key))
    else {
        return done(fail_response("collection-failed"), Vec::new());
    };
    let batches = crate::sync::pdsync::split_batches(records, PDSYNC_BATCH_BYTES);
    let total = batches.len();
    let mut out = Vec::with_capacity(total);
    for (i, batch) in batches.into_iter().enumerate() {
        let body = crate::sync::pdsync::build_data_batch(&category_name, &batch, i, total);
        out.push(PdsyncOut::Data { body });
    }
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: out,
        profile_applied: false,
    })
}

/// pdsync-data：收到自设备的增量数据，逐条 `apply_personal_remote`（幂等，
/// 重复推送被向量去重）。
///
/// 入站白名单（§8.4 红线）：逐条校验记录 key 属于注册 category
/// （`category_for_key`；`msg:item`/`msg:app` 走窗口协议按前缀另行匹配）
/// 且与信封声明的 category 一致——`p2p:*`/`meta:*`/`pmeta:*` 等 §3.2 排除
/// 前缀不在注册表内，任一记录不合法即整批拒收，防被攻陷/故障的配对设备
/// 覆写任意 sled 键（含 `p2p:identity:privateKey`）。
pub(super) fn handle_pdsync_data<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    if from != ctx.my_root_id {
        return done(fail_response("not-self-device"), Vec::new());
    }
    let Some((category_name, records)) = crate::sync::pdsync::parse_data(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // key 白名单 + 声明 category 一致性（发送方按 category 分批，合法批次
    // 不会混杂）
    for record in &records {
        let expected = if record.key.starts_with("msg:item:") {
            Some("msg:item")
        } else if record.key.starts_with("msg:app:") {
            Some("msg:app")
        } else {
            crate::sync::pdsync::category_for_key(&record.key).map(|c| c.name)
        };
        if expected != Some(category_name.as_str()) {
            return done(fail_response("category-mismatch"), Vec::new());
        }
    }
    let mut events = Vec::new();
    let mut profile_applied = false;
    let mut contacts_applied = 0usize;
    let mut convs_applied = 0usize;
    let mut org_meta_applied = 0usize;
    let mut org_contacts_applied = 0usize;
    // 自 FriendRecord 落库排除（与采集/折叠侧同键）：旧版本对端可能仍推送
    // 该键——其 peer 是设备相对值，落库会毒化本机自记录；墓碑同样丢弃
    // （对端删它的自记录不得删掉本机的）
    let self_key = crate::sync::pdsync::self_friend_key(ctx.my_root_id);
    // 消息合入按会话聚合最新一条，循环结束后逐会话发 ChatReceived（窗口
    // 回填成批到达，逐条发会冲垮广播通道）
    let mut latest_msg_by_conv: std::collections::BTreeMap<String, MessageRecord> =
        std::collections::BTreeMap::new();
    for record in records {
        if record.key == self_key {
            continue;
        }
        // 逐条 LWW 合入（pmeta 裁决；幂等）。value 是 JSON 值，落盘时转回
        // 字符串。
        let value_str = serde_json::to_string(&record.value)?;

        // `msg:conv`：远端胜出时用 merge_conv_meta 合并，保留本地消息驱动
        // 字段（unread/updated_at），只取同步字段（置顶/免打扰/草稿）。
        // convId 取 `msg:conv:personal:` 后的完整余段（direct 会话 id 是
        // `dm:{rootId}`，自身含冒号）。
        if let Some(cid) = record.key.strip_prefix("msg:conv:personal:") {
            // 先读本地快照与本地 pmeta 再裁决；合并值与 pmeta 同一 batch 提交
            // ——apply_personal_remote 先落「远端原值+meta」再补写合并值的两步
            // 写会在崩溃窗口留下「meta 已推进、本体是未合并远端值」
            let local_before = MessageService::get_conversation(storage, "personal", cid)?;
            let local_meta = crate::sync::get_personal_meta(storage, &record.key)?;
            if pdsync_remote_wins(local_meta.as_ref(), &record.meta) {
                let meta_raw = serde_json::to_string(&record.meta)?;
                let meta_key = crate::sync::personal_meta_key(&record.key);
                if crate::sync::is_tombstone(&record.meta) {
                    // 会话删除传播：删本体 + 落墓碑 pmeta（单 batch）
                    storage
                        .batch(vec![
                            crate::storage::BatchOperation::delete(record.key.clone()),
                            crate::storage::BatchOperation::put(meta_key, meta_raw),
                        ])
                        .map_err(crate::sync::SyncError::from)?;
                    convs_applied += 1;
                } else if let Ok(remote) =
                    serde_json::from_value::<crate::message::ConversationRecord>(
                        record.value.clone(),
                    )
                {
                    let merged = match local_before {
                        // 远端胜出：同步字段合并进本地副本，保留本地未读/更新时间
                        Some(mut local) => {
                            // 自聊会话（peer_root == 本机 rootId）的 peer 是设备
                            // 相对寻址（各指向对方设备），与自 FriendRecord 同性质
                            // ——保留本地值，不随远端快照覆盖
                            let self_conv_peer = (local.peer_root_id == ctx.my_root_id)
                                .then(|| local.peer.clone());
                            crate::message::MessageService::merge_conv_meta(&mut local, &remote);
                            if let Some(peer) = self_conv_peer {
                                local.peer = peer;
                            }
                            local
                        }
                        // 全新会话：未读/updated_at 是本机语义（§3.2），清零不继承
                        None => crate::message::ConversationRecord {
                            unread_count: 0,
                            updated_at: 0,
                            ..remote
                        },
                    };
                    storage
                        .batch(vec![
                            crate::storage::BatchOperation::put(
                                record.key.clone(),
                                serde_json::to_string(&merged)?,
                            ),
                            crate::storage::BatchOperation::put(meta_key, meta_raw),
                        ])
                        .map_err(crate::sync::SyncError::from)?;
                    convs_applied += 1;
                }
                // 远端值解析失败：不推进 pmeta（本地保持落后，下轮同步重试）
            }
            continue;
        }

        // `profile:self`：写 sled 后标记 profile_applied（host 负责回写身份
        // 文件，见 `handle_profile_sync` 同款处理）。
        if record.key == crate::kernel::identity::PROFILE_SELF_KEY {
            let local_ts = crate::sync::personal::get_personal_meta(storage, &record.key)
                .ok()
                .flatten()
                .map(|m| (m.ts, m.vv.clone()));
            let result = crate::sync::apply_personal_remote(
                storage,
                &record.key,
                &value_str,
                &record.meta,
            )?;
            log::info!(
                "[PROFILE_CHAIN] pdsync profile:self inbound | remote ts={} vv={:?} | local ts/vv={:?} | applied={}",
                record.meta.ts,
                record.meta.vv,
                local_ts,
                result.did_apply(),
            );
            if result.did_apply() {
                profile_applied = true;
            }
            continue;
        }

        // 消息（`msg:item` / `msg:app`）：走窗口 append-only 落盘 + byid 索引，
        // **不写 pmeta、不走 LWW**（§6.2）。msgId 天然幂等，撤回以 recalled
        // 覆盖传播。
        if record.key.starts_with("msg:item:") || record.key.starts_with("msg:app:") {
            // 事件聚合：应用前解析 convId 与消息本体（解析失败仅丢事件，
            // 不影响落库）
            if let Ok(msg) = serde_json::from_value::<MessageRecord>(record.value.clone())
                && let Some(conv_id) = pdsync_message_conv_id(&record.key)
            {
                println!(
                    "[PDSYNC_DATA] applying message | msgId={} convId={}",
                    msg.id, conv_id
                );
                // 消息已存在 → 跳过事件聚合。pdsync Hello 交换每次全量推窗口，
                // 已存在消息重复收会触发无意义的 ChatReceived（对已删除 bot
                // 的会话尤甚——bot 配置已删但会话还在，每次 Hello 都路由到插件
                // 侧却找不到 bot，产生"孤儿联系人"噪音）。
                if MessageService::get_message(storage, "personal", &conv_id, &msg.id)?
                    .is_none()
                {
                    latest_msg_by_conv
                        .entry(conv_id)
                        .and_modify(|cur| {
                            if msg.created_at > cur.created_at {
                                *cur = msg.clone();
                            }
                        })
                        .or_insert(msg);
                }
            }
            crate::sync::apply_message_record(storage, &record.key, &value_str)?;
            continue;
        }

        // 通用：普通个人域记录（联系人/设备/组织）
        if record.key.starts_with("ct:") {
            log::info!(
                "[CT_SYNC] apply ct record | key={} tombstone={} applied_check",
                record.key,
                crate::sync::is_tombstone(&record.meta),
            );
        }
        let result =
            crate::sync::apply_personal_remote(storage, &record.key, &value_str, &record.meta)?;
        if record.key.starts_with("ct:") {
            log::info!("[CT_SYNC] ct applied={} | key={}", result.did_apply(), record.key);
        }
        if result.did_apply() {
            if record.key.starts_with("device:") {
                // 设备清单：逐条 DeviceUpdated（data 即 DeviceRecord JSON）。
                // tombstone（设备删除传播）无本体、value 为 null——事件类型契约
                // 是 DeviceDto，null 载荷违约，跳过（删除随下次清单加载显现；
                // 前端监听仅触发整表刷新，不消费 payload）。
                if !crate::sync::is_tombstone(&record.meta) {
                    events.push(P2pEvent::DeviceUpdated(record.value.clone()));
                }
            } else if record.key.starts_with("ct:org:") {
                // 组织空间联系人（成员 extra / 标签 / 分组树 / req:out）：归 OrgSynced
                org_contacts_applied += 1;
            } else if record.key.starts_with("ct:") {
                // 个人空间联系人四域：归 ContactsSynced
                contacts_applied += 1;
            } else if record.key.starts_with("org:meta:") {
                // 组织记录（name/logo/members/我的 org 成员资料）：归 OrgSynced
                org_meta_applied += 1;
            }
            // org:inv 无对应壳层事件（无旧自设备快照通道），不发
        }
    }
    // 合并结果通知前端刷新（事件口径对齐旧快照通道）：
    // - 联系人四域/组织空间联系人 → ContactsSynced（整页刷新）；
    // - 会话元数据 → ConversationsSynced（刷新会话列表）；
    // - 消息 → 逐会话一条 ChatReceived（最新一条 + 当前会话快照；前端按 id
    //   去重、信任快照未读——窗口合入不动 unread_count，快照即现状）。
    if contacts_applied > 0 {
        events.push(P2pEvent::ContactsSynced(json!({ "applied": contacts_applied })));
    }
    if org_meta_applied > 0 || org_contacts_applied > 0 {
        events.push(P2pEvent::OrgSynced(json!({
            "orgMeta": org_meta_applied,
            "orgContacts": org_contacts_applied,
        })));
    }
    if convs_applied > 0 {
        events.push(P2pEvent::ConversationsSynced(json!({ "applied": convs_applied })));
    }
    for (conv_id, message) in latest_msg_by_conv {
        // 会话壳尚未同步到本机时跳过事件（conv 元数据到达后列表自会刷新，
        // 打开会话时消息从库内水合）
        let Ok(Some(conv)) = MessageService::get_conversation(storage, "personal", &conv_id)
        else {
            continue;
        };
        // online 判定与 handle_chat 同口径：conv.peer 缺失时回退朋友记录
        // （仅展示用：记录损坏不拖累整批合入，静默降级为无回退）
        let fallback_peer = ContactService::get_friend(storage, &conv.peer_root_id)
            .ok()
            .flatten()
            .and_then(|f| f.peer)
            .map(|p| p.peer_id);
        events.push(P2pEvent::ChatReceived(json!({
            "spaceKey": "personal",
            "conversation": serde_json::to_value(conversation_view(&conv, ctx.online_peers, Some(ctx.my_root_id), fallback_peer.as_deref()))?,
            "message": serde_json::to_value(message_view(&message, Some(ctx.my_root_id)))?,
        })));
    }

    // 记录最后收到对端 data 的时间，以供下次 Hello 携带 lastMsgSyncAt，
    // 避免每轮 Hello 全量推已同步的消息窗口。
    crate::sync::pdsync::set_last_msg_sync_at(storage, ctx.my_root_id, ctx.now_ms)?;

    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        // host 用 profile_applied 决定是否回写身份文件资料
        profile_applied,
    })
}

/// 从 pdsync 消息键解析 convId（仅用于事件聚合；解析失败丢事件不落库受影响）。
/// 键格式 `msg:{item|app}:personal:{convId}:{createdAt:013}:{msgId}`——从右侧
/// 剥掉 msgId 与 13 位零填充时间戳两段，余下即 convId（convId 自身可含 `:`，
/// 如 `app:{pluginId}`；msgId 含 `:` 时校验失败返回 None，仅丢事件）。
///
/// 应用消息键 `msg:app:personal:{pluginId}:...` 的 convId 需补回
/// `app:` 前缀（与落库侧 `sync::pdsync::message_conv_id` 口径一致），
/// 否则 `get_conversation` 查不到会话，ChatReceived 被静默丢弃。
fn pdsync_message_conv_id(key: &str) -> Option<String> {
    if let Some(rest) = key.strip_prefix("msg:app:personal:") {
        // pluginId 字符集不含 `:`（is_valid_plugin_id），剥两段后余下即 pluginId
        let (before_msg_id, _msg_id) = rest.rsplit_once(':')?;
        let (plugin_id, ts) = before_msg_id.rsplit_once(':')?;
        if ts.len() == 13 && ts.bytes().all(|b| b.is_ascii_digit()) && !plugin_id.is_empty() {
            return Some(format!("{}{plugin_id}", crate::message::APP_CONV_PREFIX));
        }
        return None;
    }
    let rest = key.strip_prefix("msg:item:personal:")?;
    let (before_msg_id, _msg_id) = rest.rsplit_once(':')?;
    let (conv_id, ts) = before_msg_id.rsplit_once(':')?;
    if ts.len() == 13 && ts.bytes().all(|b| b.is_ascii_digit()) {
        Some(conv_id.to_string())
    } else {
        None
    }
}

/// conv 合入的远端胜出裁决：镜像 `sync::personal::resolve_personal` 的语义
/// （vv 比较 → Concurrent 时 ts LWW → ts 相等按 nodeId 字典序兜底）。
/// personal.rs 的裁决函数为私有，而 conv 分支需要「先裁决、合并值与 pmeta
/// 同一 batch 落盘」，故在此保持同口径实现。
fn pdsync_remote_wins(
    local: Option<&crate::sync::meta::DocMeta>,
    remote: &crate::sync::meta::DocMeta,
) -> bool {
    let Some(local) = local else {
        return true;
    };
    match crate::sync::meta::compare_version_vectors(Some(&local.vv), Some(&remote.vv)) {
        crate::sync::meta::CompareResult::Equal | crate::sync::meta::CompareResult::Local => false,
        crate::sync::meta::CompareResult::Remote => true,
        crate::sync::meta::CompareResult::Concurrent => {
            if remote.ts != local.ts {
                return remote.ts > local.ts;
            }
            let remote_nid = remote.node_id.as_deref().unwrap_or("");
            let local_nid = local.node_id.as_deref().unwrap_or("");
            remote_nid > local_nid
        }
    }
}

/// pdsync-data 单批字节上限（沿用 dm 信封体积约束的保守值）。
const PDSYNC_BATCH_BYTES: usize = 256 * 1024;

/// 采集 category 相对对端折叠 vv（`remote_vv`）的增量，分批发入 `out`。
/// `exclude_key`：对称排除键（自 FriendRecord，见 handle_pdsync_hello）。
fn push_category_data<S: StorageBackend>(
    storage: &mut S,
    out: &mut Vec<PdsyncOut>,
    category: &crate::sync::pdsync::Category,
    remote_vv: &crate::sync::meta::VersionVector,
    exclude_key: Option<&str>,
) {
    let Ok(records) =
        crate::sync::pdsync::collect_incremental(storage, category, remote_vv, exclude_key)
    else {
        return;
    };
    let batches = crate::sync::pdsync::split_batches(records, PDSYNC_BATCH_BYTES);
    let total = batches.len();
    for (i, batch) in batches.into_iter().enumerate() {
        let body = crate::sync::pdsync::build_data_batch(category.name, &batch, i, total);
        out.push(PdsyncOut::Data { body });
    }
}
