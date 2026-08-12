//! 出站 DM 收件人过滤（social-feed §5，纯逻辑层）。
//!
//! 过滤是**发送方本地意愿的执行**：「我拉黑了 X」「我标 X 仅聊天」都是本机
//! 数据。编排层（kernel 门面）在构造信封前调用本过滤函数，按通道决定命中
//! 时的返回语义：
//!
//! | 通道 | 拉黑 `ct:blocked:` | 仅聊天 `chatOnly` | 非朋友 |
//! | --- | --- | --- | --- |
//! | chat（`message_send_*`） | **过滤**（本期启用） | 不过滤 | 不过滤 |
//! | feed（`feed_deliver`） | 过滤 | 过滤 | 过滤 |
//!
//! chat 通道命中拉黑 → skipped（编排层如实置 failed + UI 文案「你已拉黑
//! 对方」）；feed 通道命中 → skipped（编排层静默跳过，只给聚合计数）。
//!
//! 入站拉黑检查（`is_blocked` → `blocked` 拒收）是接收侧现状，不在此处。

use crate::storage::StorageBackend;

use super::*;
use crate::contact::{BLOCKED_PREFIX, FRIEND_PREFIX, FriendRecord};

/// DM 投递通道：决定过滤的检查集合（chat 不查仅聊天/非朋友）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmChannel {
    /// 聊天（`message_send_*`）：只过滤拉黑。
    Chat,
    /// 社交投递（`feed_deliver`）：过滤拉黑 + 仅聊天 + 非朋友。
    Feed,
}

/// 被过滤收件人的跳过原因（供编排层决定返回语义与 UI 文案）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmRecipientSkipReason {
    /// 非本机朋友（feed 通道）。
    NotFriend,
    /// 拉黑集合命中（`ct:blocked:`）。
    Blocked,
    /// 朋友权限 `chatOnly`（仅聊天，feed 通道）。
    ChatOnly,
}

/// 单个被跳过的收件人。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedRecipient {
    pub root_id: String,
    pub reason: DmRecipientSkipReason,
}

/// 过滤结果：accepted（放行投递）+ skipped（跳过，附原因）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmRecipientFilter {
    /// 放行的 rootId 名单。
    pub accepted: Vec<String>,
    /// 跳过的收件人（含原因）。
    pub skipped: Vec<SkippedRecipient>,
}

impl ContactService {
    /// 出站 DM 收件人过滤（social-feed §5，纯逻辑层）。
    ///
    /// 逐个收件人 rootId 判定：
    /// - **feed 通道**：非本机朋友 → `NotFriend`；拉黑集合命中 → `Blocked`；
    ///   朋友 `permission == "chatOnly"` → `ChatOnly`；
    /// - **chat 通道**：仅拉黑集合命中 → `Blocked`（非朋友与仅聊天不过滤——
    ///   聊天是既有 1:1 通道，非朋友也可收；仅聊天联系人仍可正常聊天）。
    ///
    /// 返回值：`accepted` 为放行名单，`skipped` 为跳过名单（含原因），
    /// 两表并集 == 入参 `recipients`。不判重（入参名单去重由调用方负责）。
    pub fn filter_dm_recipients<S: StorageBackend>(
        storage: &S,
        recipients: &[String],
        channel: DmChannel,
    ) -> Result<DmRecipientFilter> {
        let blocked = blocked_set(storage)?;
        let mut out = DmRecipientFilter::default();
        for root_id in recipients {
            // 拉黑集合命中（两通道都过滤；优先于朋友判定——拉黑是更强的本机
            // 意愿，被拉黑的非朋友也应按拉黑原因报告给编排层，chat 通道据此
            // 给「你已拉黑对方」文案）。
            if blocked.contains(root_id) {
                out.skipped.push(SkippedRecipient {
                    root_id: root_id.clone(),
                    reason: DmRecipientSkipReason::Blocked,
                });
                continue;
            }
            let friend = match read_json::<S, FriendRecord>(storage, &format!("{FRIEND_PREFIX}{root_id}"))? {
                Some(f) => f,
                None => {
                    // 非朋友：feed 通道跳过；chat 通道放行（聊天不要求是朋友）
                    if channel == DmChannel::Feed {
                        out.skipped.push(SkippedRecipient {
                            root_id: root_id.clone(),
                            reason: DmRecipientSkipReason::NotFriend,
                        });
                    } else {
                        out.accepted.push(root_id.clone());
                    }
                    continue;
                }
            };
            // 仅聊天：仅 feed 通道检查
            if channel == DmChannel::Feed && friend.permission == "chatOnly" {
                out.skipped.push(SkippedRecipient {
                    root_id: root_id.clone(),
                    reason: DmRecipientSkipReason::ChatOnly,
                });
                continue;
            }
            out.accepted.push(root_id.clone());
        }
        Ok(out)
    }
}

/// 拉黑集合（`ct:blocked:{rootId}` → `"1"`；键存在即拉黑）。
fn blocked_set<S: StorageBackend>(storage: &S) -> Result<std::collections::HashSet<String>> {
    let mut set = std::collections::HashSet::new();
    for (key, _) in storage.scan(&ScanOptions::prefix(BLOCKED_PREFIX))? {
        if let Some(root_id) = key.strip_prefix(BLOCKED_PREFIX) {
            set.insert(root_id.to_string());
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::ContactService;
    use crate::storage::MemoryStorage;

    fn friend(root_id: &str, permission: &str) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            permission: permission.to_string(),
            ..Default::default()
        }
    }

    fn set_blocked(storage: &mut MemoryStorage, root_id: &str) {
        storage.put(&format!("{BLOCKED_PREFIX}{root_id}"), "1").unwrap();
    }

    fn ids(v: &[String]) -> Vec<String> {
        v.to_vec()
    }

    #[test]
    fn feed_filters_not_friend_blocked_and_chatonly() {
        let mut s = MemoryStorage::new();
        // 本机朋友：open、chatOnly
        ContactService::upsert_friend(&mut s, &friend("root-open", "open")).unwrap();
        ContactService::upsert_friend(&mut s, &friend("root-chatonly", "chatOnly")).unwrap();
        // 拉黑一个朋友 + 一个非朋友
        set_blocked(&mut s, "root-blocked");
        set_blocked(&mut s, "root-nonfriend");

        let recipients = ids(&[
            "root-open".into(),
            "root-chatonly".into(),
            "root-blocked".into(),
            "root-nonfriend".into(),
            "root-missing".into(),
        ]);
        let out = ContactService::filter_dm_recipients(&s, &recipients, DmChannel::Feed).unwrap();
        // 只放行 open
        assert_eq!(out.accepted, vec!["root-open".to_string()]);
        // 跳过顺序 = 入参顺序
        let reasons: Vec<(String, DmRecipientSkipReason)> = out
            .skipped
            .iter()
            .map(|r| (r.root_id.clone(), r.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("root-chatonly".to_string(), DmRecipientSkipReason::ChatOnly),
                ("root-blocked".to_string(), DmRecipientSkipReason::Blocked),
                ("root-nonfriend".to_string(), DmRecipientSkipReason::Blocked),
                ("root-missing".to_string(), DmRecipientSkipReason::NotFriend),
            ]
        );
    }

    #[test]
    fn chat_only_filters_blocked() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &friend("root-open", "open")).unwrap();
        ContactService::upsert_friend(&mut s, &friend("root-chatonly", "chatOnly")).unwrap();
        set_blocked(&mut s, "root-blocked");

        let recipients = ids(&[
            "root-open".into(),
            "root-chatonly".into(),
            "root-blocked".into(),
            "root-nonfriend".into(),
        ]);
        let out = ContactService::filter_dm_recipients(&s, &recipients, DmChannel::Chat).unwrap();
        // chat 通道：仅拉黑被过滤；chatOnly 与非朋友放行
        assert_eq!(
            out.accepted,
            vec!["root-open".to_string(), "root-chatonly".to_string(), "root-nonfriend".to_string()]
        );
        assert_eq!(
            out.skipped,
            vec![SkippedRecipient {
                root_id: "root-blocked".to_string(),
                reason: DmRecipientSkipReason::Blocked,
            }]
        );
    }
}
