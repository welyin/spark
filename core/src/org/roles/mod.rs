//! 组织角色解析（wiki `design/org-data-sync.md` §2/§4 的下一版；
//! 网关活跃集计分见 `docs/architecture/foundation/network.md` §4.2，
//! 全员数据节点见 `docs/architecture/community/membership.md` §4.1）。
//!
//! - **网关账号**：**不可指定**（A9：指定通路已移除，`gateways` 字段仅解析
//!   兼容存量、读取即忽略）——全体成员皆为候选，活跃集按计分推导限流
//!   （在线 > 最近活跃 > rotate_key 轮换 > rootId 字典序，叶子不履职，
//!   见 [`select_gateway_active`]）；
//! - **数据节点**：**数据账号角色已退役**（A14 / membership §4.1）——副本池 =
//!   全体成员账号，无指定无缺省角色（`dataAccounts` 字段仅解析兼容存量、
//!   读取即忽略）；K=3 集体口径 = 全体成员 PC 类设备副本合计 ≥ 3（叶子
//!   不计入，不达标只提醒）；数据面与治理面解耦（任免 admin 不牵动数据职责）；
//! - 角色绑定账号（rootId），账号的任一在线设备履职——设备不入成员表、不计副本。

use super::types::{OrganizationMember, OrganizationRecord};
use crate::storage::StorageBackend;

/// 网关活跃集上限（自荐限流；职责天然幂等，超发无害只是冗余）。
pub const GATEWAY_ACTIVE_LIMIT: usize = 3;

/// 网关活跃集轮换粒度：小时（确定性哈希轮换，避免活跃集每 tick 抖动）。
pub const GATEWAY_ACTIVE_ROTATE_MS: i64 = 3_600_000;

/// 网关履职候选计分输入（network §4.2 三因子）：由
/// [`gateway_candidate_scores`] 从存储装配；单测直接构造
/// （固定名册 + 固定在线视图 → 固定履职集）。
#[derive(Clone, Debug)]
pub struct GatewayCandidateScore {
    /// 成员 rootId。
    pub root_id: String,
    /// 当前在线（任一端点 peer_activity 有进行中会话）。
    pub online: bool,
    /// 最近活跃时刻（任一端点 last_seen_at 最大值；0 = 无记录）。
    pub last_active_ms: i64,
    /// 叶子（纯移动设备成员）：不履职（mobile-leaf-mode：不为网络/他人服务）。
    pub leaf: bool,
}

/// 履职集排序（纯函数；`rotate` 注入便于同分 tie-break 测试）：
/// 过滤叶子 → 在线优先 → 最近活跃降序 → rotate_key 升序（小时轮换）
/// → **rootId 字典序 tie-break**；取前 [`GATEWAY_ACTIVE_LIMIT`]。
/// 视图分歧如实接受（network §4.2）：各节点在线/活跃视图天然不一致，
/// 履职集可暂时分歧——org-mail「任一履职即收 + 按消息唯一标识去重」
/// 幂等容忍冗余，无需视图协商。
pub fn order_gateway_candidates(
    scores: &[GatewayCandidateScore],
    rotate: &dyn Fn(&str) -> u64,
) -> Vec<String> {
    let mut candidates: Vec<&GatewayCandidateScore> = scores.iter().filter(|s| !s.leaf).collect();
    candidates.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
            .then_with(|| rotate(&a.root_id).cmp(&rotate(&b.root_id)))
            .then_with(|| a.root_id.cmp(&b.root_id))
    });
    candidates
        .into_iter()
        .take(GATEWAY_ACTIVE_LIMIT)
        .map(|s| s.root_id.clone())
        .collect()
}

/// 履职集选择：rotate 取既有 `sha256(orgId|bucket|rootId)` 小时轮换键。
pub fn select_gateway_active(
    scores: &[GatewayCandidateScore],
    org_id: &str,
    now_ms: i64,
) -> Vec<String> {
    let bucket = (now_ms / GATEWAY_ACTIVE_ROTATE_MS) as u64;
    order_gateway_candidates(scores, &|rid| rotate_key(org_id, bucket, rid))
}

/// 从存储装配候选计分：在线/最近活跃取成员端点 peerId 的 peer_activity
/// 记录（`current_session_connected_at` 在 = 在线；last_seen_at 最大者 =
/// 最近活跃），设备类取 [`member_device_class`]（mobile = 叶子不履职）。
/// `self_root_id`（本机账号）恒按在线计——self 视图下自己必然在线，
/// peer_activity 只记对端不记本机。
pub fn gateway_candidate_scores<S: StorageBackend>(
    storage: &S,
    record: &OrganizationRecord,
    self_root_id: Option<&str>,
    _now_ms: i64,
) -> Vec<GatewayCandidateScore> {
    record
        .members
        .iter()
        .map(|m| {
            let is_self = self_root_id.is_some_and(|s| s == m.root_id);
            let mut online = is_self;
            let mut last_active_ms = 0i64;
            if let Some(set) = &m.node_info {
                for endpoint in set.iter() {
                    let Some(peer_id) = endpoint.peer_id.as_deref() else {
                        continue;
                    };
                    let Some(activity) =
                        crate::p2p::peer_activity::get_peer_activity(storage, peer_id)
                    else {
                        continue;
                    };
                    if activity.current_session_connected_at.is_some() {
                        online = true;
                    }
                    last_active_ms = last_active_ms.max(activity.last_seen_at);
                }
            }
            GatewayCandidateScore {
                root_id: m.root_id.clone(),
                online,
                last_active_ms,
                leaf: member_device_class(storage, m) == "mobile",
            }
        })
        .collect()
}

/// 网关活跃集（推导唯一通路，A9）：存储装配计分 → [`select_gateway_active`]。
pub fn gateway_active_set<S: StorageBackend>(
    storage: &S,
    record: &OrganizationRecord,
    self_root_id: Option<&str>,
    now_ms: i64,
) -> Vec<String> {
    let scores = gateway_candidate_scores(storage, record, self_root_id, now_ms);
    select_gateway_active(&scores, &record.org_id, now_ms)
}

/// 某账号是否活跃网关（承担地址发布/成员提示提供/邮箱暂存的职责）。
/// 成员判定先行；`root_id` 作 self 注入（自身恒在线）。
pub fn is_gateway_active<S: StorageBackend>(
    storage: &S,
    record: &OrganizationRecord,
    root_id: &str,
    now_ms: i64,
) -> bool {
    if record.find_member(root_id).is_none() {
        return false;
    }
    gateway_active_set(storage, record, Some(root_id), now_ms)
        .iter()
        .any(|rid| rid == root_id)
}

/// 确定性轮换键：sha256(orgId | bucket | rootId) 前 8 字节。
/// 全成员独立计算同一排序 → 活跃集全员一致，无需协调。
fn rotate_key(org_id: &str, bucket: u64, root_id: &str) -> u64 {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(format!("{org_id}|{bucket}|{root_id}").as_bytes());
    u64::from_be_bytes(digest[..8].try_into().expect("slice len 8"))
}

/// 数据节点集合（A14 / membership §4.1：**全员数据节点**）= 全体成员账号。
/// org scope 复制组推导（data-accounts 集合的 accounts 轴取值）的唯一来源。
pub fn data_node_set(record: &OrganizationRecord) -> Vec<String> {
    record
        .members
        .iter()
        .map(|m| m.root_id.clone())
        .collect()
}

/// 某账号是否为数据节点（A14：成员即数据节点）。
pub fn is_data_node(record: &OrganizationRecord, root_id: &str) -> bool {
    record.find_member(root_id).is_some()
}

/// 成员的设备类（K 记账的 PC 计入判定）：查 DeviceRecord。
/// 端点化：成员端点集任一端点为 PC 类设备即记 pc（账号至少 1 台 PC 达标）；
/// 无记录（离线成员、未同步设备数据）按 pc 计入——宁可多算不漏算
/// （漏算会触发不必要的补副本推送）。
pub fn member_device_class<S: crate::storage::StorageBackend>(
    storage: &S,
    member: &OrganizationMember,
) -> &'static str {
    let Some(set) = &member.node_info else {
        return "pc";
    };
    // F1：跟踪是否有端点带 peerId 进入查表——端点集存在但全部端点仅地址
    // （无 peerId）时无法查设备记录，视同"无记录"按 pc 计入（与无 nodeInfo
    // 的兜底一致，宁可多算不漏算）。
    let mut any_peer_lookup = false;
    for endpoint in set.iter() {
        let Some(peer_id) = endpoint.peer_id.as_deref() else {
            continue;
        };
        any_peer_lookup = true;
        let os = crate::device::DeviceService::get(storage, peer_id)
            .ok()
            .flatten()
            .map(|record| record.os);
        // DeviceRecord.os 是友好名（"Android"/"iOS"/"Windows"…），按小写前缀判定
        match os.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some(os) if os.starts_with("android") || os.starts_with("ios") => continue,
            _ => return "pc",
        }
    }
    if !any_peer_lookup {
        return "pc";
    }
    "mobile"
}

#[cfg(test)]
mod tests;
