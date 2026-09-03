//! K 副本统计与 OrgNetworkStatus 判定单测。

use spark_core::org::replica::*;
use spark_core::org::sync_state::OrgSyncState;
use spark_core::org::types::{
    OrganizationDeviceSet, OrganizationMember, OrganizationNodeInfo, OrganizationRole,
    OrganizationSyncVersions,
};
use spark_core::p2p::RecoveryState;

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
        node_info: peer_id.map(|p| {
            OrganizationDeviceSet::from_single(OrganizationNodeInfo {
                device_uid: None,
                peer_id: Some(p.to_string()),
                addresses: vec![],
            })
        }),
        ..Default::default()
    }
}

fn no_state() -> impl FnMut(&str, Option<&str>) -> Option<OrgSyncState> {
    |_, _| None
}

/// 由成员列表构造记录（K 口径测试夹具）。
fn record_of(members: &[OrganizationMember]) -> spark_core::org::types::OrganizationRecord {
    spark_core::org::types::OrganizationRecord {
        org_id: "org_x".to_string(),
        created_by: rid('z'),
        members: members.to_vec(),
        ..Default::default()
    }
}

/// batch2 新签名的调用包装：成员级用例默认无履职观测、K 不适用
/// （has_data_collections=false → synced_peers 保持成员级口径）。
fn overview_of(
    members: &[OrganizationMember],
    current_root_id: Option<&str>,
    current_versions: Option<&OrganizationSyncVersions>,
    state_lookup: impl FnMut(&str, Option<&str>) -> Option<OrgSyncState>,
    duty: &[DutyObservation],
    has_data_collections: bool,
    now_ms: i64,
) -> OrgSyncOverview {
    compute_org_sync_overview(
        &record_of(members),
        current_root_id,
        current_versions,
        state_lookup,
        duty,
        has_data_collections,
        |_| "pc",
        now_ms,
    )
}

#[test]
fn self_always_counts() {
    let members = vec![member('a', None)];
    let overview =
        overview_of(&members, Some(&rid('a')), None, no_state(), &[], false, NOW);
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
    let overview = overview_of(
        &members,
        None,
        Some(&current),
        |_, _| Some(state_at(NOW - ORG_REPLICA_FRESH_WINDOW_MS)),
        &[],
        false,
        NOW,
    );
    assert!(overview.members[0].ever_synced, "30 天边界仍计入");
    // 超出窗口 1ms 且版本落后 → 不计入
    let overview = overview_of(
        &members,
        None,
        Some(&current),
        |_, _| Some(state_at(NOW - ORG_REPLICA_FRESH_WINDOW_MS - 1)),
        &[],
        false,
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
    let overview = overview_of(
        &members,
        None,
        Some(&current),
        |_, _| Some(stale_ttl),
        &[],
        false,
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
    let overview = overview_of(
        &members,
        None,
        Some(&current),
        |_, _| Some(lagging),
        &[],
        false,
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
        overview_of(&members, None, Some(&current), no_state(), &[], false, NOW);
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
    let overview = overview_of(&members, None, None, no_state(), &[], false, NOW);
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
    let overview = overview_of(
        &members,
        None,
        Some(&current),
        |root, legacy_peer| {
            // O1 账号口径：rootId 为主键，legacy peerId 是迁移回填的辅助键
            assert_eq!(root, rid('b'), "lookup 主键是账号 rootId");
            assert_eq!(
                legacy_peer,
                Some("peer-b"),
                "legacy 键必须用 trim 后的 peerId"
            );
            Some(state)
        },
        &[],
        false,
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
    let overview = overview_of(
        &members,
        Some(&rid('a')),
        Some(&current),
        |_, _| Some(fresh),
        &[],
        false,
        NOW,
    );
    assert_eq!(overview.synced_peers, 3);
    assert_eq!(overview.replica_target, ORG_REPLICA_TARGET);
    assert!(overview.is_replica_sufficient());
    // 纯统计路径：网络字段为占位默认
    assert_eq!(overview.connected_peers, 0);
    assert_eq!(overview.recovery_state, RecoveryState::Idle);
    assert_eq!(overview.last_connected_at, None);
    assert_eq!(overview.status, OrgNetworkStatus::LocalOnly);
}

// ------------------------------------------------------------------
// 组织网络状态判定
// ------------------------------------------------------------------

fn status_input() -> OrgNetworkStatusInput {
    OrgNetworkStatusInput {
        p2p_running: true,
        total_connected_peers: 5,
        connected_peers: 2,
        replica_target: 3,
        total_members: 3,
        recovery_state: RecoveryState::Idle,
        unreachable_ms: 0,
    }
}

#[test]
fn status_good_when_connected_replicas_reach_target() {
    // 本机 + 2 远端 = 3/3 → 良好
    assert_eq!(
        decide_org_network_status(&status_input()),
        OrgNetworkStatus::Good
    );
    // 超出目标也算良好
    let mut input = status_input();
    input.connected_peers = 4;
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Good);
}

#[test]
fn status_good_scales_target_for_small_orgs() {
    // 单成员组织：仅本机即达标（目标折算 min(K, 成员数)）
    let mut input = status_input();
    input.total_members = 1;
    input.connected_peers = 0;
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Good);
    // 双成员组织：本机 + 1 远端 → 良好
    input.total_members = 2;
    input.connected_peers = 1;
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Good);
}

#[test]
fn status_unstable_when_partial_below_target() {
    // 本机 + 1 远端 = 2/3 → 不稳定
    let mut input = status_input();
    input.connected_peers = 1;
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::Unstable
    );
}

#[test]
fn status_debounce_window_stays_unstable() {
    let mut input = status_input();
    input.connected_peers = 0;
    // 刚断开（防抖窗口内）→ 不稳定，不判丢失
    input.unreachable_ms = ORG_NETWORK_LOST_DEBOUNCE_MS - 1;
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::Unstable
    );
    // 恰好到达防抖边界 → 进入丢失分支
    input.unreachable_ms = ORG_NETWORK_LOST_DEBOUNCE_MS;
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Lost);
}

#[test]
fn status_lost_requires_some_overlay_connectivity() {
    let mut input = status_input();
    input.connected_peers = 0;
    input.unreachable_ms = ORG_NETWORK_LOST_DEBOUNCE_MS + 1;
    // 覆盖网仍有连接（在线但组织成员全部失败）→ 丢失
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Lost);
    // 全网零连接（完全离线）→ 仅本地
    input.total_connected_peers = 0;
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::LocalOnly
    );
}

#[test]
fn status_recovering_overrides_lost_but_not_local_only() {
    let mut input = status_input();
    input.connected_peers = 0;
    input.unreachable_ms = ORG_NETWORK_LOST_DEBOUNCE_MS + 1;
    input.recovery_state = RecoveryState::Recovering { since: NOW };
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::Recovering
    );
    // failed 不再显示恢复中：回到丢失/仅本地
    input.recovery_state = RecoveryState::Failed { since: NOW };
    assert_eq!(decide_org_network_status(&input), OrgNetworkStatus::Lost);
    input.total_connected_peers = 0;
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::LocalOnly
    );
}

#[test]
fn status_p2p_stopped_is_local_only() {
    let mut input = status_input();
    input.p2p_running = false;
    assert_eq!(
        decide_org_network_status(&input),
        OrgNetworkStatus::LocalOnly
    );
}

#[test]
fn status_strings_stable() {
    assert_eq!(OrgNetworkStatus::Good.as_str(), "good");
    assert_eq!(OrgNetworkStatus::Unstable.as_str(), "unstable");
    assert_eq!(OrgNetworkStatus::Lost.as_str(), "lost");
    assert_eq!(OrgNetworkStatus::Recovering.as_str(), "recovering");
    assert_eq!(OrgNetworkStatus::LocalOnly.as_str(), "localOnly");
}

// ---------------------------------------------------------------------------
// batch2 §1.2：K 口径细分（hello 履职观测驱动，计数单元 = 数据账号 × PC 设备）
// ---------------------------------------------------------------------------

fn admin(root: char) -> OrganizationMember {
    let mut m = member(root, None);
    m.role = OrganizationRole::Admin;
    m
}

fn duty_of(root: char, peer: &str, class: &str, at: i64) -> DutyObservation {
    DutyObservation {
        root_id: rid(root),
        peer_id: peer.to_string(),
        device_class: class.to_string(),
        observed_at: at,
    }
}

/// (1,1,1) 合格：三个数据账号各一台 PC 窗口内履职 → 组织级 3 ≥ K 且逐账号达标。
#[test]
fn k_sufficient_one_pc_per_account() {
    let members = vec![admin('a'), admin('b'), admin('c')];
    let duty = vec![
        duty_of('a', "p-a", "pc", NOW),
        duty_of('b', "p-b", "pc", NOW),
        duty_of('c', "p-c", "pc", NOW),
    ];
    let overview = overview_of(&members, None, None, no_state(), &duty, true, NOW);
    assert!(overview.k_applicable);
    assert_eq!(overview.synced_peers, 3, "去重 PC 履职对合计");
    assert!(overview.data_accounts.iter().all(|a| a.pc_synced));
    assert!(overview.is_replica_sufficient(), "(1,1,1) 合格");
}

/// (3) 合格：单数据账号三台 PC 履职（小组织默认形态）→ 组织级 3 + 该账号达标。
#[test]
fn k_sufficient_single_account_three_pcs() {
    let members = vec![admin('a')];
    let duty = vec![
        duty_of('a', "p-a1", "pc", NOW),
        duty_of('a', "p-a2", "pc", NOW),
        duty_of('a', "p-a3", "pc", NOW),
    ];
    let overview = overview_of(&members, None, None, no_state(), &duty, true, NOW);
    assert_eq!(overview.synced_peers, 3);
    assert!(overview.is_replica_sufficient(), "(3) 合格");
}

/// (2,0) 不达标：两数据账号，一个有 2 台 PC、一个空心（无履职）→ 组织级
/// 不足 + 空心账号 → 不达标（只提醒，记录口径可辨识）。
#[test]
fn k_insufficient_hollow_account() {
    let members = vec![admin('a'), admin('b')];
    let duty = vec![
        duty_of('a', "p-a1", "pc", NOW),
        duty_of('a', "p-a2", "pc", NOW),
    ];
    let overview = overview_of(&members, None, None, no_state(), &duty, true, NOW);
    assert_eq!(overview.synced_peers, 2, "空心账号不计");
    let b = overview.data_accounts.iter().find(|a| a.root_id == rid('b')).unwrap();
    assert!(!b.pc_synced, "空心数据账号标记不达标");
    assert!(!overview.is_replica_sufficient(), "(2,0) 不达标");
}

/// all-members 组织无 K：不适用 → 不提醒（恒达标），成员级计数保持展示口径。
#[test]
fn k_not_applicable_for_all_members_org() {
    let members = vec![member('a', None), member('b', None)];
    let overview = overview_of(&members, None, None, no_state(), &[], false, NOW);
    assert!(!overview.k_applicable, "无 data-accounts 集合 → 无 K");
    assert!(overview.is_replica_sufficient(), "无 K 不提醒（恒达标）");
    assert!(overview.data_accounts.is_empty());
}

/// 窗口外履职不计；mobile 不计；去重（同账号同设备重复观测算一对）。
#[test]
fn k_duty_window_and_device_class_and_dedup() {
    let members = vec![admin('a'), admin('b'), admin('c')];
    let duty = vec![
        // 窗口外（30 天 + 1ms）→ 不计
        duty_of('a', "p-a", "pc", NOW - ORG_REPLICA_FRESH_WINDOW_MS - 1),
        // mobile 不计
        duty_of('b', "p-b", "mobile", NOW),
        // 重复观测同（账号, 设备）→ 去重算一对
        duty_of('c', "p-c", "pc", NOW),
        duty_of('c', "p-c", "pc", NOW - 1000),
    ];
    let overview = overview_of(&members, None, None, no_state(), &duty, true, NOW);
    assert_eq!(overview.synced_peers, 1, "窗口外 + mobile + 去重后仅 1 对");
    assert!(!overview.is_replica_sufficient());
    // 窗口边界恰好 30 天仍计入
    let duty_edge = vec![
        duty_of('a', "p-a", "pc", NOW - ORG_REPLICA_FRESH_WINDOW_MS),
        duty_of('b', "p-b", "pc", NOW),
        duty_of('c', "p-c", "pc", NOW),
    ];
    let overview = overview_of(&members, None, None, no_state(), &duty_edge, true, NOW);
    assert_eq!(overview.synced_peers, 3, "30 天边界仍计入");
    assert!(overview.is_replica_sufficient());
}

/// 本机是数据账号且为 PC → 自证计入（无需 hello 自观测）。
#[test]
fn k_self_data_account_pc_counts() {
    let members = vec![admin('a'), admin('b'), admin('c')];
    let duty = vec![duty_of('b', "p-b", "pc", NOW), duty_of('c', "p-c", "pc", NOW)];
    let overview = overview_of(&members, Some(&rid('a')), None, no_state(), &duty, true, NOW);
    assert_eq!(overview.synced_peers, 3, "本机自证 + 两台观测 = 3");
    assert!(overview.is_replica_sufficient());
}
