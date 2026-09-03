//! 阶段四A P2（org-member-split §4）：per-member 条目的两个新写/擦路径。
//!
//! - [`OrganizationService::upsert_own_member_entry`]：成员自写条目（L2
//!   claim 退役的取代通道）——本机端点（nodeInfo）经 `org:member:{org}:{self}`
//!   条目随 orgsync 全员扩散，取代 nodeInfoClaim 捎带的管理员回填；
//! - [`wipe_org_local`]：本机被移出组织的本地擦除（L3 removed 通知入站与
//!   legacy pull Removed 分支共用）——whole + 成员条目（值与 pmeta）+ orgq
//!   现场一并清除。

use crate::storage::{ScanOptions, StorageBackend};

use super::super::types::{OrganizationNodeInfo, org_member_key, organization_key};
use super::{OrganizationService, Result};

impl OrganizationService {
    /// 成员自写条目（P2 L2）：把本机当前端点 upsert 进自己的
    /// `org:member:{orgId}:{self}` 条目。条目内其余字段以**装配视图**当前值
    /// 为基（条目为权威，whole 的 members 段不读）。
    ///
    /// **条目单写**（不动 whole org:meta）：端点刷新相对高频，whole 双写会
    /// 产生扩散噪音；混跑期旧端只读 whole、可能晚见到新端点——§6 flag-day
    /// 立场接受。版本化句柄上 put 经中间件本机 bump（本地写语义：这确实是
    /// 本机写）；raw 句柄裸写（测试/个人域路径）。
    ///
    /// 无成员资格 / 端点无变化 → `Ok(false)`（不写不 bump，幂等）。
    pub fn upsert_own_member_entry<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        current_root_id: &str,
        node_info: &OrganizationNodeInfo,
    ) -> Result<bool> {
        let Some(record) = Self::get_record(storage, org_id)? else {
            return Ok(false);
        };
        let Some(member) = record.find_member(current_root_id) else {
            return Ok(false);
        };
        let mut entry = member.clone();
        let set = entry.node_info.get_or_insert_with(Default::default);
        if !set.upsert(node_info) {
            return Ok(false); // 端点集无变化（upsert 幂等判定复用端点化语义）
        }
        storage.put(
            &org_member_key(org_id, current_root_id),
            &serde_json::to_string(&entry)?,
        )?;
        Ok(true)
    }
}

/// 本机被移出组织的本地擦除（P2 L3）：删 org:meta whole（句柄语义——版本化
/// 句柄=墓碑+pdsync 自设备传播，raw=裸删）+ 全部 `org:member:{orgId}:` 条目
/// （值与 pmeta 同删）+ orgq 现场（缓存/离线队列/在线目录，
/// `orgq_wipe_org_local` 既有语义）。幂等。返回擦除的条目数（观测用）。
///
/// 句柄差异说明：入站 `handle_org_member_removed` 持 raw 句柄（裸删无
/// 墓碑）；legacy pull Removed 分支持版本化句柄——条目删除会先产墓碑再
/// 被紧随的 pmeta 裸删覆盖（净效果同上），org/个人 dlog 会留条目（org 侧
/// 传播无意义：被移除者的信封过不了对端「from ∈ 成员表」前置；个人侧经
/// pdsync 驱动自设备条目擦除——正是所需）。自设备擦除的完备性挂账
/// P3/P4（设计 §4）。
pub fn wipe_org_local<S: StorageBackend>(storage: &mut S, org_id: &str) -> Result<usize> {
    storage.delete(&organization_key(org_id))?;
    let prefix = format!("{}{}:", super::super::types::ORG_MEMBER_PREFIX, org_id);
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(&prefix))?
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    let mut wiped = 0usize;
    for key in keys {
        storage.delete(&key)?;
        // pmeta 键不受管（不匹配任何受管前缀），版本化句柄上同形裸删
        storage.delete(&crate::sync::personal_meta_key(&key))?;
        wiped += 1;
    }
    crate::sync::orgsync::orgq_wipe_org_local(storage, org_id);
    Ok(wiped)
}
