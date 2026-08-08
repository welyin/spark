//! 自设备通讯录快照同步（contact-sync 信封的构建与合入纯逻辑）。
//!
//! 同步范围（个人空间）：朋友记录、收到/发出的好友申请、标签数组、
//! 分组数组、拉黑集合。聊天记录不在本模块（见 dm 消息链路）。
//!
//! 裁决口径（LWW）：
//! - 朋友：记录级 `updatedAt`（本机每次变更刷新；存量记录为 0，任何带
//!   `updatedAt` 的快照条目严格更新、必然覆盖）。自记录（rootId == 本机
//!   身份）不参与同步——其 `peer` 字段指向的是「本机视角的对端设备」，
//!   两台设备各自正确、不可互灌。
//! - 申请：`updatedAt` 记录级 LWW（状态流转/内容更新都刷新该字段）。
//! - 标签/分组/拉黑：整域版本号（`ct:sync-meta` 内 tags/groups/blocked
//!   三个时间戳，本机每次变更对应域刷新）。整域替换保证分组顺序、
//!   删除传播（删标签/删分组/取消拉黑）一并生效——这是整域 LWW 相对
//!   记录级 LWW 的关键优势（无墓碑需求）。
//!
//! 删除传播：朋友删除与申请删除不传播（朋友无墓碑；申请本就只流转不
//! 删除）。标签/分组/拉黑的删除随整域替换自然传播。

use serde_json::{Map, Value};

use crate::storage::{ScanOptions, StorageBackend};

use super::super::{
    BLOCKED_PREFIX, ContactTag, FRIEND_PREFIX, FriendRecord, FriendRequestRecord, GROUPS_KEY,
    REQ_IN_PREFIX, REQ_OUT_PREFIX, Result, TAGS_KEY,
};
use super::{ContactService, read_json, read_vec, scan_json, write_json};

/// 整域版本号存储键（`{"tags":ts,"groups":ts,"blocked":ts}`）。
pub(crate) const CONTACT_SYNC_META_KEY: &str = "ct:sync-meta";

/// 可整域同步的域。
#[derive(Clone, Copy, Debug)]
pub(crate) enum SyncDomain {
    Tags,
    Groups,
    Blocked,
}

/// 读取整域版本号（缺失键/字段按 0——任何正版本号都严格更新）。
fn read_versions<S: StorageBackend>(storage: &S) -> Result<(i64, i64, i64)> {
    let raw: Option<String> = storage.get(CONTACT_SYNC_META_KEY)?;
    let Some(raw) = raw else {
        return Ok((0, 0, 0));
    };
    let value: Value = serde_json::from_str(&raw)?;
    let get = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
    Ok((get("tags"), get("groups"), get("blocked")))
}

/// 刷新某域的本地版本号（本机变更时调用；取 max 防时钟回拨导致版本倒退）。
pub(crate) fn bump_version<S: StorageBackend>(
    storage: &mut S,
    domain: SyncDomain,
    now_ms: i64,
) -> Result<()> {
    let (mut tags, mut groups, mut blocked) = read_versions(storage)?;
    match domain {
        SyncDomain::Tags => tags = tags.max(now_ms),
        SyncDomain::Groups => groups = groups.max(now_ms),
        SyncDomain::Blocked => blocked = blocked.max(now_ms),
    }
    write_json(
        storage,
        CONTACT_SYNC_META_KEY,
        &serde_json::json!({"tags": tags, "groups": groups, "blocked": blocked}),
    )
}

/// 拉黑集合（rootId 列表，键序稳定）。
fn blocked_list<S: StorageBackend>(storage: &S) -> Result<Vec<String>> {
    let rows = storage.scan(&ScanOptions::prefix(BLOCKED_PREFIX))?;
    let mut list: Vec<String> = rows
        .into_iter()
        .map(|(key, _)| key[BLOCKED_PREFIX.len()..].to_string())
        .collect();
    list.sort();
    Ok(list)
}

/// 申请记录的 LWW 版本（updatedAt 由写路径维护；兜底 createdAt）。
fn request_version(record: &FriendRequestRecord) -> i64 {
    record.updated_at.max(record.created_at)
}

/// 构建 contact-sync 快照（信封 body）。自记录（`my_root_id`）不入快照。
pub(crate) fn build_contact_sync_snapshot<S: StorageBackend>(
    storage: &S,
    my_root_id: &str,
) -> Result<Value> {
    let friends: Vec<FriendRecord> = scan_json::<S, FriendRecord>(storage, FRIEND_PREFIX)?
        .into_iter()
        .map(|(_, record)| record)
        .filter(|record| record.root_id != my_root_id)
        .collect();
    let requests_in: Vec<FriendRequestRecord> =
        scan_json::<S, FriendRequestRecord>(storage, REQ_IN_PREFIX)?
            .into_iter()
            .map(|(_, record)| record)
            .collect();
    let requests_out: Vec<FriendRequestRecord> =
        scan_json::<S, FriendRequestRecord>(storage, REQ_OUT_PREFIX)?
            .into_iter()
            .map(|(_, record)| record)
            .collect();
    let tags: Vec<ContactTag> = read_vec(storage, TAGS_KEY)?;
    let groups: Vec<super::super::ContactGroup> = read_vec(storage, GROUPS_KEY)?;
    let blocked = blocked_list(storage)?;
    let (tags_v, groups_v, blocked_v) = read_versions(storage)?;
    Ok(serde_json::json!({
        "versions": { "tags": tags_v, "groups": groups_v, "blocked": blocked_v },
        "friends": friends,
        "requestsIn": requests_in,
        "requestsOut": requests_out,
        "tags": tags,
        "groups": groups,
        "blocked": blocked,
    }))
}

/// 合入 contact-sync 快照（LWW；返回实际写入的条目数——供上层决定是否
/// 通知前端刷新）。仅处理个人空间数据。
///
/// 合入是「裸写」：不刷新本地版本/updatedAt（快照里的时间戳即事实来源），
/// 也不会再触发对外广播——防两端互灌循环。
pub(crate) fn apply_contact_sync_snapshot<S: StorageBackend>(
    storage: &mut S,
    my_root_id: &str,
    body: &Value,
) -> Result<usize> {
    let mut applied = 0usize;

    // 1) 朋友：记录级 LWW（跳过自记录）
    if let Some(items) = body.get("friends").and_then(Value::as_array) {
        for item in items {
            let Ok(incoming) = serde_json::from_value::<FriendRecord>(item.clone()) else {
                continue;
            };
            if incoming.root_id.is_empty() || incoming.root_id == my_root_id {
                continue;
            }
            let local = ContactService::get_friend(storage, &incoming.root_id)?;
            let dominated = local
                .as_ref()
                .map(|l| l.updated_at >= incoming.updated_at)
                .unwrap_or(false);
            if dominated {
                continue;
            }
            ContactService::upsert_friend(storage, &incoming)?;
            applied += 1;
        }
    }

    // 2) 申请（in/out 两个方向）：记录级 LWW
    for (field, prefix) in [("requestsIn", REQ_IN_PREFIX), ("requestsOut", REQ_OUT_PREFIX)] {
        if let Some(items) = body.get(field).and_then(Value::as_array) {
            for item in items {
                let Ok(incoming) = serde_json::from_value::<FriendRequestRecord>(item.clone())
                else {
                    continue;
                };
                if incoming.id.is_empty() {
                    continue;
                }
                let key = format!("{prefix}{}", incoming.id);
                let local: Option<FriendRequestRecord> = read_json(storage, &key)?;
                let dominated = local
                    .as_ref()
                    .map(|l| request_version(l) >= request_version(&incoming))
                    .unwrap_or(false);
                if dominated {
                    continue;
                }
                write_json(storage, &key, &incoming)?;
                applied += 1;
            }
        }
    }

    // 3) 标签/分组/拉黑：整域 LWW（版本严格更新才整域替换）
    let (local_tags_v, local_groups_v, local_blocked_v) = read_versions(storage)?;
    let versions = body.get("versions").cloned().unwrap_or(Value::Null);
    let remote = |key: &str| versions.get(key).and_then(Value::as_i64).unwrap_or(0);
    let (remote_tags_v, remote_groups_v, remote_blocked_v) =
        (remote("tags"), remote("groups"), remote("blocked"));

    let mut tags_v = local_tags_v;
    let mut groups_v = local_groups_v;
    let mut blocked_v = local_blocked_v;

    if remote_tags_v > local_tags_v {
        if let Some(items) = body.get("tags").and_then(Value::as_array) {
            let tags: Vec<ContactTag> = items
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            write_json(storage, TAGS_KEY, &tags)?;
            tags_v = remote_tags_v;
            applied += 1;
        }
    }
    if remote_groups_v > local_groups_v {
        if let Some(items) = body.get("groups").and_then(Value::as_array) {
            let groups: Vec<super::super::ContactGroup> = items
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            write_json(storage, GROUPS_KEY, &groups)?;
            groups_v = remote_groups_v;
            applied += 1;
        }
    }
    if remote_blocked_v > local_blocked_v {
        if let Some(items) = body.get("blocked").and_then(Value::as_array) {
            let remote_set: std::collections::BTreeSet<String> = items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let local_set: std::collections::BTreeSet<String> =
                blocked_list(storage)?.into_iter().collect();
            for root_id in local_set.difference(&remote_set) {
                storage.delete(&format!("{BLOCKED_PREFIX}{root_id}"))?;
            }
            for root_id in remote_set.difference(&local_set) {
                storage.put(&format!("{BLOCKED_PREFIX}{root_id}"), "1")?;
            }
            blocked_v = remote_blocked_v;
            applied += 1;
        }
    }

    // 版本落库（取 max：未被替换的域保留本地更高版本）
    if tags_v != local_tags_v || groups_v != local_groups_v || blocked_v != local_blocked_v {
        write_json(
            storage,
            CONTACT_SYNC_META_KEY,
            &serde_json::json!({"tags": tags_v, "groups": groups_v, "blocked": blocked_v}),
        )?;
    }

    Ok(applied)
}

/// 序列化辅助：body 中的可选字段补齐（供调用方调试/日志）。
#[allow(dead_code)]
pub(crate) fn snapshot_summary(body: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["friends", "requestsIn", "requestsOut", "tags", "groups", "blocked"] {
        let count = body
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        out.insert(key.to_string(), Value::from(count));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact::{ContactGroup, FriendRequestStatus};
    use crate::storage::MemoryStorage;

    const NOW: i64 = 1_720_000_000_000;
    const MY_ROOT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn friend(root_id: &str, nickname: &str, updated_at: i64) -> FriendRecord {
        FriendRecord {
            root_id: root_id.to_string(),
            nickname: nickname.to_string(),
            avatar: None,
            signature: String::new(),
            gender: None,
            added_at: NOW,
            peer: None,
            remark: String::new(),
            phones: Vec::new(),
            tag_ids: Vec::new(),
            group_id: String::new(),
            memo: String::new(),
            photos: Vec::new(),
            permission: "open".to_string(),
            blocked: false,
            updated_at,
        }
    }

    fn request(id: &str, root_id: &str, status: FriendRequestStatus, updated_at: i64) -> FriendRequestRecord {
        FriendRequestRecord {
            id: id.to_string(),
            root_id: root_id.to_string(),
            nickname: "申请人".to_string(),
            avatar: None,
            message: String::new(),
            source: String::new(),
            status,
            created_at: NOW,
            updated_at,
            peer: None,
            thread: Vec::new(),
            invite_code: None,
        }
    }

    /// 端到端：A 机构建快照 → B 机合入 → 数据齐全且版本对齐。
    #[test]
    fn roundtrip_build_then_apply() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        ContactService::upsert_friend(&mut a, &friend(&"bb".repeat(32), "好友甲", NOW)).unwrap();
        ContactService::upsert_friend(&mut a, &friend(MY_ROOT, "自己", NOW)).unwrap(); // 自记录
        ContactService::put_incoming_request(
            &mut a,
            &request("in-1", &"cc".repeat(32), FriendRequestStatus::Pending, NOW),
        )
        .unwrap();
        ContactService::put_outgoing_request(
            &mut a,
            &request("out-1", &"dd".repeat(32), FriendRequestStatus::Pending, NOW),
        )
        .unwrap();
        ContactService::create_tag_with_id(&mut a, "personal", "t1", "邻居", NOW).unwrap();
        ContactService::create_group_with_id(&mut a, "g1", "家人", NOW).unwrap();
        ContactService::set_blocked(&mut a, "personal", &"ee".repeat(32), true, NOW).unwrap();

        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        let applied = apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        assert!(applied > 0);

        // 朋友同步（自记录除外）
        assert!(ContactService::get_friend(&b, &"bb".repeat(32)).unwrap().is_some());
        assert!(
            ContactService::get_friend(&b, MY_ROOT).unwrap().is_none(),
            "自记录不同步"
        );
        // 申请双向
        assert!(ContactService::get_incoming_request(&b, "in-1").unwrap().is_some());
        assert!(ContactService::get_outgoing_request(&b, "out-1").unwrap().is_some());
        // 标签/分组/拉黑
        let view = ContactService::overview(&b, "personal").unwrap();
        assert_eq!(view.tags.len(), 1);
        assert_eq!(view.groups.len(), 1);
        assert!(ContactService::is_blocked(&b, &"ee".repeat(32)).unwrap());

        // 幂等：重放同快照无新写入
        let again = apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        assert_eq!(again, 0, "同快照重放幂等");
    }

    /// 朋友 LWW：旧快照不覆盖新记录；新快照覆盖旧记录。
    #[test]
    fn friend_lww() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let root = "bb".repeat(32);

        ContactService::upsert_friend(&mut a, &friend(&root, "旧名", NOW)).unwrap();
        ContactService::upsert_friend(&mut b, &friend(&root, "新名", NOW + 1000)).unwrap();

        // A（旧）→ B（新）：不覆盖
        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        let applied = apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        assert_eq!(applied, 0);
        assert_eq!(
            ContactService::get_friend(&b, &root).unwrap().unwrap().nickname,
            "新名"
        );

        // B（新）→ A（旧）：覆盖
        let body = build_contact_sync_snapshot(&b, MY_ROOT).unwrap();
        let applied = apply_contact_sync_snapshot(&mut a, MY_ROOT, &body).unwrap();
        assert_eq!(applied, 1);
        assert_eq!(
            ContactService::get_friend(&a, &root).unwrap().unwrap().nickname,
            "新名"
        );
    }

    /// 申请记录 LWW：状态流转（pending→accepted，updatedAt 前进）覆盖旧状态。
    #[test]
    fn request_lww() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        ContactService::put_outgoing_request(
            &mut a,
            &request("r1", &"cc".repeat(32), FriendRequestStatus::Pending, NOW),
        )
        .unwrap();
        ContactService::put_outgoing_request(
            &mut b,
            &request("r1", &"cc".repeat(32), FriendRequestStatus::Accepted, NOW + 500),
        )
        .unwrap();

        // 旧（pending）→ 新（accepted）：不动
        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        assert_eq!(apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap(), 0);
        // 新 → 旧：覆盖
        let body = build_contact_sync_snapshot(&b, MY_ROOT).unwrap();
        assert_eq!(apply_contact_sync_snapshot(&mut a, MY_ROOT, &body).unwrap(), 1);
        assert_eq!(
            ContactService::get_outgoing_request(&a, "r1").unwrap().unwrap().status,
            FriendRequestStatus::Accepted
        );
    }

    /// 整域 LWW：分组删除随整域替换传播（版本更高的空列表覆盖非空列表）。
    #[test]
    fn groups_deletion_propagates() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        // 双方起初都有 g1+g2
        for s in [&mut a, &mut b] {
            ContactService::create_group_with_id(s, "g1", "家人", NOW).unwrap();
            ContactService::create_group_with_id(s, "g2", "同学", NOW + 1).unwrap();
        }
        // A 删除 g2（版本前进）
        ContactService::delete_group(&mut a, "g2", NOW + 2000).unwrap();

        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        let applied = apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        assert!(applied > 0);
        let view = ContactService::overview(&b, "personal").unwrap();
        assert_eq!(view.groups.len(), 1, "删除随整域替换传播");
        assert_eq!(view.groups[0].id, "g1");
    }

    /// 存量记录（updatedAt=0）被任何带版本的快照条目覆盖。
    #[test]
    fn legacy_record_dominated() {
        let mut b = MemoryStorage::new();
        let root = "bb".repeat(32);
        // 存量：updatedAt = 0
        ContactService::upsert_friend(&mut b, &friend(&root, "旧", 0)).unwrap();

        let mut a = MemoryStorage::new();
        ContactService::upsert_friend(&mut a, &friend(&root, "新", 1)).unwrap();
        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        assert_eq!(apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap(), 1);
        assert_eq!(
            ContactService::get_friend(&b, &root).unwrap().unwrap().nickname,
            "新"
        );
    }

    /// 拉黑集合：取消拉黑（删除条目）随整域替换传播。
    #[test]
    fn unblock_propagates() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let target = "ee".repeat(32);
        for (s, ts) in [(&mut a, NOW), (&mut b, NOW)] {
            ContactService::set_blocked(s, "personal", &target, true, ts).unwrap();
        }
        // A 取消拉黑（版本前进）
        ContactService::set_blocked(&mut a, "personal", &target, false, NOW + 3000).unwrap();

        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        let applied = apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        assert!(applied > 0);
        assert!(
            !ContactService::is_blocked(&b, &target).unwrap(),
            "取消拉黑随整域替换传播"
        );
    }

    /// 分组顺序与重排传播（数组顺序即显示顺序）。
    #[test]
    fn group_order_propagates() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        for s in [&mut a, &mut b] {
            ContactService::create_group_with_id(s, "g1", "甲", NOW).unwrap();
            ContactService::create_group_with_id(s, "g2", "乙", NOW + 1).unwrap();
        }
        ContactService::move_group(&mut a, "g2", 0, NOW + 5000).unwrap();

        let body = build_contact_sync_snapshot(&a, MY_ROOT).unwrap();
        apply_contact_sync_snapshot(&mut b, MY_ROOT, &body).unwrap();
        let view = ContactService::overview(&b, "personal").unwrap();
        let order: Vec<&str> = view.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(order, vec!["g2", "g1"], "重排结果随快照传播");

        // ContactGroup 结构检查（serde 线形不含多余字段）
        let _: ContactGroup = view.groups[0].clone();
    }
}
