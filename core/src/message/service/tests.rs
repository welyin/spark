//! 消息服务单元测试（从 `service.rs` 拆出，文件长度约束）。

use super::*;
use crate::storage::MemoryStorage;
use crate::message::types::{ConversationKind, ConversationRecord};

mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::message::types::{ConversationKind, ConversationRecord};

    fn conv(id: &str, peer: &str) -> ConversationRecord {
        ConversationRecord {
            id: id.to_string(),
            kind: ConversationKind::Direct,
            title: peer.to_string(),
            peer_root_id: peer.to_string(),
            peer: None,
            unread_count: 0,
            pinned_at: 0,
            muted: false,
            draft: String::new(),
            updated_at: 0,
            meta_updated_at: 0,
        }
    }

    /// merge_conv_meta：远端同步字段覆盖本地，但本地消息驱动字段保留。
    #[test]
    fn merge_conv_meta_takes_sync_fields_keeps_local_driven() {
        let mut local = conv("c1", "peer1");
        local.unread_count = 5; // 本地消息驱动字段
        local.updated_at = 1000; // 本地消息驱动字段
        local.muted = false;
        local.pinned_at = 0;
        local.draft = String::new();

        let mut remote = conv("c1", "peer1");
        remote.muted = true; // 远端同步字段
        remote.pinned_at = 2000;
        remote.draft = "草稿".to_string();
        remote.meta_updated_at = 3000;
        // 远端消息驱动字段（应被忽略，保留本地）
        remote.unread_count = 0;
        remote.updated_at = 500;

        MessageService::merge_conv_meta(&mut local, &remote);

        assert_eq!(local.muted, true);
        assert_eq!(local.pinned_at, 2000);
        assert_eq!(local.draft, "草稿");
        assert_eq!(local.meta_updated_at, 3000);
        // 本地消息驱动字段保留
        assert_eq!(local.unread_count, 5);
        assert_eq!(local.updated_at, 1000);
    }

    /// pdsync 会话写入（个人空间）：写记录 + bump pmeta。
    #[test]
    fn upsert_conversation_pdsync_writes_pmeta_for_personal() {
        let mut storage = MemoryStorage::new();
        let record = conv("c1", "peer1");
        MessageService::upsert_conversation_pdsync(
            &mut storage,
            "personal",
            &record,
            1000,
            Some("node-a"),
        )
        .unwrap();

        let key = conversation_key("personal", "c1");
        assert!(storage.get(&key).unwrap().is_some());
        // pmeta 存在且 vv 含 node-a:1
        let pmeta = crate::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
        assert_eq!(pmeta.vv.get("node-a"), Some(&1));
    }

    /// pdsync 会话写入（组织空间）：不写 pmeta（仅组织/应用会话不走 pdsync）。
    #[test]
    fn upsert_conversation_pdsync_skips_meta_for_org() {
        let mut storage = MemoryStorage::new();
        let record = conv("c1", "peer1");
        MessageService::upsert_conversation_pdsync(
            &mut storage,
            "org:abc",
            &record,
            1000,
            Some("node-a"),
        )
        .unwrap();

        let key = conversation_key("org:abc", "c1");
        assert!(storage.get(&key).unwrap().is_some());
        assert!(crate::sync::get_personal_meta(&storage, &key).unwrap().is_none());
    }

    fn text_msg(id: &str, created_at: i64) -> MessageRecord {
        MessageRecord {
            id: id.to_string(),
            sender_id: "peer1".to_string(),
            sender_name: "对方".to_string(),
            msg_type: crate::message::MessageType::Text,
            content: "hi".to_string(),
            file_size: None,
            duration: None,
            link: None,
            quote: None,
            created_at,
            status: None,
            recalled: false,
            read: false,
        }
    }

    /// pdsync 追加消息（个人空间）：只收发消息的会话壳也 bump pmeta。
    #[test]
    fn append_message_pdsync_bumps_conv_pmeta() {
        let mut storage = MemoryStorage::new();
        // 存量会话壳：裸写，无 pmeta
        MessageService::upsert_conversation(&mut storage, "personal", &conv("c1", "peer1")).unwrap();
        let key = conversation_key("personal", "c1");
        assert!(crate::sync::get_personal_meta(&storage, &key).unwrap().is_none());

        MessageService::append_message_pdsync(
            &mut storage,
            "personal",
            "c1",
            &text_msg("m1", 1000),
            1000,
            Some("node-a"),
        )
        .unwrap();
        let pmeta = crate::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
        assert_eq!(pmeta.vv.get("node-a"), Some(&1));
        // 会话 updated_at 仍按消息时间推进
        let c = MessageService::get_conversation(&storage, "personal", "c1").unwrap().unwrap();
        assert_eq!(c.updated_at, 1000);

        // 组织空间：不 bump（组织会话不走 pdsync）
        MessageService::upsert_conversation(&mut storage, "org:abc", &conv("c2", "peer1")).unwrap();
        MessageService::append_message_pdsync(
            &mut storage,
            "org:abc",
            "c2",
            &text_msg("m2", 1000),
            1000,
            Some("node-a"),
        )
        .unwrap();
        let key = conversation_key("org:abc", "c2");
        assert!(crate::sync::get_personal_meta(&storage, &key).unwrap().is_none());
    }

    /// 消息 append 的 conv pmeta bump 不推高 ts：ts 保持会话的
    /// `meta_updated_at`（LWW 依据元数据编辑时间，消息流不得扭曲
    /// pin/mute/draft 的并发裁决），vv 照常递增。
    #[test]
    fn append_message_pdsync_bump_preserves_meta_ts() {
        let mut storage = MemoryStorage::new();
        // 会话在 t=100 被 pin：meta_updated_at=100，pmeta.ts=100
        let mut c = conv("c1", "peer1");
        c.pinned_at = 100;
        c.meta_updated_at = 100;
        MessageService::upsert_conversation_pdsync(
            &mut storage,
            "personal",
            &c,
            100,
            Some("node-a"),
        )
        .unwrap();
        let key = conversation_key("personal", "c1");
        let before = crate::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
        assert_eq!(before.ts, 100);

        // t=5000 仅追加消息：vv 递增，pmeta.ts 仍为 100
        MessageService::append_message_pdsync(
            &mut storage,
            "personal",
            "c1",
            &text_msg("m1", 5000),
            5000,
            Some("node-a"),
        )
        .unwrap();
        let after = crate::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
        assert_eq!(after.vv.get("node-a"), Some(&2), "vv 照常递增");
        assert_eq!(after.ts, 100, "消息 append 不得推高 conv pmeta.ts");

        // 存量会话壳（meta_updated_at=0，裸写无 pmeta）：append bump 后 ts=0
        MessageService::upsert_conversation(&mut storage, "personal", &conv("c2", "peer1")).unwrap();
        MessageService::append_message_pdsync(
            &mut storage,
            "personal",
            "c2",
            &text_msg("m2", 9000),
            9000,
            Some("node-a"),
        )
        .unwrap();
        let key2 = conversation_key("personal", "c2");
        let pmeta2 = crate::sync::get_personal_meta(&storage, &key2).unwrap().unwrap();
        assert_eq!(pmeta2.vv.get("node-a"), Some(&1));
        assert_eq!(pmeta2.ts, 0, "无元数据编辑的会话 ts 保持 meta_updated_at=0");
    }

    /// pdsync 删除会话（个人空间）：记录删除 + tombstone pmeta。
    #[test]
    fn delete_conversation_pdsync_leaves_tombstone() {
        let mut storage = MemoryStorage::new();
        let record = conv("c1", "peer1");
        MessageService::upsert_conversation_pdsync(
            &mut storage,
            "personal",
            &record,
            1000,
            Some("node-a"),
        )
        .unwrap();
        MessageService::append_message(
            &mut storage,
            "personal",
            "c1",
            &text_msg("m1", 1000),
        )
        .unwrap();

        MessageService::delete_conversation_pdsync(
            &mut storage,
            "personal",
            "c1",
            2000,
            Some("node-a"),
        )
        .unwrap();
        let key = conversation_key("personal", "c1");
        // 记录与消息均删除
        assert!(storage.get(&key).unwrap().is_none());
        assert!(MessageService::get_messages(&storage, "personal", "c1").unwrap().is_empty());
        // tombstone pmeta 保留（删除可传播），vv 递增
        let pmeta = crate::sync::get_personal_meta(&storage, &key).unwrap().unwrap();
        assert!(crate::sync::is_tombstone(&pmeta));
        assert_eq!(pmeta.vv.get("node-a"), Some(&2));

        // 空删（记录不存在）：不产生垃圾 tombstone
        MessageService::delete_conversation_pdsync(
            &mut storage,
            "personal",
            "ghost",
            3000,
            Some("node-a"),
        )
        .unwrap();
        let key = conversation_key("personal", "ghost");
        assert!(crate::sync::get_personal_meta(&storage, &key).unwrap().is_none());
    }

