//! org-pull 响应方纯逻辑单测（列表/快照响应帧、入站载荷校验、响应分类）。

use serde_json::Value;

use spark_core::identity::{derive_root_identity, parse_mnemonic};
use spark_core::org::claim::sign_node_info_claim;
use spark_core::org::pull::*;
use spark_core::org::service::{CreateOrganizationInput, OrganizationService};
use spark_core::org::types::{OrganizationNodeInfo, OrganizationRecord};
use spark_core::storage::MemoryStorage;

const NOW: i64 = 1_720_000_000_000;
const MNEMONIC: &str = "与 祝 产 鸡 永 烂 施 师 蓝 荷 有 邓 朗 防 管 李 原 芳 饿 万 措 走 腰 旅";
const MNEMONIC2: &str = "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal will";

fn root_id_of(mnemonic: &str) -> String {
    let parsed = parse_mnemonic(mnemonic).unwrap();
    derive_root_identity(&parsed.seed).id()
}

fn setup() -> (MemoryStorage, String, OrganizationRecord) {
    let mut storage = MemoryStorage::new();
    let admin = root_id_of(MNEMONIC);
    let record = OrganizationService::create_organization(
        &mut storage,
        &CreateOrganizationInput {
            name: "组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        },
        &admin,
        NOW,
    )
    .unwrap();
    (storage, admin, record)
}

#[test]
fn auth_status_rules() {
    let (mut storage, admin, record) = setup();
    // 非成员
    assert_eq!(
        member_auth_status(&record, &"ab".repeat(32), None),
        Err("not-member")
    );
    // 成员无 peerId → 放行
    assert_eq!(member_auth_status(&record, &admin, None), Ok(()));
    // 成员带 peerId：一致放行，不一致/缺失拒绝
    let member = root_id_of(MNEMONIC2);
    OrganizationService::add_member(
        &mut storage,
        &record.org_id,
        &member,
        Some(&OrganizationNodeInfo {
            peer_id: Some("peer-xxx1".to_string()),
            addresses: vec![],
        }),
        &admin,
        NOW,
    )
    .unwrap();
    let record = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        member_auth_status(&record, &member, Some("peer-xxx1")),
        Ok(())
    );
    assert_eq!(
        member_auth_status(&record, &member, Some(" peer-xxx1 ")),
        Ok(())
    );
    assert_eq!(
        member_auth_status(&record, &member, Some("peer-yyy2")),
        Err("peer-mismatch")
    );
    assert_eq!(
        member_auth_status(&record, &member, None),
        Err("peer-mismatch")
    );
}

#[test]
fn pull_list_missing_requester() {
    let (mut storage, _, _) = setup();
    let (response, applied) =
        handle_pull_list_request(&mut storage, &serde_json::json!({}), None, None, NOW).unwrap();
    assert!(applied.is_empty());
    assert_eq!(response["ok"], false);
    assert_eq!(response["type"], "org-pull-list-response");
    assert_eq!(response["reason"], "missing-requester-root");
}

#[test]
fn pull_list_filters_by_membership() {
    let (mut storage, admin, record) = setup();
    let member = root_id_of(MNEMONIC2);
    OrganizationService::add_member(&mut storage, &record.org_id, &member, None, &admin, NOW)
        .unwrap();

    // 成员可见（sync 为 record.sync.versions 未塌缩形状）
    let (response, applied) = handle_pull_list_request(
        &mut storage,
        &serde_json::json!({"requesterRootId": member}),
        Some(&admin),
        None,
        NOW,
    )
    .unwrap();
    assert!(applied.is_empty());
    assert_eq!(response["ok"], true);
    let orgs = response["organizations"].as_array().unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0]["orgId"], record.org_id);
    assert!(orgs[0]["sync"]["summaryVersion"].is_number());

    // 非成员 → 空列表
    let (response, applied) = handle_pull_list_request(
        &mut storage,
        &serde_json::json!({"requesterRootId": "cd".repeat(32)}),
        Some(&admin),
        None,
        NOW,
    )
    .unwrap();
    assert!(applied.is_empty());
    assert_eq!(response["organizations"].as_array().unwrap().len(), 0);
}

#[test]
fn pull_list_claim_applied_only_for_known_member() {
    let (mut storage, admin, record) = setup();
    let member = root_id_of(MNEMONIC2);
    OrganizationService::add_member(&mut storage, &record.org_id, &member, None, &admin, NOW)
        .unwrap();

    let parsed = parse_mnemonic(MNEMONIC2).unwrap();
    let identity = derive_root_identity(&parsed.seed);
    let claim = sign_node_info_claim(
        &identity.signing_key,
        OrganizationNodeInfo {
            peer_id: Some("member-peer".to_string()),
            addresses: vec!["/ip4/1.2.3.4/tcp/1".to_string()],
        },
        NOW,
    );
    let claim_value = serde_json::to_value(&claim).unwrap();

    // 已知成员：claim 应用 → 回填 nodeInfo（且 admin 视角重读可见）
    let (response, applied) = handle_pull_list_request(
        &mut storage,
        &serde_json::json!({
            "requesterRootId": member,
            "requesterPeerId": "member-peer",
            "nodeInfoClaim": claim_value,
        }),
        Some(&admin),
        Some("member-peer"),
        NOW,
    )
    .unwrap();
    assert_eq!(
        applied,
        vec![record.org_id.clone()],
        "claim 应用组织随响应返回"
    );
    assert_eq!(response["ok"], true);
    let updated = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    let m = updated.find_member(&member).unwrap();
    assert_eq!(
        m.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some("member-peer")
    );
    // 响应里的版本是回填后重读的版本（= NOW  bump 后的 updatedAt）
    let orgs = response["organizations"].as_array().unwrap();
    assert_eq!(orgs[0]["sync"]["membersVersion"], Value::from(NOW));

    // 非成员：claim 不应用（组织记录不被触碰）
    let stranger = "ee".repeat(32);
    let parsed = parse_mnemonic(MNEMONIC2).unwrap();
    let identity = derive_root_identity(&parsed.seed);
    let stranger_claim = sign_node_info_claim(
        &identity.signing_key,
        OrganizationNodeInfo {
            peer_id: Some("stranger-peer".to_string()),
            addresses: vec!["/ip4/9.9.9.9/tcp/1".to_string()],
        },
        NOW,
    );
    let mut claim_v = serde_json::to_value(&stranger_claim).unwrap();
    // 把 claim 的 rootId 换成非成员（验签会失败，但门卫在验签之前就该拦截）
    claim_v["rootId"] = Value::from(stranger.clone());
    let before = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    let (response, applied) = handle_pull_list_request(
        &mut storage,
        &serde_json::json!({
            "requesterRootId": stranger,
            "nodeInfoClaim": claim_v,
        }),
        Some(&admin),
        None,
        NOW + 1000,
    )
    .unwrap();
    assert!(applied.is_empty());
    assert_eq!(response["organizations"].as_array().unwrap().len(), 0);
    let after = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(before, after, "非成员 claim 不得改动任何组织记录");
}

#[test]
fn pull_org_response_shapes() {
    let (mut storage, admin, record) = setup();
    let member = root_id_of(MNEMONIC2);
    OrganizationService::add_member(&mut storage, &record.org_id, &member, None, &admin, NOW)
        .unwrap();

    // missing requester
    let response =
        handle_pull_org_request(&storage, &serde_json::json!({"orgId": record.org_id}), None)
            .unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["reason"], "missing-requester-root");
    assert_eq!(response["orgId"], record.org_id);

    // missing org id
    let response = handle_pull_org_request(
        &storage,
        &serde_json::json!({"requesterRootId": member}),
        None,
    )
    .unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["reason"], "missing-org-id");

    // 组织不存在 → removed/org-not-found
    let response = handle_pull_org_request(
        &storage,
        &serde_json::json!({"requesterRootId": member, "orgId": "org_nope"}),
        None,
    )
    .unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["status"], "removed");
    assert_eq!(response["reason"], "org-not-found");

    // 非成员 → removed/not-member（与真删除不可区分）
    let response = handle_pull_org_request(
        &storage,
        &serde_json::json!({"requesterRootId": "ff".repeat(32), "orgId": record.org_id}),
        None,
    )
    .unwrap();
    assert_eq!(response["status"], "removed");
    assert_eq!(response["reason"], "not-member");

    // 成员 → member + 重建快照（版本塌缩：四字段 = updatedAt）+ pluginDocs
    let response = handle_pull_org_request(
        &storage,
        &serde_json::json!({"requesterRootId": member, "orgId": record.org_id}),
        None,
    )
    .unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["status"], "member");
    let org = &response["organization"];
    assert_eq!(org["orgId"], record.org_id);
    assert!(org.get("summary").is_some(), "快照线形（非原始记录）");
    let updated = OrganizationService::get_record(&storage, &record.org_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        org["sync"]["summaryVersion"],
        Value::from(updated.updated_at)
    );
    assert_eq!(
        org["sync"]["transactionsVersion"],
        Value::from(updated.updated_at)
    );
    assert_eq!(response["pluginDocs"].as_array().unwrap().len(), 0);
}

#[test]
fn classify_pull_org_responses() {
    assert_eq!(
        classify_pull_org_response(None),
        PullOrgOutcome::Unavailable
    );
    assert_eq!(
        classify_pull_org_response(Some(&serde_json::json!({"ok": false}))),
        PullOrgOutcome::Unavailable
    );
    assert_eq!(
        classify_pull_org_response(Some(&serde_json::json!({
            "ok": true, "type": "org-pull-org-response", "status": "removed"
        }))),
        PullOrgOutcome::Removed
    );
    let member_response = serde_json::json!({
        "ok": true, "type": "org-pull-org-response", "status": "member",
        "organization": {"orgId": "org_x"},
        "pluginDocs": [{"id": "d1"}]
    });
    match classify_pull_org_response(Some(&member_response)) {
        PullOrgOutcome::Member {
            organization,
            plugin_docs,
        } => {
            assert_eq!(organization["orgId"], "org_x");
            assert_eq!(plugin_docs.len(), 1);
        }
        other => panic!("expected Member, got {other:?}"),
    }
    // member 但 organization 缺失 → Unavailable
    assert_eq!(
        classify_pull_org_response(Some(&serde_json::json!({
            "ok": true, "type": "org-pull-org-response", "status": "member",
            "organization": null
        }))),
        PullOrgOutcome::Unavailable
    );
}

#[test]
fn parse_pull_list_items() {
    let response = serde_json::json!({
        "ok": true, "type": "org-pull-list-response",
        "organizations": [
            {"orgId": "org_a", "sync": {"summaryVersion": 1, "membersVersion": 2, "memberDetailsVersion": 3, "transactionsVersion": 4}},
            {"orgId": "org_b"},
            {"noOrgId": true}
        ]
    });
    let items = parse_pull_list_organizations(&response);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].0, "org_a");
    assert_eq!(items[0].1.as_ref().unwrap().members_version, 2);
    assert_eq!(items[1].0, "org_b");
    assert!(items[1].1.is_none());

    // ok:false / 其他 type → 空
    assert!(parse_pull_list_organizations(&serde_json::json!({"ok": false})).is_empty());
    assert!(
        parse_pull_list_organizations(&serde_json::json!({
            "ok": true, "type": "org-pull-org-response"
        }))
        .is_empty()
    );
}

#[test]
fn resolve_local_versions_fallback() {
    let (.., record) = setup();
    // 有 sync → 用 record.sync.versions
    let v = resolve_local_versions(&record);
    assert_eq!(v.summary_version, NOW);
    // 无 sync → 塌缩到 updatedAt
    let mut bare = record.clone();
    bare.sync = None;
    let v = resolve_local_versions(&bare);
    assert_eq!(v.summary_version, bare.updated_at);
    assert_eq!(v.transactions_version, bare.updated_at);
}

#[test]
fn validate_share_payload_rules() {
    let me = "ab".repeat(32);
    let payload = serde_json::json!({
        "targetRootId": me,
        "syncId": "s1",
        "organization": {"orgId": "org_x", "members": [{"rootId": me}]},
        "pluginDocs": [{"id": 1}]
    });
    let (target, org, sync_id, docs) =
        validate_incoming_share_payload(&payload, Some(&me)).unwrap();
    assert_eq!(target, me);
    assert_eq!(org["orgId"], "org_x");
    assert_eq!(sync_id.as_deref(), Some("s1"));
    assert_eq!(docs.len(), 1);

    // target 不匹配
    assert_eq!(
        validate_incoming_share_payload(&payload, Some(&"cd".repeat(32))),
        Err("target mismatch")
    );
    // 未登录
    assert_eq!(
        validate_incoming_share_payload(&payload, None),
        Err("missing identity context")
    );
    // 成员不含本机
    let payload = serde_json::json!({
        "targetRootId": me,
        "organization": {"orgId": "org_x", "members": [{"rootId": "zz".repeat(32)}]}
    });
    assert_eq!(
        validate_incoming_share_payload(&payload, Some(&me)),
        Err("current root not found in members")
    );
    // 缺 orgId
    let payload = serde_json::json!({"targetRootId": me, "organization": {}});
    assert_eq!(
        validate_incoming_share_payload(&payload, Some(&me)),
        Err("invalid payload")
    );
    // summary.members 兜底形状
    let payload = serde_json::json!({
        "targetRootId": me,
        "organization": {"orgId": "org_x", "summary": {"members": [{"rootId": me}]}}
    });
    assert!(validate_incoming_share_payload(&payload, Some(&me)).is_ok());
}
