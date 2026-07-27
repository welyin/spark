//! kernel 接受邀请与组织推送编排集成测试：accept_invite 守卫/全流程
//! （原始 P2pNode 扮演邀请方）、双 kernel 互连对跑与 org-share 推送。

mod common;

use std::time::Duration;

use serde_json::{Value, json};

use spark_core::org::invite::{OrgInviteInviter, OrgInvitePayload, encode_org_invite};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::P2pEvent;
use spark_core::p2p::node::system_now_ms;

use common::*;

#[test]
fn accept_invite_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 未解锁 → Locked
    assert_eq!(
        kernel.accept_invite("whatever").unwrap_err().to_string(),
        "Root identity is locked"
    );

    let (root_id, _) = init_identity(&mut kernel);
    // 登录即在线：init_identity 已自动启动 p2p；本用例覆盖未启动路径，先停
    kernel.stop_p2p().unwrap();

    // 坏邀请码 → 解析错误（发生在 p2p 检查之前，对齐 TS 先解码）
    assert!(kernel.accept_invite("not-a-code").is_err());

    // 自邀拒绝（p2p 未启动也先报自邀，对齐 TS 校验顺序）
    let self_code = encode_org_invite(&OrgInvitePayload::new(
        "org_selfinvite1".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id,
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&self_code).unwrap_err().to_string(),
        "不能接受自己发出的邀请码"
    );

    // 合法他人邀请码但 p2p 未启动 → TS 文案
    let other_code = encode_org_invite(&OrgInvitePayload::new(
        "org_otherinvite".to_string(),
        "组织".to_string(),
        OrgInviteInviter {
            root_id: "cd".repeat(32),
            peer_id: Some("peer-1234567890".to_string()),
            addresses: vec![],
        },
        system_now_ms(),
    ));
    assert_eq!(
        kernel.accept_invite(&other_code).unwrap_err().to_string(),
        "P2P 网络未启动，无法通过邀请码加入"
    );

    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// accept_invite 全流程：原始 P2pNode 扮演邀请方（org-pull 响应宿主），
// kernel 作为加入方完成 连接 → claim 捎带 → 拉取 → 落库确认。
// ---------------------------------------------------------------------------

/// 邀请方宿主：serve 受邀组织快照与 pluginDocs（org-pull 直连响应）。
struct InviteAdminHost {
    org_id: String,
    record_value: Value,
    plugin_docs: Vec<Value>,
}

impl spark_core::p2p::P2pHost for InviteAdminHost {
    fn handle_org_pull_list(
        &mut self,
        _payload: Value,
        _remote_peer_id: Option<String>,
    ) -> Result<Value, String> {
        Ok(json!({
            "ok": true,
            "type": "org-pull-list-response",
            "organizations": [{ "orgId": self.org_id }]
        }))
    }

    fn handle_org_pull_org(
        &mut self,
        payload: Value,
        _remote_peer_id: Option<String>,
    ) -> Result<Value, String> {
        let org_id = payload.get("orgId").and_then(Value::as_str).unwrap_or("");
        if org_id != self.org_id {
            return Ok(json!({
                "ok": true,
                "type": "org-pull-org-response",
                "orgId": org_id,
                "status": "removed",
                "reason": "org-not-found"
            }));
        }
        Ok(json!({
            "ok": true,
            "type": "org-pull-org-response",
            "orgId": self.org_id,
            "status": "member",
            "organization": self.record_value,
            "pluginDocs": self.plugin_docs,
        }))
    }
}

#[test]
fn accept_invite_full_flow() {
    // 加入方 kernel（先建身份，管理员记录需要预录其 rootId）
    let joiner_dir = tempfile::tempdir().unwrap();
    let mut joiner = fresh_kernel(joiner_dir.path());
    let (joiner_root, _) = init_identity(&mut joiner);

    // 管理员侧组织记录：创建者 admin + 预录加入方为 member
    let admin_root = "ef".repeat(32);
    let now = system_now_ms();
    let mut admin_storage = spark_core::storage::MemoryStorage::new();
    let record = spark_core::org::OrganizationService::create_organization(
        &mut admin_storage,
        &CreateOrganizationInput {
            name: "邀请组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        },
        &admin_root,
        now,
    )
    .unwrap();
    let org_id = record.org_id.clone();
    let record = spark_core::org::OrganizationService::add_member(
        &mut admin_storage,
        &org_id,
        &joiner_root,
        None,
        &admin_root,
        now,
    )
    .unwrap();
    let record_value = serde_json::to_value(&record).unwrap();

    // 随快照捎带的插件文档（接收方应应用落库）
    let plugin_docs = vec![json!({
        "domain": "plugin:app",
        "collection": "notes",
        "id": "d1",
        "payload": {"text": "hello", "orgId": org_id},
        "meta": {"vv": {"admin-node": 1}, "ts": now, "nodeId": "admin-node"}
    })];

    // 邀请方节点（独立 tokio runtime；P2pNode 句柄可跨 block_on 持有，
    // 事件循环在该 runtime 上存活，拉取完成后显式 stop）
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (admin_peer, admin_addrs, admin_node) = rt.block_on(async {
        let host = InviteAdminHost {
            org_id: org_id.clone(),
            record_value,
            plugin_docs,
        };
        let mut node = spark_core::p2p::P2pNode::start(
            test_p2p_config(),
            spark_core::storage::MemoryStorage::new(),
            Box::new(host),
        )
        .await
        .expect("admin node starts");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let addrs = loop {
            let event = tokio::time::timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                node.next_event(),
            )
            .await
            .expect("Started event")
            .expect("event stream open");
            if let P2pEvent::Started {
                listen_addresses, ..
            } = event
            {
                break listen_addresses;
            }
        };
        let peer = node.peer_id().to_string();
        let dialable: Vec<String> = addrs
            .iter()
            .filter(|a| a.contains("/ip4/"))
            .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
            .collect();
        (peer, dialable, node)
    });
    assert!(!admin_addrs.is_empty(), "管理员节点应有可拨地址");

    // 邀请码：管理员 rootId + 节点信息
    let code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "邀请组织".to_string(),
        OrgInviteInviter {
            root_id: admin_root.clone(),
            peer_id: Some(admin_peer),
            addresses: admin_addrs,
        },
        system_now_ms(),
    ));

    // 加入方启动 p2p 并接受邀请：连接 → claim 捎带 → 拉取 → 落库确认
    joiner.start_p2p().unwrap();
    let acceptance = joiner.accept_invite(&code).unwrap();
    assert_eq!(acceptance.org_id, org_id);
    assert_eq!(acceptance.org_name, "邀请组织");
    assert_eq!(acceptance.member_count, 2);

    // 组织记录落库：当前用户为 member 角色
    let mine = joiner.list_orgs().unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].record.org_id, org_id);
    assert!(!mine[0].is_current_user_admin);
    assert_eq!(mine[0].member_count, 2);

    // pluginDocs 已应用
    let doc = joiner.doc_get("plugin:app", "notes", "d1").unwrap();
    assert_eq!(doc, Some(json!({"text": "hello", "orgId": org_id})));

    joiner.shutdown().unwrap();
    rt.block_on(admin_node.stop());
}

// ---------------------------------------------------------------------------
// 阶段③c 组织同步编排：双 kernel 互连对跑
// （org-share 推送 / org-pull 响应方 / accept_invite 全流程）
// ---------------------------------------------------------------------------

/// org-share 推送编排：A add_member 触发推送 → B（目标 rootId）落库 + A 记账。
#[test]
fn org_share_push_delivers_between_kernels() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织并把 B 预录为成员（带 B 的真实 nodeInfo → 推送可直连送达）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "推送组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        peer_id: Some(kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap()),
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a
        .org_add_member(&org_id, &root_b, Some(&b_node))
        .unwrap();

    // B 收到快照落库（org-share 接收应答：target 匹配 + 成员包含 + merge）
    wait_until(
        || kernel_b.list_orgs().map(|l| l.len() == 1).unwrap_or(false),
        20_000,
        "B 收到组织快照",
    );
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b[0].record.org_id, org_id);
    assert_eq!(mine_b[0].member_count, 2);
    assert!(!mine_b[0].is_current_user_admin, "B 为 member 角色");
    // B 侧记录成员集与 A 一致
    let members_a = kernel_a.list_orgs().unwrap();
    assert_eq!(members_a[0].member_count, 2);

    // A 对 B 的 sync-state 已记账（K 副本口径：B everSynced）
    let overview = kernel_a.org_overview(&org_id).unwrap();
    let b_entry = overview
        .members
        .iter()
        .find(|m| m.root_id == root_b)
        .expect("B 在概览中");
    assert!(b_entry.ever_synced, "直连送达后记账生效");
    assert!(b_entry.last_synced_at.is_some());

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// org-pull 响应方接线：双 kernel accept_invite 全流程（邀请方也是 kernel）。
#[test]
fn accept_invite_two_kernels_full() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B（无 nodeInfo——邀请码引导 claim 回填）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "邀请组织".to_string(),
            description: None,
            base_plugin_domain: "plugin:app".to_string(),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    kernel_a.org_add_member(&org_id, &root_b, None).unwrap();

    // 邀请码：A 的 rootId + 真实节点信息
    let code = encode_org_invite(&OrgInvitePayload::new(
        org_id.clone(),
        "邀请组织".to_string(),
        OrgInviteInviter {
            root_id: root_a.clone(),
            peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
            addresses: dialable_addrs(&kernel_a),
        },
        system_now_ms(),
    ));

    // B 接受邀请：connect → pull-list（捎带 claim）→ pull-org → 落库确认
    let acceptance = kernel_b.accept_invite(&code).unwrap();
    assert_eq!(acceptance.org_id, org_id);
    assert_eq!(acceptance.member_count, 2);
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert!(!mine_b[0].is_current_user_admin);

    // A 侧 claim 已回填 B 的 nodeInfo（handle_org_pull_list 的 claim 应用路径）
    let record_a = kernel_a.list_orgs().unwrap();
    let b_member = record_a[0]
        .members
        .iter()
        .find(|m| m.root_id == root_b)
        .expect("B 是成员");
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();
    assert_eq!(
        b_member.node_info.as_ref().unwrap().peer_id.as_deref(),
        Some(b_peer.as_str()),
        "claim 回填 B 的 peerId"
    );

    // A 再加一名成员：触发向已知成员推送 → B 收到更新（成员数 3）
    let root_c = "cd".repeat(32);
    kernel_a.org_add_member(&org_id, &root_c, None).unwrap();
    wait_until(
        || {
            kernel_b
                .list_orgs()
                .map(|l| l[0].member_count == 3)
                .unwrap_or(false)
        },
        20_000,
        "B 收到成员变更推送",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}
