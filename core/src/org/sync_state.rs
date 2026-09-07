//! org-sync-state 记账（对齐 org.md §8 / p2p-messages.md §11）。
//!
//! 存储键 `p2p:org-sync-state:<peerId>:<orgId>`，值 JSON：
//! `{ "versions": {summaryVersion,membersVersion,memberDetailsVersion,transactionsVersion},
//!    "lastSyncedAt": ms }`
//!
//! ## 有意修复（非逐 bug 对齐，p2p-messages.md §13.2）
//!
//! TS 在 org-share 推送路径（直连送达 / pubsub ack 两个写入时机）把
//! `{versions, sections, lastSyncedAt}` **外壳对象**当作 versions 写入
//! （org-share-sync.ts:439,464 传入的 `snapshot.sync` 在推送线形下是记录的整个
//! sync 状态而非四字段 versions），造成两个后果：
//!
//! 1. 推送前 stale 检查 `isOrganizationSyncStale(previousState.versions, snapshot.sync)`
//!    两侧四字段全为 undefined，比较恒 false → **存在历史 sync-state 后，
//!    对该 peer 的 org-share 推送恒被 "skip stale sync" 跳过**；
//! 2. K 副本统计 `coversCurrent` 对污染记录恒 true → 该成员永久计入
//!    everSynced（绕过 30 天窗口，org.md §12.3）。
//!
//! Rust 内核写入一律为规范 versions 形状；legacy org-share/org-pull 平面
//! 退役后，唯一写入点是 orgsync 平面的活动反哺（[`note_orgsync_activity`]）。
//! coversCurrent 按四字段真实比较。读取侧 [`OrgSyncState`]
//! 的反序列化对 TS 遗留污染形状做**兼容解包**（外壳里嵌套的 versions 才是
//! 有效数据），避免把 bug 传播回新实现。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::OrganizationSyncVersions;

/// org-sync-state 存储键前缀（p2p/constants.ts:146）。
pub const ORG_SYNC_STATE_PREFIX: &str = "p2p:org-sync-state:";

/// org-sync-state 保留期：90 天（data-management/constants.ts:17）。
pub const ORG_SYNC_STATE_MAX_AGE_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// 存储键（节点口径，O1 前）：`p2p:org-sync-state:<peerId>:<orgId>`。
/// 新写入一律走账号口径 [`org_sync_state_account_key`]；本函数保留给
/// 旧键迁移读取（[`migrate_org_sync_state_to_account`]）。
pub fn org_sync_state_key(peer_id: &str, org_id: &str) -> String {
    format!("{ORG_SYNC_STATE_PREFIX}{peer_id}:{org_id}")
}

/// 存储键（账号口径，O1）：`p2p:org-sync-state:acct:<rootId>:<orgId>`。
/// 同一账号多设备（peerId 漂移）共享一份记账——"账号持有数据"与
/// "哪台设备当时在线"解耦。
pub fn org_sync_state_account_key(root_id: &str, org_id: &str) -> String {
    format!("{ORG_SYNC_STATE_PREFIX}acct:{root_id}:{org_id}")
}

/// 账号口径读取（O1 迁移）：新键优先；缺失且提供了 legacy peerId 时读旧键
/// 并回填新键（旧键保留，90 天过期清理自然回收）。
pub fn read_org_sync_state_account<S: crate::storage::StorageBackend>(
    storage: &mut S,
    root_id: &str,
    org_id: &str,
    legacy_peer_id: Option<&str>,
) -> Option<OrgSyncState> {
    let key = org_sync_state_account_key(root_id, org_id);
    if let Ok(Some(raw)) = storage.get(&key)
        && let Some(state) = OrgSyncState::from_json(&raw)
    {
        return Some(state);
    }
    let peer_id = legacy_peer_id?;
    let legacy = storage
        .get(&org_sync_state_key(peer_id, org_id))
        .ok()
        .flatten()
        .and_then(|raw| OrgSyncState::from_json(&raw))?;
    let _ = storage.put(&key, &legacy.to_json());
    Some(legacy)
}

/// org-sync-state 记录（规范形状）。
///
/// 反序列化对 TS 污染形状宽容：`versions` 若为 `{versions, sections, lastSyncedAt}`
/// 外壳，自动解包取内层四字段（污染形状下外壳自身没有四字段，TS 的
/// coversCurrent 恒 true 正是由此而来——见模块文档"有意修复"）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OrgSyncState {
    /// 同步完成时该 peer 持有的组织版本（规范四字段）。
    pub versions: OrganizationSyncVersions,
    /// 记账时间（ms）。
    #[serde(rename = "lastSyncedAt")]
    pub last_synced_at: i64,
}

impl<'de> Deserialize<'de> for OrgSyncState {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            versions: Value,
            #[serde(rename = "lastSyncedAt")]
            last_synced_at: i64,
        }
        let wire = Wire::deserialize(deserializer)?;
        // 优先按规范四字段解析；失败则尝试污染形状的解包（外壳.versions）
        let versions = serde_json::from_value::<OrganizationSyncVersions>(wire.versions.clone())
            .or_else(|_| {
                use serde::ser::Error as _;
                let inner = wire
                    .versions
                    .get("versions")
                    .cloned()
                    .ok_or_else(|| serde_json::Error::custom("missing versions"))?;
                serde_json::from_value::<OrganizationSyncVersions>(inner)
            })
            .map_err(|e| serde::de::Error::custom(e.to_string()))?;
        Ok(OrgSyncState {
            versions,
            last_synced_at: wire.last_synced_at,
        })
    }
}

impl OrgSyncState {
    /// 从存储 JSON 解析；缺失/损坏时返回 `None`（对齐 TS `getOrgSyncState`）。
    pub fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }

    /// 序列化为存储 JSON（规范形状）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("OrgSyncState is always serializable")
    }
}

/// 90 天清理判定（data-management cleanup.ts:80-92）：
/// `now - lastSyncedAt > 90天` 时删除。
pub fn is_org_sync_state_expired(last_synced_at: i64, now_ms: i64) -> bool {
    now_ms - last_synced_at > ORG_SYNC_STATE_MAX_AGE_MS
}

/// orgsync 平面活动反哺记账（replica.rs「overview 记账切 orgsync 平面」
/// TODO 落地，卫生批）：与 `from` 成员的一次验签成功的 orgsync-hello/data
/// 交换后，按账号口径写 sync-state（versions = 本机当前组织版本，
/// lastSyncedAt = 现在）。
///
/// 语义：orgsync 反熵平面里「交换过 hello/data」即互为副本（内建
/// all-members 集合全员全量，hello 摘要即覆盖证明）；`p2p:` 前缀键不进
/// 同步流量，幂等覆盖（交换频繁时只是刷新时间戳）。
pub fn note_orgsync_activity<S: crate::storage::StorageBackend>(
    storage: &mut S,
    record: &super::types::OrganizationRecord,
    from_root_id: &str,
    now_ms: i64,
) {
    let versions = super::snapshot::resolve_local_versions(record);
    let state = OrgSyncState {
        versions,
        last_synced_at: now_ms,
    };
    let _ = storage.put(
        &org_sync_state_account_key(from_root_id, &record.org_id),
        &state.to_json(),
    );
}
