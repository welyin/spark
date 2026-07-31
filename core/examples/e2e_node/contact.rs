//! 联系人命令：contact-overview/send-request/accept-request/reply-request/
//! ask-request/remove-friend/block-root。

use serde_json::{Value, json};
use spark_core::kernel::{Kernel, SendFriendRequestInput};

use crate::dispatch::{Params, to_json};

/// 出站申请 id 缺省生成（客户端幂等键口径）。
fn default_request_id() -> String {
    format!("req-{}", spark_core::p2p::node::system_now_ms())
}

/// `contact-overview`：空间通讯录总览（space 缺省 personal）。
pub fn overview(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    to_json(kernel.contact_overview(params.str_or("space", "personal")))
}

/// `send-request`：发出好友申请。rootId 必填；peerId/addresses 显式寻址
/// （脚本从对方 p2p-status 取得）；raw 缺省用 rootId；投递终态经
/// FriendRequestSent 事件回传。
pub fn send_request(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let root_id = params.need_str("rootId")?;
    let input = SendFriendRequestInput {
        id: params.str_or("requestId", "").to_string(),
        root_id: root_id.to_string(),
        raw: params.str_or("raw", root_id).to_string(),
        peer_id: params.opt_str("peerId").map(ToString::to_string),
        addresses: params.opt_strings("addresses"),
        source: params.str_or("source", "名片").to_string(),
        message: params.str_or("message", "").to_string(),
    };
    let input = SendFriendRequestInput {
        id: if input.id.is_empty() {
            default_request_id()
        } else {
            input.id
        },
        ..input
    };
    to_json(kernel.contact_send_request(input))
}

/// `accept-request`：接受入站申请（requestId 为复合 id `{from}:{原id}`）。
pub fn accept_request(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let request_id = params.need_str("requestId")?;
    let record = kernel
        .contact_resolve_request(request_id, true, params.opt_str("permission"))
        .map_err(|e| e.to_string())?;
    to_json(Ok(record))
}

/// `reply-request`：回复对方对申请的询问（本地 thread + 投递 friend-reply）。
pub fn reply_request(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let request_id = params.need_str("requestId")?;
    let text = params.need_str("text")?;
    to_json(kernel.contact_reply_request(request_id, text))
}

/// `ask-request`：接收方主动向申请方发起询问（本地 thread + 投递 friend-reply，
/// requestId 为复合 id `{from}:{原id}`）。
pub fn ask_request(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let request_id = params.need_str("requestId")?;
    let text = params.need_str("text")?;
    to_json(kernel.contact_ask_request(request_id, text))
}

/// `remove-friend`：删除朋友（block=true 同时拉黑）。
pub fn remove_friend(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let root_id = params.need_str("rootId")?;
    kernel
        .contact_remove_friend(root_id, params.opt_bool("block").unwrap_or(false))
        .map_err(|e| e.to_string())?;
    Ok(json!({"removed": true}))
}

/// `block-root`：设置/取消拉黑（拉黑集合独立于朋友记录）。
pub fn block_root(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let root_id = params.need_str("rootId")?;
    let blocked = params.opt_bool("blocked").unwrap_or(true);
    kernel
        .contact_set_blocked(params.str_or("space", "personal"), root_id, blocked)
        .map_err(|e| e.to_string())?;
    Ok(json!({"blocked": blocked}))
}
