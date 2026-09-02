//! 通讯录只读门面（社交投递层 social-feed §9.4 `contact:read` 最小只读面）。
//!
//! 供插件 SDK `contacts` 模块消费：`list_friends` 返回裁剪后的朋友只读摘要，
//! `list_groups` / `list_tags` 复用既有 `ContactService::list_groups` /
//! `list_tags`（group.rs / tag.rs，纯查询，按 order 升序）。
//!
//! 纯查询不加 io_lock（对齐内核查询类方法惯例，如 `message_list_conversations`）；
//! 仅返回 `FriendSummary` 等裁剪后的只读摘要，不暴露 FriendRecord 的敏感字段
//! （签名/性别/电话/备忘/照片/设备寻址等），字段裁剪理由见 [`crate::contact::FriendSummary`]。

use crate::contact::FriendSummary;
use crate::storage::StorageBackend;

use super::super::FRIEND_PREFIX;
use super::scan_json;

impl super::ContactService {
    /// 列出所有朋友的只读摘要（个人空间；按存储顺序）。
    ///
    /// 与 `overview` 的差异：不 overlay 拉黑集合（`blocked` 字段被裁剪）、
    /// 不保证注入「自己」条目（`ensure_self_friend` 属 overview 侧编排，只读
    /// 门面不产生副作用）。
    pub fn list_friends<S: StorageBackend>(
        storage: &S,
    ) -> Result<Vec<FriendSummary>, crate::contact::ContactError> {
        let records = scan_json::<S, crate::contact::FriendRecord>(storage, FRIEND_PREFIX)?;
        Ok(records
            .into_iter()
            .map(|(_, record)| FriendSummary {
                root_id: record.root_id,
                nickname: record.nickname,
                avatar: record.avatar,
                group_id: record.group_id,
                tag_ids: record.tag_ids,
                permission: record.permission,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::contact::{ContactService, FriendRecord};
    use crate::storage::MemoryStorage;

    /// 构造带满字段的朋友记录：敏感字段（签名/性别/电话/备忘/照片/peers）置值，
    /// 用于断言只读摘要的字段裁剪正确剔除。
    fn rich_friend(root_id: &str, permission: &str) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            nickname: "阿强".to_string(),
            avatar: Some("data:image/png;base64,AAA".to_string()),
            // 以下均应为敏感字段，被 `FriendSummary` 裁剪掉
            signature: "秘密签名".to_string(),
            gender: Some("male".to_string()),
            phones: vec!["13800000000".to_string()],
            memo: "内部备忘".to_string(),
            photos: vec!["data:image/png;base64,BBB".to_string()],
            peers: vec![crate::contact::PeerRef {
                peer_id: "peer-1".to_string(),
                addresses: vec!["/ip4/1.2.3.4/tcp/4001".to_string()],
                ..Default::default()
            }],
            // 保留字段
            group_id: "grp_001".to_string(),
            tag_ids: vec!["tag_a".to_string(), "tag_b".to_string()],
            permission: permission.to_string(),
            ..Default::default()
        }
    }

    /// 字段裁剪正确：只暴露 rootId/nickname/avatar/groupId/tagIds/permission，
    /// 敏感字段（签名/性别/电话/备忘/照片/peers/remark/blocked）不出现。
    #[test]
    fn list_friends_trims_sensitive_fields() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &rich_friend("root-a", "open")).unwrap();

        let friends = ContactService::list_friends(&s).unwrap();
        assert_eq!(friends.len(), 1);
        let summary = &friends[0];
        assert_eq!(summary.root_id, "root-a");
        assert_eq!(summary.nickname, "阿强");
        assert_eq!(summary.avatar.as_deref(), Some("data:image/png;base64,AAA"));
        assert_eq!(summary.group_id, "grp_001");
        assert_eq!(
            summary.tag_ids,
            vec!["tag_a".to_string(), "tag_b".to_string()]
        );
        assert_eq!(summary.permission, "open");

        // 序列化线形：敏感字段的键不得出现
        let json = serde_json::to_value(&summary).unwrap();
        for sensitive in [
            "signature",
            "gender",
            "phones",
            "memo",
            "photos",
            "peers",
            "remark",
            "blocked",
            "addedAt",
            "updatedAt",
        ] {
            assert!(
                json.get(sensitive).is_none(),
                "敏感字段 {sensitive} 不应暴露"
            );
        }
        // 保留字段的 camelCase 键齐全
        for kept in ["rootId", "nickname", "groupId", "tagIds", "permission"] {
            assert!(json.get(kept).is_some(), "保留字段 {kept} 应存在");
        }
        assert!(json.get("avatar").is_some(), "有头像时 avatar 键应在");
    }

    /// permission 字段透出（open / chatOnly 原样返回）。`list_friends` 走
    /// `scan_json`（BTreeMap 按 key 字典序返回）：root-chatonly < root-open
    /// （'c' < 'o'），故断言用 key 序。
    #[test]
    fn list_friends_passthrough_permission() {
        let mut s = MemoryStorage::new();
        ContactService::upsert_friend(&mut s, &rich_friend("root-open", "open")).unwrap();
        ContactService::upsert_friend(&mut s, &rich_friend("root-chatonly", "chatOnly")).unwrap();

        let friends = ContactService::list_friends(&s).unwrap();
        let perms: Vec<&str> = friends.iter().map(|f| f.permission.as_str()).collect();
        assert_eq!(perms, vec!["chatOnly", "open"]);
    }

    /// 空集合：无朋友记录时返回空数组（不报错）。
    #[test]
    fn list_friends_empty_collection() {
        let s = MemoryStorage::new();
        assert!(ContactService::list_friends(&s).unwrap().is_empty());
    }
}
