//! M6 事件驱动补副本的**纯决策**（无网络）：副本充足性判定 + 补推目标规划 + 节流。
//!
//! connection-policy M6 起，管理员补副本不再由 keepalive tick 周期触发，改挂到
//! 组织写入事件点（push.rs `ensure_replicas_after_write` 调用本模块的
//! [`plan_replica_push_targets`]）。本模块只做本地存储读判定（不碰网络）：
//! - [`plan_replica_push_targets`]：管理员且副本不足 K 时返回应补推成员；副本已足 /
//!   非管理员 / 无未同步可寻址成员时返回空（零外联短路）。
//! - [`replica_check_due`]：每 org 最小检查间隔的节流判定（防写入风暴反复推送）。
//!
//! 拆成独立文件：push.rs 专注 org-share 推送链路（<450 行），本模块承载补副本
//! 决策与其单测。

use super::{REPLICA_CHECK_MIN_INTERVAL_MS, REPLICA_PUSH_PER_ORG};
use crate::org::sync_state::OrgSyncState;
use crate::org::types::OrganizationRecord;
use crate::org::{compute_org_sync_overview, resolve_local_versions};
use crate::p2p::peer_targets::PeerNodeInfo;

/// M6 事件驱动补副本的**纯决策**（无网络）：管理员且副本不足 K 时，返回应补推
/// 快照的目标成员（每组织最多 [`REPLICA_PUSH_PER_ORG`] 个，取首个可寻址端点）；
/// 副本已足 / 非管理员 / 无未同步可寻址成员时返回空（零外联短路）。
///
/// 拆出来便于单测：网络推送只在有返回值时发生，测试直接断言空/非空与目标集合。
pub(crate) fn plan_replica_push_targets<S: crate::storage::StorageBackend>(
    storage: &S,
    record: &OrganizationRecord,
    actor_root_id: &str,
    mut state_lookup: impl FnMut(&str, Option<&str>) -> Option<OrgSyncState>,
    now_ms: i64,
) -> Vec<(PeerNodeInfo, String)> {
    // 仅管理员触发补副本
    if !record.is_admin(actor_root_id) {
        return Vec::new();
    }
    let versions = record
        .sync
        .as_ref()
        .map(|s| s.versions)
        .or_else(|| Some(resolve_local_versions(record)));
    // batch2 §1.2：K 口径证据 = hello 履职观测 + data-accounts 声明适用性 +
    // 设备类兜底（无观测时按设备记录，无记录兜底 pc）
    let duty: Vec<crate::org::DutyObservation> =
        crate::sync::orgsync::orgq_da_duty_observations(storage, &record.org_id)
            .into_iter()
            .map(|(root_id, peer_id, device_class, observed_at)| crate::org::DutyObservation {
                root_id,
                peer_id,
                device_class,
                observed_at,
            })
            .collect();
    let has_data_collections =
        crate::plugindata::org_has_data_account_collections(storage, &record.org_id);
    let overview = compute_org_sync_overview(
        record,
        Some(actor_root_id),
        versions.as_ref(),
        &mut state_lookup,
        &duty,
        has_data_collections,
        |root_id| {
            record
                .find_member(root_id)
                .map(|m| crate::org::roles::member_device_class(storage, m))
                .unwrap_or("pc")
        },
        now_ms,
    );
    if overview.is_replica_sufficient() {
        return Vec::new();
    }
    let mut targets = Vec::new();
    for member in &overview.members {
        if targets.len() >= REPLICA_PUSH_PER_ORG {
            break;
        }
        if member.is_self || member.ever_synced {
            continue;
        }
        // 端点化：遍历成员端点集，取首个可寻址端点（peerId 或地址非空）。
        let Some(set) = record
            .find_member(&member.root_id)
            .and_then(|m| m.node_info.clone())
        else {
            continue;
        };
        let Some(info) = set.iter().find(|info| {
            info.peer_id
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty())
                || !info.addresses.is_empty()
        }) else {
            continue;
        };
        targets.push((
            PeerNodeInfo {
                peer_id: info.peer_id.clone(),
                addresses: info.addresses.clone(),
            },
            member.root_id.clone(),
        ));
    }
    targets
}

/// 补副本节流判定：距上次检查 >= [`REPLICA_CHECK_MIN_INTERVAL_MS`]（或从未检查，
/// `last_ms == 0`）才允许再次检查。拆出便于单测节流窗口行为。
pub(crate) fn replica_check_due(last_ms: i64, now_ms: i64) -> bool {
    last_ms == 0 || now_ms - last_ms >= REPLICA_CHECK_MIN_INTERVAL_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationNodeInfo, OrganizationRole};

    fn member(root_id: &str, peer_id: &str) -> OrganizationMember {
        let node_info = if peer_id.is_empty() {
            None
        } else {
            Some(crate::org::types::OrganizationDeviceSet::from_single(
                OrganizationNodeInfo {
                    device_uid: None,
                    peer_id: Some(peer_id.to_string()),
                    addresses: vec![format!("/ip4/1.2.3.4/tcp/1/ws/{peer_id}")],
                },
            ))
        };
        OrganizationMember {
            root_id: root_id.to_string(),
            role: OrganizationRole::Member,
            joined_at: 1000,
            added_by: "creator".to_string(),
            node_info,
            nickname: None,
            avatar: None,
            signature: None,
            gender: None,
            region: None,
            use_personal_identity: None,
            access_key: None,
            extra: Default::default(),
        }
    }

    /// 构造组织（本机 `self` 为唯一管理员）。
    fn admin_org(org_id: &str, extra: Vec<OrganizationMember>) -> OrganizationRecord {
        let mut members = vec![{
            let mut m = member("self", "p-self");
            m.role = OrganizationRole::Admin;
            m
        }];
        members.extend(extra);
        OrganizationRecord {
            org_id: org_id.to_string(),
            name: "测试组织".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: "creator".to_string(),
            updated_at: 1000,
            members,
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        }
    }

    /// 无 sync-state → 未同步（不计副本）。
    fn no_state(_: &str, _: Option<&str>) -> Option<OrgSyncState> {
        None
    }

    /// batch2 K 口径测试设施：含一条 data-accounts 集合声明的存储
    ///（K 口径适用；纯 all-members 组织无 K 不做达标判定）。
    fn storage_with_data_decl() -> crate::storage::MemoryStorage {
        let mut s = crate::storage::MemoryStorage::new();
        crate::storage::StorageBackend::put(
            &mut s,
            "org:coll:org-1:ai-chat:x@v1",
            r#"{"name":"ai-chat:x","version":"1","accounts":"data-accounts"}"#,
        )
        .unwrap();
        s
    }

    /// 写一条 PC 履职观测（hello roles 含 data + deviceClass=pc 的等价落库）。
    fn duty(s: &mut crate::storage::MemoryStorage, root: &str, peer: &str, ts: i64) {
        crate::sync::orgsync::orgq_note_data_account_duty(s, "org-1", root, peer, "pc", ts);
    }

    /// 命中该成员 rootId → 返回最近同步过（30 天窗口内）的 sync-state，计入副本。
    /// `synced_roots` 集合内的成员视为已同步。
    fn synced_state<'a>(
        synced_roots: &'a [String],
    ) -> impl FnMut(&str, Option<&str>) -> Option<OrgSyncState> + 'a {
        move |root_id, _| {
            if synced_roots.iter().any(|r| r == root_id) {
                Some(OrgSyncState {
                    versions: Default::default(),
                    last_synced_at: 1000,
                })
            } else {
                None
            }
        }
    }

    /// 副本不足（本机 1 + 无已同步成员 < K=3）→ 向未同步可寻址成员推送。
    #[test]
    fn replica_insufficient_pushes_unsynced_members() {
        let record = admin_org("org-1", vec![member("a", "p-a"), member("b", "p-b")]);
        let targets = plan_replica_push_targets(&mut storage_with_data_decl(), &record, "self", no_state, 1000);
        let got: Vec<String> = targets.iter().map(|t| t.1.clone()).collect();
        assert_eq!(
            got,
            vec!["a".to_string(), "b".to_string()],
            "不足 K 推全部未同步成员"
        );
        // 端点带出 peerId（供 `sync_org_to_member` 懒拨号）
        assert_eq!(targets[0].0.peer_id.as_deref(), Some("p-a"));
    }

    /// 副本充足（含本机 ≥ K）→ 零外联（返回空，不推送）。
    #[test]
    fn replica_sufficient_no_push() {
        // batch2 口径：本机（数据账号 PC 自证 1 对）+ 2 台 PC 履职观测 = 3 ≥ K
        let record = admin_org("org-1", vec![member("a", "p-a"), member("b", "p-b")]);
        let mut storage = storage_with_data_decl();
        duty(&mut storage, "a", "p-a", 1000);
        duty(&mut storage, "b", "p-b", 1000);
        let targets = plan_replica_push_targets(&storage, &record, "self", no_state, 1000);
        assert!(targets.is_empty(), "副本已足 → 零外联");
    }

    /// 非管理员不触发补副本（即使副本不足）。
    #[test]
    fn non_admin_never_pushes() {
        let record = admin_org("org-1", vec![member("a", "p-a")]);
        // actor 非管理员（org 里没有的角色）
        let targets = plan_replica_push_targets(&mut storage_with_data_decl(), &record, "not-admin", no_state, 1000);
        assert!(targets.is_empty(), "非管理员不触发补副本");
    }

    /// 推送目标上限 REPLICA_PUSH_PER_ORG=2：超过 2 个未同步成员时只取前 2。
    #[test]
    fn replica_push_capped_at_two() {
        let record = admin_org(
            "org-1",
            vec![member("a", "p-a"), member("b", "p-b"), member("c", "p-c")],
        );
        let targets = plan_replica_push_targets(&mut storage_with_data_decl(), &record, "self", no_state, 1000);
        assert_eq!(targets.len(), 2, "每组织最多推送 2 个");
        assert_eq!(targets[0].1, "a");
        assert_eq!(targets[1].1, "b");
    }

    /// 无 nodeInfo / 无可寻址端点的未同步成员不推（跳过后仍不足，但无可推目标 → 空）。
    #[test]
    fn replica_push_skips_unaddressable_member() {
        // 本机 + 一个无 nodeInfo 的未同步成员 → 副本不足但无可推目标
        let record = admin_org("org-1", vec![member("a", "")]);
        let targets = plan_replica_push_targets(&mut storage_with_data_decl(), &record, "self", no_state, 1000);
        assert!(targets.is_empty(), "无寻址端点的成员不可推，返回空");
    }

    /// 节流窗口：间隔内禁止再次检查；越过最小间隔允许；从未检查（0）允许。
    #[test]
    fn replica_check_throttle_window() {
        let interval = REPLICA_CHECK_MIN_INTERVAL_MS;
        assert!(replica_check_due(0, 1000), "从未检查 → 立即允许");
        assert!(
            !replica_check_due(1000, 1000 + interval - 1),
            "间隔内 → 短路跳过"
        );
        assert!(replica_check_due(1000, 1000 + interval), "到达间隔 → 允许");
    }
}
