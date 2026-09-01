//! 组织网关候选收集（connection-policy M3：网关优先 + 够用即停）。
//!
//! keepalive tick 的组织拨号已从「每 tick 无条件拨 ≤3 个」一路收敛：
//! - M3 改为网关优先 + 够用即停 + 成功冷却；
//! - M5 删覆盖网成员周期轮询段；
//! - **M6 彻底懒连接：tick 内主动拨号全删**（`plan_gateway_dials` /
//!   `ORG_DIAL_SUCCESS_COOLDOWN_MS` 一并删除）——组织成员连接只由懒拨号与
//!   被动接受入站建立。
//!
//! 本文件保留 [`connected_gateway_candidates`]：收集**已连接**的活跃网关端点，
//! 供 tick 反熵拉取与失联判空用（M6 后不再被拨号，但仍是「组织可达」的依据
//! 与反熵拉取源——无已连接候选时拉取循环天然空转，正是「没连接就不拉取」）。
//!
//! 纯逻辑层只做候选收集判定（不碰网络），供 `OrgSyncContext::maintain_org_tick`
//! 编排执行。

use std::collections::HashSet;

use crate::org::types::OrganizationRecord;
use crate::org::roles;
use crate::p2p::peer_targets::{PeerNodeInfo, extract_peer_id};

/// 收集**已连接**的活跃网关端点候选（I4）：当前用户所属 org 的活跃网关中，
/// peerId 已在 `connected` 集内的成员端点。供 tick 反熵拉取与失联恢复判空用——
/// 已连接网关不再被周期拨号（M5 删周期轮询、M6 删 tick 拨号），但仍是
/// 「组织可达」的判定依据与反熵拉取源。
pub fn connected_gateway_candidates(
    orgs: &[OrganizationRecord],
    current_root_id: &str,
    connected: &HashSet<String>,
    now_ms: i64,
) -> Vec<PeerNodeInfo> {
    let mut out: Vec<PeerNodeInfo> = Vec::new();
    for record in orgs {
        if record.find_member(current_root_id).is_none() {
            continue;
        }
        let gateway_root_ids = roles::gateway_active_set(record, now_ms);
        for root_id in &gateway_root_ids {
            let Some(member) = record.find_member(root_id) else {
                continue;
            };
            for candidate in member_endpoint_candidates(member) {
                if extract_peer_id(&candidate)
                    .as_deref()
                    .is_some_and(|p| connected.contains(p))
                {
                    out.push(candidate);
                }
            }
        }
    }
    out
}

/// 从成员端点集提取可拨号候选（peerId 或地址任一非空）。
fn member_endpoint_candidates(
    member: &crate::org::types::OrganizationMember,
) -> Vec<PeerNodeInfo> {
    let Some(set) = &member.node_info else {
        return Vec::new();
    };
    set.iter()
        .map(|info| PeerNodeInfo {
            peer_id: info.peer_id.clone(),
            addresses: info.addresses.clone(),
        })
        .filter(|c| {
            c.peer_id
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty())
                || !c.addresses.is_empty()
        })
        .collect()
}

/// 本机（`current_root_id`）在该组织是否已有成员端点连接（leaf 模式 §5
/// 单连接判定：已有任一组织连接即保持，不另拨）。
pub fn has_connected_org_member(
    record: &OrganizationRecord,
    current_root_id: &str,
    connected: &HashSet<String>,
) -> bool {
    record.members.iter().any(|m| {
        m.root_id != current_root_id
            && m.node_info.as_ref().is_some_and(|set| {
                set.iter()
                    .any(|info| info.peer_id.as_deref().is_some_and(|p| connected.contains(p)))
            })
    })
}

/// leaf 模式组织单连接的目标排序（mobile-leaf-mode §5）：**网关活跃集 →
/// 数据账号 → 任一最近在线成员**。成员表（含 deviceUid 端点集）是全员同步
/// 数据，leaf 读本地副本即可选人，无需任何发现协议。
///
/// - 网关活跃集按 [`roles::gateway_active_set`] 顺序（显式指定保序，缺省
///   确定性轮换）逐成员出端点；
/// - 数据账号（[`roles::data_account_set`]，显式指定或缺省全体管理员）排
///   第二梯队；
/// - 其余成员按端点 `last_seen_at`（PeerActivity 记录，经 `last_seen_of`
///   回调注入）最大值降序，无记录者垫底；
/// - 三梯队各自去重（同 rootId 只出现一次），本机（`current_root_id`）排除。
pub fn leaf_ordered_org_candidates(
    record: &OrganizationRecord,
    current_root_id: &str,
    now_ms: i64,
    last_seen_of: &mut dyn FnMut(&str) -> Option<i64>,
) -> Vec<PeerNodeInfo> {
    let mut ordered: Vec<String> = Vec::new();
    let push_unique = |rid: &str, ordered: &mut Vec<String>| {
        if rid != current_root_id && !ordered.iter().any(|r| r == rid) {
            ordered.push(rid.to_string());
        }
    };
    for rid in roles::gateway_active_set(record, now_ms) {
        push_unique(&rid, &mut ordered);
    }
    for rid in roles::data_account_set(record) {
        push_unique(&rid, &mut ordered);
    }
    // 其余成员：按端点 last_seen 最大值降序（无记录垫底；稳定排序保成员表序）
    let mut rest: Vec<&crate::org::types::OrganizationMember> = record
        .members
        .iter()
        .filter(|m| m.root_id != current_root_id && !ordered.iter().any(|r| r == &m.root_id))
        .collect();
    let mut member_last_seen = |m: &crate::org::types::OrganizationMember| -> i64 {
        m.node_info
            .as_ref()
            .map(|set| {
                set.iter()
                    .filter_map(|info| info.peer_id.as_deref())
                    .filter_map(|pid| last_seen_of(pid))
                    .max()
                    .unwrap_or(i64::MIN)
            })
            .unwrap_or(i64::MIN)
    };
    rest.sort_by_key(|m| std::cmp::Reverse(member_last_seen(m)));
    for m in rest {
        ordered.push(m.root_id.clone());
    }

    let mut out = Vec::new();
    for rid in &ordered {
        if let Some(member) = record.find_member(rid) {
            out.extend(member_endpoint_candidates(member));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{
        OrganizationDeviceSet, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
        OrganizationRole,
    };

    fn member(root_id: &str, peer_id: &str) -> OrganizationMember {
        let node_info = if peer_id.is_empty() {
            None
        } else {
            Some(OrganizationDeviceSet::from_single(OrganizationNodeInfo {
                device_uid: None,
                peer_id: Some(peer_id.to_string()),
                addresses: vec![format!("/ip4/1.2.3.4/tcp/1/ws/{peer_id}")],
            }))
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

    /// 构建一个显式指定网关的组织记录（跳过缺省活跃集推导的随机性）。
    /// 自动含一个本机成员 `self`（当前用户，非网关）。
    fn org_with_gateways(
        org_id: &str,
        members: Vec<OrganizationMember>,
        gateways: Vec<&str>,
    ) -> OrganizationRecord {
        let mut members = members;
        members.push(member("self", "p-self"));
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
            gateways: gateways.into_iter().map(ToString::to_string).collect(),
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        }
    }

    /// I4：已连接活跃网关并入判定集——反熵拉取源 + 失联恢复判空依据。
    #[test]
    fn connected_gateway_candidates_includes_connected() {
        let record = org_with_gateways(
            "org-1",
            vec![member("g1", "p-g1"), member("g2", "p-g2"), member("g3", "p-g3")],
            vec!["g1", "g2", "g3"],
        );
        // 已连接 g1、g3 → 返回这两个网关端点（供反熵拉取/恢复判空）
        let connected = HashSet::from(["p-g1".to_string(), "p-g3".to_string()]);
        let cands = connected_gateway_candidates(&[record], "self", &connected, 0);
        let got: HashSet<String> = cands.iter().filter_map(|c| c.peer_id.clone()).collect();
        assert_eq!(
            got,
            HashSet::from(["p-g1".to_string(), "p-g3".to_string()]),
            "已连接活跃网关并入判定集"
        );
        // 未连接的不计入
        assert!(!got.contains("p-g2"));
    }

    /// 非当前成员的组织不计入已连接网关候选。
    #[test]
    fn connected_gateway_candidates_skips_non_member_org() {
        let record = org_with_gateways("org-1", vec![member("g1", "p-g1")], vec!["g1"]);
        let connected = HashSet::from(["p-g1".to_string()]);
        let cands = connected_gateway_candidates(&[record], "other", &connected, 0);
        assert!(cands.is_empty(), "非成员组织不计入");
    }

    /// leaf 模式 §5 组织单连接目标排序：网关活跃集（保序）→ 数据账号 →
    /// 其余成员按端点 last_seen 降序（无记录垫底）；本机排除；跨梯队去重。
    #[test]
    fn leaf_ordered_org_candidates_gateway_data_recent() {
        let mut record = org_with_gateways(
            "org-1",
            vec![
                member("g1", "p-g1"),
                member("g2", "p-g2"),
                member("d1", "p-d1"),
                member("m1", "p-m1"),
                member("m2", "p-m2"),
            ],
            vec!["g1", "g2"],
        );
        // g1 同为数据账号：验证跨梯队去重（只在网梯队出现一次）
        record.data_accounts = vec!["d1".to_string(), "g1".to_string()];
        let mut last_seen = |pid: &str| match pid {
            "p-m1" => Some(100i64),
            "p-m2" => Some(200i64),
            _ => None,
        };
        let cands = leaf_ordered_org_candidates(&record, "self", 0, &mut last_seen);
        let got: Vec<String> = cands.iter().filter_map(|c| c.peer_id.clone()).collect();
        assert_eq!(
            got,
            vec!["p-g1", "p-g2", "p-d1", "p-m2", "p-m1"],
            "首个候选必须是网关活跃集端点；随后数据账号；再按 last_seen 降序"
        );
        assert!(!got.contains(&"p-self".to_string()), "本机端点排除");
    }

    /// leaf 排序：非显式网关时走缺省活跃集推导（确定性轮换取前 3），网关
    /// 梯队仍先于数据账号与普通成员；无 last_seen 记录不等于被剔除。
    #[test]
    fn leaf_ordered_org_candidates_default_gateway_set() {
        let record = org_with_gateways(
            "org-1",
            vec![member("a", "p-a"), member("b", "p-b")],
            vec![], // 未显式指定网关：缺省全体成员候选，轮换取前 3
        );
        let cands = leaf_ordered_org_candidates(&record, "self", 0, &mut |_| None);
        let got: Vec<String> = cands.iter().filter_map(|c| c.peer_id.clone()).collect();
        assert_eq!(got.len(), 2, "两名成员端点都应在候选中（无记录垫底不剔除）");
        assert!(!got.contains(&"p-self".to_string()));
    }

    /// leaf 单连接判定（H-C/M-C）：成员端点 peerId 命中已连接集 → true；
    /// 本机端点与未连接成员不计。
    #[test]
    fn has_connected_org_member_detects_existing_link() {
        let record = org_with_gateways(
            "org-1",
            vec![member("g1", "p-g1"), member("m1", "p-m1")],
            vec!["g1"],
        );
        let connected = HashSet::from(["p-m1".to_string()]);
        assert!(
            has_connected_org_member(&record, "self", &connected),
            "已连接成员端点应判定为已有组织连接"
        );
        let connected = HashSet::from(["p-other".to_string(), "p-self".to_string()]);
        assert!(
            !has_connected_org_member(&record, "self", &connected),
            "非成员端点/本机端点不计入"
        );
        assert!(
            !has_connected_org_member(&record, "self", &HashSet::new()),
            "无连接时为 false"
        );
    }
}
