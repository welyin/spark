//! 组织角色解析（账号角色模型，wiki `design/org-data-sync.md` §2/§4）。
//!
//! **默认推导 + 可选显式指定**：
//!
//! - **网关账号**：`gateways` 显式指定（保留旧字段，空 = 未指定）→ 指定者活跃；
//!   未指定时缺省**全体成员皆为候选**，活跃集自荐限流（确定性轮换，见
//!   [`gateway_active_set`]）；
//! - **数据账号**：`data_accounts` 显式指定（新保留键）→ 指定者担责；未指定时
//!   缺省**全体管理员**；
//! - 角色绑定账号（rootId），账号的任一在线设备履职——设备不入成员表、不计副本。

use super::types::{OrganizationMember, OrganizationRecord, OrganizationRole};

/// 网关活跃集上限（自荐限流；职责天然幂等，超发无害只是冗余）。
pub const GATEWAY_ACTIVE_LIMIT: usize = 3;

/// 网关活跃集轮换粒度：小时（确定性哈希轮换，避免活跃集每 tick 抖动）。
pub const GATEWAY_ACTIVE_ROTATE_MS: i64 = 3_600_000;

/// 某账号是否活跃网关（承担地址发布/成员提示提供的职责）。
///
/// - 显式指定（`gateways` 非空）：在列表即活跃；
/// - 缺省（未指定）：全体成员候选，确定性轮换取前 [`GATEWAY_ACTIVE_LIMIT`] 个。
pub fn is_gateway_active(
    record: &OrganizationRecord,
    root_id: &str,
    now_ms: i64,
) -> bool {
    if record.find_member(root_id).is_none() {
        return false;
    }
    if !record.gateways.is_empty() {
        return record.is_gateway(root_id);
    }
    let mut candidates: Vec<&str> = record
        .members
        .iter()
        .map(|m| m.root_id.as_str())
        .collect();
    candidates.sort_unstable();
    let bucket = (now_ms / GATEWAY_ACTIVE_ROTATE_MS) as u64;
    candidates.sort_by_key(|rid| rotate_key(&record.org_id, bucket, rid));
    candidates
        .iter()
        .take(GATEWAY_ACTIVE_LIMIT)
        .any(|rid| *rid == root_id)
}

/// 缺省网关活跃集（UI 呈现"当前承担网关职责的成员"用）。
pub fn gateway_active_set(record: &OrganizationRecord, now_ms: i64) -> Vec<String> {
    if !record.gateways.is_empty() {
        return record
            .gateways
            .iter()
            .filter(|g| record.find_member(g).is_some())
            .cloned()
            .collect();
    }
    let mut candidates: Vec<String> = record
        .members
        .iter()
        .map(|m| m.root_id.clone())
        .collect();
    candidates.sort_unstable();
    let bucket = (now_ms / GATEWAY_ACTIVE_ROTATE_MS) as u64;
    candidates.sort_by_key(|rid| rotate_key(&record.org_id, bucket, rid));
    candidates.truncate(GATEWAY_ACTIVE_LIMIT);
    candidates
}

/// 确定性轮换键：sha256(orgId | bucket | rootId) 前 8 字节。
/// 全成员独立计算同一排序 → 活跃集全员一致，无需协调。
fn rotate_key(org_id: &str, bucket: u64, root_id: &str) -> u64 {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(format!("{org_id}|{bucket}|{root_id}").as_bytes());
    u64::from_be_bytes(digest[..8].try_into().expect("slice len 8"))
}

/// 数据账号集合（解析后）：显式 `data_accounts` 非空 → 指定者（过滤非成员）；
/// 否则全体管理员。
pub fn data_account_set(record: &OrganizationRecord) -> Vec<String> {
    if !record.data_accounts.is_empty() {
        return record
            .data_accounts
            .iter()
            .filter(|rid| record.find_member(rid).is_some())
            .cloned()
            .collect();
    }
    record
        .members
        .iter()
        .filter(|m| m.role == OrganizationRole::Admin)
        .map(|m| m.root_id.clone())
        .collect()
}

/// 某账号是否为数据账号（解析后，含缺省推导）。
pub fn is_data_account(record: &OrganizationRecord, root_id: &str) -> bool {
    if record.find_member(root_id).is_none() {
        return false;
    }
    if !record.data_accounts.is_empty() {
        return record.data_accounts.iter().any(|rid| rid == root_id);
    }
    record.is_admin(root_id)
}

/// 数据账号是否经显式指定（区分"缺省=全体管理员"与显式收窄——晋升提示与
/// 降级清理逻辑用）。
pub fn has_explicit_data_accounts(record: &OrganizationRecord) -> bool {
    !record.data_accounts.is_empty()
}

/// 成员的设备类（K 记账的 PC 计入判定）：查 DeviceRecord。
/// 无记录（离线成员、未同步设备数据）按 pc 计入——宁可多算不漏算
/// （漏算会触发不必要的补副本推送）。
pub fn member_device_class<S: crate::storage::StorageBackend>(
    storage: &S,
    member: &OrganizationMember,
) -> &'static str {
    let class = member
        .node_info
        .as_ref()
        .and_then(|n| n.peer_id.as_deref())
        .and_then(|peer_id| {
            crate::device::DeviceService::get(storage, peer_id).ok().flatten()
        })
        .map(|record| record.os);
    // DeviceRecord.os 是友好名（"Android"/"iOS"/"Windows"…），按小写前缀判定
    match class.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some(os) if os.starts_with("android") || os.starts_with("ios") => "mobile",
        _ => "pc",
    }
}

#[cfg(test)]
mod tests;
