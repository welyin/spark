//! 好友申请（个人空间）：收到的 `ct:req:in:` 与发出的 `ct:req:out:`。

use crate::storage::{ScanOptions, StorageBackend};

use super::*;
use crate::contact::{
    FriendRequestRecord, FriendRequestStatus, PeerRef, REQ_IN_PREFIX, REQ_OUT_PREFIX,
};

impl ContactService {
    /// 落库一条收到的好友申请（`ct:req:in:{id}`）。
    pub fn put_incoming_request<S: StorageBackend>(
        storage: &mut S,
        request: &FriendRequestRecord,
    ) -> Result<()> {
        write_json(storage, &format!("{REQ_IN_PREFIX}{}", request.id), request)
    }

    /// 落库一条发出的好友申请（`ct:req:out:{id}`；id 由调用方给定，kernel
    /// 门面以客户端生成的 id 落库，信封 `requestId` 与之对应）。
    ///
    /// `updated_at` 兜底：未填（0，如 serde default 的旧数据）时取
    /// `created_at`——新建 = createdAt，后续变更由调用方刷新后传入。
    pub fn put_outgoing_request<S: StorageBackend>(
        storage: &mut S,
        request: &FriendRequestRecord,
    ) -> Result<()> {
        let mut request = request.clone();
        if request.updated_at == 0 {
            request.updated_at = request.created_at;
        }
        write_json(storage, &format!("{REQ_OUT_PREFIX}{}", request.id), &request)
    }

    /// 读取收到的好友申请；不存在返回 `Ok(None)`。
    pub fn get_incoming_request<S: StorageBackend>(
        storage: &S,
        id: &str,
    ) -> Result<Option<FriendRequestRecord>> {
        read_json(storage, &format!("{REQ_IN_PREFIX}{id}"))
    }

    /// 读取发出的好友申请；不存在返回 `Ok(None)`。
    pub fn get_outgoing_request<S: StorageBackend>(
        storage: &S,
        id: &str,
    ) -> Result<Option<FriendRequestRecord>> {
        read_json(storage, &format!("{REQ_OUT_PREFIX}{id}"))
    }

    /// 处理收到的申请：pending → accepted / ignored（对齐 TS `resolveRequest`）。
    ///
    /// 非 pending 或不存在时忽略并返回 `Ok(false)`；成功流转返回 `Ok(true)`。
    /// 接受后的建朋友/权限写入由调用方完成（`upsert_friend` + `update_profile`）。
    pub fn resolve_incoming_request<S: StorageBackend>(
        storage: &mut S,
        id: &str,
        accept: bool,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(mut request) = Self::get_incoming_request(storage, id)? else {
            return Ok(false);
        };
        if request.status != FriendRequestStatus::Pending {
            return Ok(false);
        }
        request.status = if accept {
            FriendRequestStatus::Accepted
        } else {
            FriendRequestStatus::Ignored
        };
        request.updated_at = now_ms;
        Self::put_incoming_request(storage, &request)?;
        Ok(true)
    }

    /// 发出添加请求（对齐 TS `sendFriendRequest`）：写入本地 outbox，id 为
    /// `out-{now_ms}-{count}` 风格，同毫秒冲突时递增后缀避让。
    pub fn create_outgoing_request<S: StorageBackend>(
        storage: &mut S,
        root_id: &str,
        nickname: &str,
        message: &str,
        source: &str,
        peer: Option<PeerRef>,
        now_ms: i64,
    ) -> Result<FriendRequestRecord> {
        let mut count = storage.scan(&ScanOptions::prefix(REQ_OUT_PREFIX))?.len();
        let mut id = format!("out-{now_ms}-{count}");
        while storage.get(&format!("{REQ_OUT_PREFIX}{id}"))?.is_some() {
            count += 1;
            id = format!("out-{now_ms}-{count}");
        }
        let request = FriendRequestRecord {
            id,
            root_id: root_id.to_string(),
            nickname: nickname.to_string(),
            avatar: None,
            message: message.to_string(),
            source: source.to_string(),
            status: FriendRequestStatus::Pending,
            created_at: now_ms,
            updated_at: now_ms,
            peer,
        };
        write_json(
            storage,
            &format!("{REQ_OUT_PREFIX}{}", request.id),
            &request,
        )?;
        Ok(request)
    }

    /// 按 rootId 查找发出的申请；多条时返回键序最新的一条。
    pub fn find_outgoing_by_root<S: StorageBackend>(
        storage: &S,
        root_id: &str,
    ) -> Result<Option<FriendRequestRecord>> {
        let mut found = None;
        for (_, record) in scan_json::<S, FriendRequestRecord>(storage, REQ_OUT_PREFIX)? {
            if record.root_id == root_id {
                found = Some(record);
            }
        }
        Ok(found)
    }

    /// 对方确认后把发出的申请标记为 accepted；非 pending 或不存在返回 `Ok(false)`。
    pub fn mark_outgoing_accepted<S: StorageBackend>(
        storage: &mut S,
        id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let key = format!("{REQ_OUT_PREFIX}{id}");
        let Some(mut request): Option<FriendRequestRecord> = read_json(storage, &key)? else {
            return Ok(false);
        };
        if request.status != FriendRequestStatus::Pending {
            return Ok(false);
        }
        request.status = FriendRequestStatus::Accepted;
        request.updated_at = now_ms;
        write_json(storage, &key, &request)?;
        Ok(true)
    }
}
