//! `data_ops.rs` 的内联单元测试（拆分至独立文件，满足 650 行硬线——测试
//! 用例天然罗列，orgsync.rs 同款先例）。
use super::*;
use crate::kernel::KernelConfig;
use crate::org::service::CreateOrganizationInput;
use crate::plugindata::{Accounts, Scope, Space};
use crate::storage::StorageBackend;
use crate::sync::orgsync::orgq_queue_has_data;
use serde_json::json;

const PASSWORD: &str = "correct-horse-battery";

fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel.init_identity(PASSWORD, "alice", None).unwrap();
    (dir, kernel)
}

/// O3 成员写入主动入队：非数据账号成员写 org data-accounts 集合且全部数据
/// 账号离线 → 落本地 orgq 队列（而非直接落 orgd 副本/报错）。
#[test]
fn member_write_to_offline_data_accounts_enqueues() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .org_set_member_role(&org_id, BOB, crate::org::OrganizationRole::Admin)
        .unwrap();
    kernel
        .org_set_data_accounts(&org_id, &[BOB.to_string()])
        .unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:finance".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    kernel
        .data_save(
            "plugin:ai-chat",
            "ai-chat:finance",
            "k1",
            json!({"amt": 1}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    assert!(
        orgq_queue_has_data(kernel.require_storage().unwrap(), &org_id),
        "全部数据账号离线 → 成员写入入队"
    );
    let data_key = format!("orgd:{org_id}:ai-chat:finance@v1.0.0:k1");
    let storage = kernel.require_storage().unwrap();
    assert!(
        storage.get(&data_key).unwrap().is_none(),
        "成员写 data-accounts 不落 orgd 副本"
    );
}

/// O3 读路由决策：非数据账号成员对 org data-accounts 集合且数据账号离线 →
/// `Offline`（有缓存回缓存/无缓存 UnavailableOffline）；本机是数据账号 →
/// `Local`。
#[test]
fn data_orgq_read_plan_routes_offline_for_member() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .org_set_member_role(&org_id, BOB, crate::org::OrganizationRole::Admin)
        .unwrap();
    kernel
        .org_set_data_accounts(&org_id, &[BOB.to_string()])
        .unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:finance".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    let plan = kernel
        .data_orgq_read_plan(
            &org_id,
            "ai-chat:finance",
            "1.0.0",
            &std::collections::HashSet::new(),
        )
        .unwrap();
    assert_eq!(
        plan,
        crate::sync::orgsync::MemberReadPlan::Offline { has_cache: false },
        "成员对离线 data-accounts 集合 → Offline"
    );
}

/// O3 读路径透明路由（Tauri 通路）：非数据账号成员经 `data_get`（带 org_id）
/// 读 data-accounts 集合 → 路由到成员侧缓存（无缓存 → None=UnavailableOffline；
/// 有缓存 → 返回缓存值，UI 标注陈旧）。
#[test]
fn org_read_routes_to_member_cache() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
    kernel.org_add_member(&org_id, BOB, None).unwrap();
    kernel
        .org_set_member_role(&org_id, BOB, crate::org::OrganizationRole::Admin)
        .unwrap();
    kernel
        .org_set_data_accounts(&org_id, &[BOB.to_string()])
        .unwrap();
    kernel
        .data_declare_collection(
            "plugin:ai-chat",
            DeclareInput {
                name: "ai-chat:finance".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::DataAccounts),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    assert!(
        kernel
            .data_get(
                "plugin:ai-chat",
                "ai-chat:finance",
                "k1",
                Some("1.0.0"),
                Some(&org_id)
            )
            .unwrap()
            .is_none(),
        "成员读离线 data-accounts 无缓存 → None"
    );
    let col = "ai-chat:finance@v1.0.0";
    let rec = crate::sync::orgsync::OrgqRespRecord {
        key: format!("orgd:{org_id}:{col}:k1"),
        value: serde_json::json!({"amt": 9}),
        meta: Default::default(),
    };
    kernel
        .require_storage_mut()
        .unwrap()
        .put(
            &crate::sync::orgsync::orgq_cache_key(&org_id, col, "k1"),
            &serde_json::to_string(&rec).unwrap(),
        )
        .unwrap();
    let got = kernel
        .data_get(
            "plugin:ai-chat",
            "ai-chat:finance",
            "k1",
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap()
        .expect("缓存命中");
    assert_eq!(got["amt"], json!(9), "成员读 data-accounts 回缓存");
}

/// O7：org 分支（Some(oid) → resolve_org）同样强制插件前缀归属——插件 A 用
/// 他插件 B 的集合名前缀调 org data_save → NamePrefixMismatch 拒绝（与个人
/// 路径对称，防越权触达他插件集合）。
#[test]
fn org_branch_rejects_cross_plugin_collection_prefix() {
    let (_dir, mut kernel) = unlocked_kernel();
    let org = kernel
        .create_org(CreateOrganizationInput {
            name: "测试组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: None,
            ..Default::default()
        })
        .unwrap();
    let org_id = org.record.org_id.clone();
    // 声明一个属于插件 B 前缀的 org 集合
    kernel
        .data_declare_collection(
            "plugin:plugin-b",
            DeclareInput {
                name: "plugin-b:ledger".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            Some(&org_id),
        )
        .unwrap();
    // 插件 A（plugin:plugin-a）试图以 B 的前缀写 org 集合 → 拒绝
    let err = kernel
        .data_save(
            "plugin:plugin-a",
            "plugin-b:ledger",
            "k1",
            json!({"amt": 1}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("does not belong to plugin")
            || err.to_string().contains("NamePrefixMismatch")
            || err.to_string().contains("prefix"),
        "插件 A 调插件 B 集合（org 分支）应被前缀归属拒绝，got: {err}"
    );
}
