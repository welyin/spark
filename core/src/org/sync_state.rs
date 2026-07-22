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
//! Rust 内核三个写入时机**一律写规范 versions 形状**（见下方三个构造函数），
//! stale / coversCurrent 均按四字段真实比较。读取侧 [`OrgSyncState`]
//! 的反序列化对 TS 遗留污染形状做**兼容解包**（外壳里嵌套的 versions 才是
//! 有效数据），避免把 bug 传播回新实现。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::snapshot::is_organization_sync_stale;
use super::types::OrganizationSyncVersions;

/// org-sync-state 存储键前缀（p2p/constants.ts:146）。
pub const ORG_SYNC_STATE_PREFIX: &str = "p2p:org-sync-state:";

/// org-sync-state 保留期：90 天（data-management/constants.ts:17）。
pub const ORG_SYNC_STATE_MAX_AGE_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// 存储键：`p2p:org-sync-state:<peerId>:<orgId>`。
pub fn org_sync_state_key(peer_id: &str, org_id: &str) -> String {
    format!("{ORG_SYNC_STATE_PREFIX}{peer_id}:{org_id}")
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

/// 写入时机 1：org-share **直连送达**确认后（org-share-sync.ts:439）。
pub fn sync_state_after_share_delivered(
    versions: OrganizationSyncVersions,
    now_ms: i64,
) -> OrgSyncState {
    OrgSyncState {
        versions,
        last_synced_at: now_ms,
    }
}

/// 写入时机 2：org-share **pubsub 收到 ack** 后（org-share-sync.ts:464）。
pub fn sync_state_after_share_acked(
    versions: OrganizationSyncVersions,
    now_ms: i64,
) -> OrgSyncState {
    OrgSyncState {
        versions,
        last_synced_at: now_ms,
    }
}

/// 写入时机 3：org-pull **成功拉取**某组织后（org-pull-sync.ts:279-296，
/// 经 `onSyncState` 回调；versions 取本地记录的 `sync.versions`）。
pub fn sync_state_after_pull_synced(
    versions: OrganizationSyncVersions,
    now_ms: i64,
) -> OrgSyncState {
    OrgSyncState {
        versions,
        last_synced_at: now_ms,
    }
}

/// 推送前跳过判定（org-share-sync.ts:394-404 的正确语义版）：
/// 存在历史 sync-state 且该 peer 记录的版本**不落后**于待推送版本时跳过。
///
/// 有意修复：TS 因污染形状使该判定恒为 true（首次送达后永不推送）；
/// Rust 按四字段真实比较——对端已覆盖当前版本才跳过。
pub fn should_skip_share_push(
    previous_state: Option<&OrgSyncState>,
    current_versions: &OrganizationSyncVersions,
) -> bool {
    match previous_state {
        Some(state) => !is_organization_sync_stale(Some(&state.versions), current_versions),
        None => false,
    }
}

/// 90 天清理判定（data-management cleanup.ts:80-92）：
/// `now - lastSyncedAt > 90天` 时删除。
pub fn is_org_sync_state_expired(last_synced_at: i64, now_ms: i64) -> bool {
    now_ms - last_synced_at > ORG_SYNC_STATE_MAX_AGE_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(v: i64) -> OrganizationSyncVersions {
        OrganizationSyncVersions {
            summary_version: v,
            members_version: v,
            member_details_version: v,
            transactions_version: v,
        }
    }

    #[test]
    fn key_format() {
        assert_eq!(
            org_sync_state_key("peer1", "org_abc"),
            "p2p:org-sync-state:peer1:org_abc"
        );
    }

    #[test]
    fn canonical_shape_roundtrip() {
        let state = sync_state_after_share_delivered(versions(100), 5000);
        let json = state.to_json();
        assert_eq!(
            json,
            "{\"versions\":{\"summaryVersion\":100,\"membersVersion\":100,\"memberDetailsVersion\":100,\"transactionsVersion\":100},\"lastSyncedAt\":5000}"
        );
        let parsed = OrgSyncState::from_json(&json).unwrap();
        assert_eq!(parsed, state);
        assert!(OrgSyncState::from_json("{bad json").is_none());
    }

    #[test]
    fn polluted_legacy_shape_is_unwrapped() {
        // TS share 路径写入的污染形状：versions = {versions, sections, lastSyncedAt} 外壳
        let polluted = r#"{"versions":{"versions":{"summaryVersion":100,"membersVersion":100,"memberDetailsVersion":100,"transactionsVersion":99},"sections":["summary","members","member-details","transactions"],"lastSyncedAt":4000},"lastSyncedAt":5000}"#;
        let state = OrgSyncState::from_json(polluted).unwrap();
        assert_eq!(state.versions.summary_version, 100);
        assert_eq!(state.versions.transactions_version, 99);
        assert_eq!(state.last_synced_at, 5000);
        // 重新序列化回规范形状（修复随写传播）
        let canonical = state.to_json();
        assert!(canonical.contains("\"summaryVersion\":100"));
        assert!(!canonical.contains("sections"));
    }

    #[test]
    fn skip_push_decision_correct_semantics() {
        // 无历史 → 不跳过
        assert!(!should_skip_share_push(None, &versions(100)));
        // 对端版本覆盖当前 → 跳过
        let state = sync_state_after_pull_synced(versions(100), 1);
        assert!(should_skip_share_push(Some(&state), &versions(100)));
        assert!(should_skip_share_push(Some(&state), &versions(99)));
        // 对端落后（任一字段） → 不跳过——TS 污染形状下该分支永不可达
        assert!(!should_skip_share_push(Some(&state), &versions(101)));
        let mut newer = versions(100);
        newer.transactions_version = 101;
        assert!(!should_skip_share_push(Some(&state), &newer));
    }

    #[test]
    fn expiry_decision() {
        let now = 100 * 24 * 60 * 60 * 1000i64;
        assert!(!is_org_sync_state_expired(now - ORG_SYNC_STATE_MAX_AGE_MS, now));
        assert!(is_org_sync_state_expired(now - ORG_SYNC_STATE_MAX_AGE_MS - 1, now));
        assert!(!is_org_sync_state_expired(now, now));
    }

    #[test]
    fn three_write_timings_share_shape() {
        let a = sync_state_after_share_delivered(versions(1), 10);
        let b = sync_state_after_share_acked(versions(1), 10);
        let c = sync_state_after_pull_synced(versions(1), 10);
        assert_eq!(a, b);
        assert_eq!(b, c);
        // 三者都必须是规范四字段（TS 仅时机 3 是规范形状）
        let json = a.to_json();
        assert!(json.starts_with("{\"versions\":{\"summaryVersion\":1,"));
    }
}
