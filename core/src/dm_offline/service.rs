//! dm_offline 服务：统一密文暂存补投（social-feed §6，纯逻辑层）。
//!
//! 对齐 social-feed §6 与 p2p-dm §19.3（§8.4 网关层落地）。本模块只负责
//! 存储语义（键构造 / CRUD / 容量 TTL），**不碰网络**——补投的拨号与投递由
//! kernel 编排层（`on_peer_connected` flush 钩子 + 60s 周期 flush）调用本模块
//! 的读取/删除后，用 p2p 层 `dm_direct` 实际重发。
//!
//! ## 空间与同步
//!
//! - 个人空间（`dm:pending:`）：入队经 `put_personal` 携带 pmeta（pdsync
//!   `dm:pending` category），同一 rootId 的多台设备互为补投备份——任一台
//!   在线设备上线都 flush 补投；
//! - 组织空间（`org:dm:pending:{orgId}:`）：按个人空间同构落地（键带 orgId），
//!   但**org-sync 网关通道尚未就绪**——本模块先提供 org 键的存储语义，org
//!   记录暂不经 org-sync 扩散（差距：见交还报告）。待 org-sync 网关代收落地后
//!   接入。
//!
//! ## 容量 / TTL
//!
//! TTL 7 天（`PENDING_TTL_MS`）、单 recipient 上限 `PER_RECIPIENT_PENDING_CAP`
//! 条、全局上限 `GLOBAL_PENDING_CAP` 条，超出淘汰最旧（按 `createdAt` 升序）。

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::put_personal;

use super::types::{
    GLOBAL_PENDING_CAP, ORG_PENDING_PREFIX, PENDING_PREFIX, PER_RECIPIENT_PENDING_CAP,
    PENDING_TTL_MS, PendingRecord,
};

/// 空间：个人（pdsync 自设备扩散）或某个组织（org-sync 待接入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingSpace<'a> {
    Personal,
    Org(&'a str),
}

/// dm_offline 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum DmOfflineError {
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// 同步写入（put_personal）错误。
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),
}

/// dm_offline 模块 Result 别名。
pub type Result<T> = std::result::Result<T, DmOfflineError>;

/// pending 记录存储键。
///
/// - 个人：`dm:pending:{toRootId}:{messageId}`
/// - 组织：`org:dm:pending:{orgId}:{toRootId}:{messageId}`
pub fn pending_key(space: PendingSpace, to_root_id: &str, message_id: &str) -> String {
    match space {
        PendingSpace::Personal => format!("{PENDING_PREFIX}{to_root_id}:{message_id}"),
        PendingSpace::Org(org_id) => format!("{ORG_PENDING_PREFIX}{org_id}:{to_root_id}:{message_id}"),
    }
}

/// 空间级 pending 记录前缀（全量扫描用）。
fn pending_prefix(space: PendingSpace) -> String {
    match space {
        PendingSpace::Personal => PENDING_PREFIX.to_string(),
        PendingSpace::Org(org_id) => format!("{ORG_PENDING_PREFIX}{org_id}:"),
    }
}

/// 某 recipient 的 pending 记录前缀（`dm:pending:{to}:` 级）。
fn recipient_prefix(space: PendingSpace, to_root_id: &str) -> String {
    match space {
        PendingSpace::Personal => format!("{PENDING_PREFIX}{to_root_id}:"),
        PendingSpace::Org(org_id) => {
            format!("{ORG_PENDING_PREFIX}{org_id}:{to_root_id}:")
        }
    }
}

/// 读取空间前缀下全部 pending 记录（键 + 记录；损坏记录跳过）。
fn scan_pending<S: StorageBackend>(
    storage: &S,
    space: PendingSpace,
) -> Result<Vec<(String, PendingRecord)>> {
    let prefix = pending_prefix(space);
    let mut out = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(prefix))? {
        if let Ok(record) = serde_json::from_str::<PendingRecord>(&raw) {
            out.push((key, record));
        }
    }
    Ok(out)
}

/// 入队一条密文消息（幂等：同 (to, messageId) 覆盖）。
///
/// - 个人空间经 `put_personal`（pdsync `dm:pending` category）扩散到自设备；
/// - 组织空间直接 `storage.put`（org-sync 待接入）；
/// - 入队后统一收敛：清过期 + 强制单 recipient/全局容量。
pub fn enqueue<S: StorageBackend>(
    storage: &mut S,
    space: PendingSpace,
    to_root_id: &str,
    record: &PendingRecord,
    node_id: &str,
    now_ms: i64,
) -> Result<()> {
    let key = pending_key(space, to_root_id, &record.message_id);
    let value = serde_json::to_string(record)?;
    match space {
        PendingSpace::Personal => {
            put_personal(storage, node_id, &key, &value, now_ms)?;
        }
        PendingSpace::Org(_) => {
            storage.put(&key, &value)?;
        }
    }
    prune_expired(storage, space, now_ms)?;
    enforce_caps(storage, space)?;
    Ok(())
}

/// 列出某 recipient 的全部待补投记录（键 + 记录；过期记录已剔除，供编排层
/// 直接据此补投）。补投成功由编排层调用 [`remove`] 删除。
pub fn list_for_recipient<S: StorageBackend>(
    storage: &S,
    space: PendingSpace,
    to_root_id: &str,
    now_ms: i64,
) -> Result<Vec<(String, PendingRecord)>> {
    let prefix = recipient_prefix(space, to_root_id);
    let mut out = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(prefix))? {
        let Ok(record) = serde_json::from_str::<PendingRecord>(&raw) else {
            continue;
        };
        if now_ms.saturating_sub(record.created_at) > PENDING_TTL_MS {
            continue;
        }
        out.push((key, record));
    }
    Ok(out)
}

/// 补投成功后删除一条 pending 记录（幂等；不存在不报错）。
pub fn remove<S: StorageBackend>(storage: &mut S, key: &str) -> Result<()> {
    storage.delete(key)?;
    Ok(())
}

/// 清理过期记录（`createdAt < now - TTL`）。
pub fn prune_expired<S: StorageBackend>(
    storage: &mut S,
    space: PendingSpace,
    now_ms: i64,
) -> Result<()> {
    let expired: Vec<String> = scan_pending(storage, space)?
        .into_iter()
        .filter(|(_, r)| now_ms.saturating_sub(r.created_at) > PENDING_TTL_MS)
        .map(|(key, _)| key)
        .collect();
    for key in expired {
        storage.delete(&key)?;
    }
    Ok(())
}

/// 强制容量：先全局上限（超 `GLOBAL_PENDING_CAP` 淘汰最旧），再单 recipient
/// 上限（超 `PER_RECIPIENT_PENDING_CAP` 淘汰该 recipient 最旧）。
fn enforce_caps<S: StorageBackend>(storage: &mut S, space: PendingSpace) -> Result<()> {
    let mut items = scan_pending(storage, space)?;
    // 旧在前（淘汰最旧优先删最早的）
    items.sort_by(|a, b| {
        a.1.created_at
            .cmp(&b.1.created_at)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut to_delete: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1) 全局 cap
    let mut remaining = items.len();
    for (key, _) in &items {
        if remaining <= GLOBAL_PENDING_CAP {
            break;
        }
        to_delete.insert(key.clone());
        remaining -= 1;
    }

    // 2) 单 recipient cap（统计各 recipient 未删条数，最旧优先删）
    let mut per_recip: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (key, r) in &items {
        if !to_delete.contains(key) {
            *per_recip.entry(r.to.clone()).or_insert(0) += 1;
        }
    }
    for (key, r) in &items {
        if to_delete.contains(key) {
            continue;
        }
        let count = per_recip.get_mut(&r.to).expect("counted above");
        if *count > PER_RECIPIENT_PENDING_CAP {
            to_delete.insert(key.clone());
            *count -= 1;
        }
    }

    for key in to_delete {
        storage.delete(&key)?;
    }
    Ok(())
}
