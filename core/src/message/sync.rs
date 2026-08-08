//! 自设备会话元数据快照同步（conv-sync 信封的构建与合入纯逻辑）。
//!
//! 同步范围：个人空间 direct 会话的「外壳 + 元数据」——会话存在性
//! （对端设备据此建好会话入口，后续新消息经自设备消息扇出自然流入）、
//! 置顶/免打扰/草稿。聊天记录本体不在本模块（用户明确暂不做历史同步），
//! 故 `unread_count`/`updated_at`（消息驱动字段）一律不随快照传播。
//!
//! 匹配键：会话 id 是建会话时随机生成的（两台设备各自建会话 id 不同），
//! 快照按 `peerRootId` 匹配 direct 会话。
//!
//! 裁决：`metaUpdatedAt` 记录级 LWW（仅置顶/免打扰/草稿变更时刷新；
//! 与消息驱动的 `updatedAt` 严格分离）。系统/应用会话不参与同步。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::Result;
use super::service::MessageService;
use super::types::{ConversationKind, ConversationRecord, PeerRef, generate_conversation_id};

/// 构建 conv-sync 快照（信封 body）：个人空间全部 direct 会话。
pub(crate) fn build_conv_sync_snapshot<S: StorageBackend>(storage: &S) -> Result<Value> {
    let conversations = MessageService::list_conversations(storage, "personal")?;
    let items: Vec<Value> = conversations
        .into_iter()
        .filter(|c| c.kind == ConversationKind::Direct)
        .map(|c| {
            serde_json::json!({
                "peerRootId": c.peer_root_id,
                "title": c.title,
                "peer": c.peer,
                "pinnedAt": c.pinned_at,
                "muted": c.muted,
                "draft": c.draft,
                "metaUpdatedAt": c.meta_updated_at,
            })
        })
        .collect();
    Ok(serde_json::json!({ "conversations": items }))
}

/// 合入 conv-sync 快照（返回实际变更的会话数——供上层决定是否通知前端）。
///
/// - 本地无该 peer 的 direct 会话 → 建外壳（unread=0，消息字段取本机
///   接收时间；标题取快照值，展示层仍按朋友备注/昵称重解析）；
/// - 本地已有 → 仅当快照 `metaUpdatedAt` 严格更新才应用置顶/免打扰/草稿；
/// - 合入不触发对外广播（防互灌循环）；
/// - 落库经 `upsert_conversation_pdsync`（写 pmeta，P2 conv 迁入后该记录
///   可经 pdsync 反熵继续向其他自设备收敛）。
pub(crate) fn apply_conv_sync_snapshot<S: StorageBackend>(
    storage: &mut S,
    body: &Value,
    now_ms: i64,
    node_id: &str,
) -> Result<usize> {
    let mut applied = 0usize;
    let Some(items) = body.get("conversations").and_then(Value::as_array) else {
        return Ok(0);
    };
    for item in items {
        let Some(peer_root_id) = item
            .get("peerRootId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let peer_root_id = peer_root_id.to_string();
        let meta_updated_at = item
            .get("metaUpdatedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let pinned_at = item.get("pinnedAt").and_then(Value::as_i64).unwrap_or(0);
        let muted = item.get("muted").and_then(Value::as_bool).unwrap_or(false);
        let draft = item
            .get("draft")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let peer: Option<PeerRef> = item
            .get("peer")
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let local = MessageService::find_direct_conversation(storage, "personal", &peer_root_id)?;
        match local {
            None => {
                let record = ConversationRecord {
                    id: generate_conversation_id(now_ms),
                    kind: ConversationKind::Direct,
                    title,
                    peer_root_id,
                    peer,
                    unread_count: 0,
                    pinned_at,
                    muted,
                    draft,
                    updated_at: now_ms,
                    meta_updated_at,
                };
                MessageService::upsert_conversation_pdsync(
                    storage,
                    "personal",
                    &record,
                    now_ms,
                    Some(node_id),
                )?;
                applied += 1;
            }
            Some(mut conv) => {
                if meta_updated_at <= conv.meta_updated_at {
                    continue;
                }
                conv.pinned_at = pinned_at;
                conv.muted = muted;
                conv.draft = draft;
                conv.meta_updated_at = meta_updated_at;
                // 对端 peer 寻址更完整时回填（本机为空才被覆盖）
                if conv.peer.is_none() && peer.is_some() {
                    conv.peer = peer;
                }
                MessageService::upsert_conversation_pdsync(
                    storage,
                    "personal",
                    &conv,
                    now_ms,
                    Some(node_id),
                )?;
                applied += 1;
            }
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_720_000_000_000;
    const NODE_A: &str = "node-a";
    const NODE_B: &str = "node-b";

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    /// 端到端：A 建会话并置顶 → B 合入 → 会话外壳 + 置顶元数据就位；
    /// 未读数/消息时间不随快照传播。
    #[test]
    fn roundtrip_creates_shell_with_meta() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let conv = MessageService::ensure_direct_conversation(
            &mut a,
            "personal",
            &rid('b'),
            "好友甲",
            None,
            NOW,
        )
        .unwrap();
        MessageService::toggle_pin(&mut a, "personal", &conv.id, NOW + 100).unwrap();
        MessageService::set_draft(&mut a, "personal", &conv.id, "草稿内容", NOW + 200).unwrap();
        // 消息驱动字段不应传播
        MessageService::increment_unread(&mut a, "personal", &conv.id).unwrap();

        let body = build_conv_sync_snapshot(&a).unwrap();
        let applied = apply_conv_sync_snapshot(&mut b, &body, NOW + 300, NODE_B).unwrap();
        assert_eq!(applied, 1);

        let synced =
            MessageService::find_direct_conversation(&b, "personal", &rid('b')).unwrap().unwrap();
        assert_eq!(synced.title, "好友甲");
        assert!(synced.pinned_at > 0, "置顶传播");
        assert_eq!(synced.draft, "草稿内容", "草稿传播");
        assert_eq!(synced.unread_count, 0, "未读数不传播");
        assert!(synced.meta_updated_at >= NOW + 200);

        // 幂等：重放无新变更
        let body = build_conv_sync_snapshot(&a).unwrap();
        assert_eq!(apply_conv_sync_snapshot(&mut b, &body, NOW + 400, NODE_B).unwrap(), 0);
    }

    /// LWW：本机元数据更新时不被旧快照覆盖；本机更旧时被新快照覆盖。
    #[test]
    fn meta_lww() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let ca = MessageService::ensure_direct_conversation(
            &mut a,
            "personal",
            &rid('c'),
            "好友乙",
            None,
            NOW,
        )
        .unwrap();
        let cb = MessageService::ensure_direct_conversation(
            &mut b,
            "personal",
            &rid('c'),
            "好友乙",
            None,
            NOW,
        )
        .unwrap();
        // 会话 id 各自随机：匹配必须按 peerRootId
        assert_ne!(ca.id, cb.id);

        // A 免打扰（metaUpdatedAt = NOW+100）
        MessageService::toggle_mute(&mut a, "personal", &ca.id, NOW + 100).unwrap();
        // B 稍后取消免打扰语义：先免打扰再取消（metaUpdatedAt = NOW+300）
        MessageService::toggle_mute(&mut b, "personal", &cb.id, NOW + 200).unwrap();
        MessageService::toggle_mute(&mut b, "personal", &cb.id, NOW + 300).unwrap();

        // A（旧，muted=true）→ B（新，muted=false）：不覆盖
        let body = build_conv_sync_snapshot(&a).unwrap();
        assert_eq!(apply_conv_sync_snapshot(&mut b, &body, NOW + 500, NODE_B).unwrap(), 0);
        let after = MessageService::find_direct_conversation(&b, "personal", &rid('c'))
            .unwrap()
            .unwrap();
        assert!(!after.muted);

        // B（新）→ A（旧）：覆盖
        let body = build_conv_sync_snapshot(&b).unwrap();
        assert_eq!(apply_conv_sync_snapshot(&mut a, &body, NOW + 600, NODE_A).unwrap(), 1);
        let after = MessageService::find_direct_conversation(&a, "personal", &rid('c'))
            .unwrap()
            .unwrap();
        assert!(!after.muted);
        assert_eq!(after.meta_updated_at, NOW + 300);
    }

    /// 非 direct 会话（系统/应用）不入快照。
    #[test]
    fn system_conversations_excluded() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        // 应用会话（app 模块建的应用消息会话）
        crate::message::app::AppMessageService::ensure_app_conversation(
            &mut a, "personal", "test-app", NOW,
        )
        .unwrap();
        let body = build_conv_sync_snapshot(&a).unwrap();
        assert_eq!(apply_conv_sync_snapshot(&mut b, &body, NOW + 100, NODE_B).unwrap(), 0);
        assert!(
            MessageService::list_conversations(&b, "personal").unwrap().is_empty(),
            "非 direct 会话不同步"
        );
    }
}
