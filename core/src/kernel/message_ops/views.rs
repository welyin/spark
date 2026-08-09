//! 视图映射与会话查询：记录 → 前端契约视图（`'me'` 映射/online 判定）的
//! 纯函数，以及 `Kernel` 的会话列表/消息列表查询与 direct 会话幂等建立。

use std::collections::HashSet;

use super::{AppMessageView, ChatMessageView, ConversationView, Kernel, Result};
use super::direct_conversation_id;
use crate::message::types::ConversationKind;
use crate::message::{AppMessageRecord, ConversationRecord, MessageRecord, MessageService};
use crate::p2p::node::system_now_ms;

/// 应用消息记录 → 视图（字段一一对应，无 `'me'` 映射——应用消息无发送者概念）。
pub(crate) fn app_message_view(record: &AppMessageRecord) -> AppMessageView {
    AppMessageView {
        id: record.id.clone(),
        plugin_id: record.plugin_id.clone(),
        summary: record.summary.clone(),
        payload: record.payload.clone(),
        card: record.card.clone(),
        created_at: record.created_at,
        status: record.status.clone(),
        read: record.read,
    }
}

/// 会话记录 → 视图（`online_peers` 为当前连接的 libp2p peerId 集合；
/// `my_root_id` 命中的会话是自己的会话，online 恒 true——自己永远在线）。
///
/// `fallback_peer_id`：`conv.peer` 缺失时（`message_ensure_direct` 从通讯录
/// 先建的会话没有寻址信息，peer 要等对方入站消息才回填）的寻址回退——朋友
/// 记录（`ct:friend:{peerRootId}`）里的 peerId，与 `resolve_conv_peer` 的
/// 回退口径对齐；回退也没有就不在线。
pub(crate) fn conversation_view(
    conv: &ConversationRecord,
    online_peers: &HashSet<String>,
    my_root_id: Option<&str>,
    fallback_peer_id: Option<&str>,
) -> ConversationView {
    let peer_id = conv
        .peer
        .as_ref()
        .map(|p| p.peer_id.as_str())
        .or(fallback_peer_id);
    let online = my_root_id.is_some_and(|me| me == conv.peer_root_id)
        || peer_id.is_some_and(|p| online_peers.contains(p));
    ConversationView {
        id: conv.id.clone(),
        kind: conv.kind,
        title: conv.title.clone(),
        peer_id: conv.peer_root_id.clone(),
        unread_count: conv.unread_count,
        pinned_at: conv.pinned_at,
        muted: conv.muted,
        online,
        draft: conv.draft.clone(),
        updated_at: conv.updated_at,
    }
}

/// 消息记录 → 视图（`my_root_id` 命中时做 `'me'` 映射；列表水合与入站
/// 事件两侧都传 `Some`，口径一致——自己设备同步来的消息同样渲染到自己侧）。
pub(crate) fn message_view(record: &MessageRecord, my_root_id: Option<&str>) -> ChatMessageView {
    let is_mine = my_root_id.is_some_and(|me| me == record.sender_id);
    ChatMessageView {
        id: record.id.clone(),
        sender_id: if is_mine {
            "me".to_string()
        } else {
            record.sender_id.clone()
        },
        sender_name: if is_mine {
            "我".to_string()
        } else {
            record.sender_name.clone()
        },
        msg_type: record.msg_type,
        content: record.content.clone(),
        file_size: record.file_size,
        duration: record.duration,
        link: record.link.clone(),
        quote: record.quote.clone(),
        created_at: record.created_at,
        status: record.status.clone(),
        recalled: record.recalled,
    }
}

impl Kernel {
    /// 会话列表（置顶优先，其余按最后消息时间倒序）。
    pub fn message_list_conversations(&self, space: &str) -> Result<Vec<ConversationView>> {
        let mut convs = MessageService::list_conversations(self.require_storage()?, space)?;
        convs.sort_by(|a, b| {
            (b.pinned_at > 0)
                .cmp(&(a.pinned_at > 0))
                .then(b.pinned_at.cmp(&a.pinned_at))
                .then(b.updated_at.cmp(&a.updated_at))
        });
        let online = self.online_peer_ids();
        let my_root_id = self.current_root_id().ok().flatten();
        let friend_peers = self.friend_peer_map();
        Ok(convs
            .iter()
            .map(|c| {
                conversation_view(
                    c,
                    &online,
                    my_root_id.as_deref(),
                    friend_peers.get(&c.peer_root_id).map(String::as_str),
                )
            })
            .collect())
    }

    /// 会话消息列表（时间升序；自己发的消息 `senderId`/`senderName` 映射为
    /// `"me"`/`"我"`）。
    pub fn message_list_messages(&self, space: &str, conv_id: &str) -> Result<Vec<ChatMessageView>> {
        let my_root_id = self.require_current_root_id()?;
        let messages = MessageService::get_messages(self.require_storage()?, space, conv_id)?;
        Ok(messages
            .iter()
            .map(|m| message_view(m, Some(&my_root_id)))
            .collect())
    }

    /// 找到或创建与 `peer_root_id` 的 1:1 会话（幂等；id 为 `dm:{peerRootId}`）。
    pub fn message_ensure_direct(
        &mut self,
        space: &str,
        peer_root_id: &str,
        title: &str,
    ) -> Result<ConversationView> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let now = system_now_ms();
        let node_id = self.sync_node_id();
        let conv = match MessageService::find_direct_conversation(
            self.require_storage()?,
            space,
            peer_root_id,
        )? {
            Some(existing) => existing,
            None => {
                let record = ConversationRecord {
                    id: direct_conversation_id(peer_root_id),
                    kind: ConversationKind::Direct,
                    title: title.to_string(),
                    peer_root_id: peer_root_id.to_string(),
                    peer: None,
                    unread_count: 0,
                    pinned_at: 0,
                    muted: false,
                    draft: String::new(),
                    updated_at: now,
                    meta_updated_at: 0,
                };
                MessageService::upsert_conversation_pdsync(
                    self.require_storage_raw_mut()?,
                    space,
                    &record,
                    now,
                    Some(&node_id),
                )?;
                record
            }
        };
        let online = self.online_peer_ids();
        let my_root_id = self.current_root_id().ok().flatten();
        let fallback = self.friend_peer_map().get(peer_root_id).cloned();
        Ok(conversation_view(
            &conv,
            &online,
            my_root_id.as_deref(),
            fallback.as_deref(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::PeerRef;

    fn conv(peer: Option<PeerRef>) -> ConversationRecord {
        ConversationRecord {
            id: "dm:peer-root".to_string(),
            kind: ConversationKind::Direct,
            title: "对方".to_string(),
            peer_root_id: "peer-root".to_string(),
            peer,
            ..Default::default()
        }
    }

    fn online(peers: &[&str]) -> HashSet<String> {
        peers.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn online_uses_conv_peer_first() {
        let set = online(&["peer-conv"]);
        assert!(conversation_view(
            &conv(Some(PeerRef {
                peer_id: "peer-conv".to_string(),
                addresses: Vec::new(),
            })),
            &set,
            None,
            None,
        )
        .online);
    }

    #[test]
    fn online_falls_back_to_friend_peer_when_conv_peer_missing() {
        let set = online(&["peer-friend"]);
        let c = conv(None);
        // 朋友记录也没有寻址信息 → 不在线
        assert!(!conversation_view(&c, &set, None, None).online);
        // conv.peer 缺失时回退朋友记录的 peerId → 在线
        assert!(conversation_view(&c, &set, None, Some("peer-friend")).online);
    }

    #[test]
    fn conv_peer_takes_precedence_over_fallback() {
        // conv.peer 存在但与回退不同源：以 conv.peer 为准（它才是会话寻址）
        let set = online(&["peer-friend"]);
        let c = conv(Some(PeerRef {
            peer_id: "peer-conv".to_string(),
            addresses: Vec::new(),
        }));
        assert!(!conversation_view(&c, &set, None, Some("peer-friend")).online);
    }

    #[test]
    fn self_conversation_always_online() {
        let set = online(&[]);
        let mut c = conv(None);
        c.peer_root_id = "me".to_string();
        assert!(conversation_view(&c, &set, Some("me"), None).online);
    }
}
