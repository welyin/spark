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
}
