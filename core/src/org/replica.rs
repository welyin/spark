//! K 副本统计（对齐 org.md §12 / org-share-sync.ts:107-176 `getOrgSyncOverview`）。
//!
//! 计入副本的判定（本机恒算一个）：**30 天窗口内同步过** 或 **sync-state 版本
//! 仍覆盖当前组织版本**，二选一（constants.ts:154-161 的设计注释：不能只用版本
//! 比较——每次编辑会瞬间翻转；也不能只用 TTL——静默组织不会刷新 sync-state）。
//!
//! ## 有意修复（org.md §12.3）
//!
//! TS 中 share 路径写入的污染形状 sync-state 使 `coversCurrent` 恒 true（成员
//! 永久计入 everSynced，绕过 30 天窗口）。Rust 的 [`covers_current`] 按四字段
//! 真实比较：sync-state 版本不落后于当前版本才成立。配合
//! [`crate::org::sync_state`] 的规范写入，统计语义恢复设计本意。

use super::snapshot::is_organization_sync_stale;
use super::sync_state::OrgSyncState;
use super::types::{OrganizationMember, OrganizationSyncVersions};

/// 副本目标 K（含本机，p2p/constants.ts:152）。
pub const ORG_REPLICA_TARGET: u32 = 3;

/// 副本新鲜窗口：30 天（p2p/constants.ts:161）。
pub const ORG_REPLICA_FRESH_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// 单成员副本状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSyncOverview {
    /// 成员 rootId。
    pub root_id: String,
    /// 成员 peerId（trim 后；无 nodeInfo 时为 `None`）。
    pub peer_id: Option<String>,
    /// 是否本机当前用户。
    pub is_self: bool,
    /// 是否计入副本（本机恒 true）。
    pub ever_synced: bool,
    /// 该成员 peer 的最近同步时间（无记录为 `None`）。
    pub last_synced_at: Option<i64>,
}

/// 组织副本概览。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgSyncOverview {
    /// 组织 id。
    pub org_id: String,
    /// 副本目标 K。
    pub replica_target: u32,
    /// 已持有副本的成员数（含本机）。
    pub synced_peers: u32,
    /// 有效成员数（rootId 非法的成员被跳过，不计入）。
    pub total_members: u32,
    /// 逐成员状态（按记录 members 顺序）。
    pub members: Vec<MemberSyncOverview>,
}

impl OrgSyncOverview {
    /// 副本数是否达标（`syncedPeers >= K`）。
    pub fn is_replica_sufficient(&self) -> bool {
        replica_sufficient(self.synced_peers)
    }
}

/// 副本达标判定。
pub fn replica_sufficient(synced_peers: u32) -> bool {
    synced_peers >= ORG_REPLICA_TARGET
}

/// `coversCurrent`：sync-state 版本仍覆盖当前组织版本（不落后）。
///
/// 正确语义（有意修复）：`!isStale(state.versions, currentVersions)`，
/// 四字段真实比较；currentVersions 缺失时不成立。
pub fn covers_current(
    state: &OrgSyncState,
    current_versions: Option<&OrganizationSyncVersions>,
) -> bool {
    match current_versions {
        Some(current) => !is_organization_sync_stale(Some(&state.versions), current),
        None => false,
    }
}

/// 单成员 `everSynced` 判定：`isSelf || recentlySynced || coversCurrent`。
pub fn member_ever_synced(
    is_self: bool,
    state: Option<&OrgSyncState>,
    current_versions: Option<&OrganizationSyncVersions>,
    now_ms: i64,
) -> bool {
    if is_self {
        return true;
    }
    let Some(state) = state else {
        return false;
    };
    let recently_synced = now_ms - state.last_synced_at <= ORG_REPLICA_FRESH_WINDOW_MS;
    recently_synced || covers_current(state, current_versions)
}

/// `getOrgSyncOverview` 纯函数版：对组织每个成员（按记录顺序）判定副本状态。
///
/// - `current_root_id`：本机当前用户（判定 isSelf；未登录为 `None`）
/// - `current_versions`：当前组织版本（`record.sync.versions`，缺失时调用方
///   应以 `build_organization_sync_versions_default(record)` 兜底）
/// - `state_lookup`：按 peerId 查 org-sync-state（成员无 peerId 时不查询）
/// - rootId 为空串的成员跳过（对齐 TS 的 `if (!rootId) continue`）
pub fn compute_org_sync_overview(
    org_id: &str,
    members: &[OrganizationMember],
    current_root_id: Option<&str>,
    current_versions: Option<&OrganizationSyncVersions>,
    mut state_lookup: impl FnMut(&str) -> Option<OrgSyncState>,
    now_ms: i64,
) -> OrgSyncOverview {
    let mut overview_members = Vec::new();
    let mut synced_peers = 0u32;

    for member in members {
        if member.root_id.is_empty() {
            continue;
        }
        let peer_id = member
            .node_info
            .as_ref()
            .and_then(|n| n.peer_id.as_deref())
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let is_self = current_root_id == Some(member.root_id.as_str());
        let state = peer_id.as_deref().and_then(&mut state_lookup);
        let ever_synced =
            member_ever_synced(is_self, state.as_ref(), current_versions, now_ms);
        if ever_synced {
            synced_peers += 1;
        }
        overview_members.push(MemberSyncOverview {
            root_id: member.root_id.clone(),
            peer_id,
            is_self,
            ever_synced,
            last_synced_at: state.map(|s| s.last_synced_at),
        });
    }

    OrgSyncOverview {
        org_id: org_id.to_string(),
        replica_target: ORG_REPLICA_TARGET,
        synced_peers,
        total_members: overview_members.len() as u32,
        members: overview_members,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationNodeInfo, OrganizationRole};

    const NOW: i64 = 100_000_000_000;

    fn rid(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn versions(v: i64) -> OrganizationSyncVersions {
        OrganizationSyncVersions {
            summary_version: v,
            members_version: v,
            member_details_version: v,
            transactions_version: v,
        }
    }

    fn member(root: char, peer_id: Option<&str>) -> OrganizationMember {
        OrganizationMember {
            root_id: rid(root),
            role: OrganizationRole::Member,
            joined_at: 1000,
            added_by: rid('z'),
            node_info: peer_id.map(|p| OrganizationNodeInfo {
                peer_id: Some(p.to_string()),
                addresses: vec![],
            }),
            extra: Default::default(),
        }
    }

    fn no_state() -> impl FnMut(&str) -> Option<OrgSyncState> {
        |_| None
    }

    #[test]
    fn self_always_counts() {
        let members = vec![member('a', None)];
        let overview =
            compute_org_sync_overview("org_x", &members, Some(&rid('a')), None, no_state(), NOW);
        assert_eq!(overview.synced_peers, 1);
        assert!(overview.members[0].is_self);
        assert!(overview.members[0].ever_synced);
        assert_eq!(overview.members[0].last_synced_at, None);
    }

    #[test]
    fn recently_synced_within_30d_window() {
        let members = vec![member('b', Some("peer-b"))];
        let state_at = |ts: i64| OrgSyncState {
            versions: versions(50), // 落后于 current(100)：coversCurrent=false
            last_synced_at: ts,
        };
        let current = versions(100);
        // 窗口内（恰好 30 天边界） → 计入
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            None,
            Some(&current),
            |_| Some(state_at(NOW - ORG_REPLICA_FRESH_WINDOW_MS)),
            NOW,
        );
        assert!(overview.members[0].ever_synced, "30 天边界仍计入");
        // 超出窗口 1ms 且版本落后 → 不计入
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            None,
            Some(&current),
            |_| Some(state_at(NOW - ORG_REPLICA_FRESH_WINDOW_MS - 1)),
            NOW,
        );
        assert!(!overview.members[0].ever_synced);
        assert_eq!(overview.synced_peers, 0);
    }

    #[test]
    fn covers_current_counts_stale_ttl_but_fresh_versions() {
        // 静默组织：lastSyncedAt 远超 30 天，但版本仍覆盖当前 → 计入
        let members = vec![member('b', Some("peer-b"))];
        let current = versions(100);
        let stale_ttl = OrgSyncState {
            versions: versions(100),
            last_synced_at: NOW - 365 * 24 * 60 * 60 * 1000,
        };
        assert!(covers_current(&stale_ttl, Some(&current)));
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            None,
            Some(&current),
            |_| Some(stale_ttl),
            NOW,
        );
        assert!(overview.members[0].ever_synced);
        assert_eq!(overview.synced_peers, 1);

        // 版本落后（任一字段） → coversCurrent=false（TS 污染形状下恒 true，有意修复）
        let lagging = OrgSyncState {
            versions: OrganizationSyncVersions {
                transactions_version: 99,
                ..versions(100)
            },
            ..stale_ttl
        };
        assert!(!covers_current(&lagging, Some(&current)));
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            None,
            Some(&current),
            |_| Some(lagging),
            NOW,
        );
        assert!(!overview.members[0].ever_synced);

        // currentVersions 缺失 → coversCurrent=false
        assert!(!covers_current(&stale_ttl, None));
    }

    #[test]
    fn member_without_peer_has_no_state() {
        let members = vec![member('b', None), member('c', Some("  "))];
        let current = versions(100);
        let overview =
            compute_org_sync_overview("org_x", &members, None, Some(&current), no_state(), NOW);
        assert_eq!(overview.synced_peers, 0);
        assert_eq!(overview.total_members, 2);
        assert_eq!(overview.members[0].peer_id, None);
        assert_eq!(overview.members[1].peer_id, None);
    }

    #[test]
    fn empty_root_id_members_skipped() {
        let mut blank = member('b', Some("peer-b"));
        blank.root_id = String::new();
        let members = vec![blank, member('c', None)];
        let overview = compute_org_sync_overview("org_x", &members, None, None, no_state(), NOW);
        assert_eq!(overview.total_members, 1);
    }

    #[test]
    fn peer_id_is_trimmed_for_state_lookup() {
        let members = vec![member('b', Some("  peer-b  "))];
        let state = OrgSyncState {
            versions: versions(100),
            last_synced_at: NOW,
        };
        let current = versions(100);
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            None,
            Some(&current),
            |peer| {
                assert_eq!(peer, "peer-b", "lookup 必须用 trim 后的 peerId");
                Some(state)
            },
            NOW,
        );
        assert_eq!(overview.members[0].peer_id.as_deref(), Some("peer-b"));
        assert!(overview.members[0].ever_synced);
    }

    #[test]
    fn replica_sufficiency() {
        assert!(!replica_sufficient(0));
        assert!(!replica_sufficient(2));
        assert!(replica_sufficient(3));
        assert!(replica_sufficient(4));

        // 全链路：本机 + 两个窗口内同步过的成员 → 达标
        let members = vec![
            member('a', Some("peer-a")),
            member('b', Some("peer-b")),
            member('c', Some("peer-c")),
        ];
        let current = versions(100);
        let fresh = OrgSyncState {
            versions: versions(1),
            last_synced_at: NOW,
        };
        let overview = compute_org_sync_overview(
            "org_x",
            &members,
            Some(&rid('a')),
            Some(&current),
            |_| Some(fresh),
            NOW,
        );
        assert_eq!(overview.synced_peers, 3);
        assert_eq!(overview.replica_target, ORG_REPLICA_TARGET);
        assert!(overview.is_replica_sufficient());
    }
}
