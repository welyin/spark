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

/// A14 全员数据节点（membership §4.1）：成员写 org data-accounts 集合 →
/// 本地驻留直接落 orgd 副本（成员即数据节点，无「数据账号离线入队」旧语义）。
#[test]
fn member_write_data_accounts_lands_local_replica() {
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
    let data_key = format!("orgd:{org_id}:ai-chat:finance@v1.0.0:k1");
    let storage = kernel.require_storage().unwrap();
    assert!(
        storage.get(&data_key).unwrap().is_some(),
        "成员写 data-accounts 落本地 orgd 副本（全员数据节点）"
    );
    assert!(
        !orgq_queue_has_data(storage, &org_id),
        "本地驻留不入队"
    );
}

/// A14 读路由决策：成员对 org data-accounts 集合恒 `Local`（成员即数据
/// 节点，本地直读；无「非数据账号成员 Offline」旧分支）。
#[test]
fn data_orgq_read_plan_local_for_member() {
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
        crate::sync::orgsync::MemberReadPlan::Local,
        "成员对 data-accounts 集合 → Local（全员数据节点）"
    );
}

/// A14 读路径：成员经 `data_get`（带 org_id）读 data-accounts 集合 → 本地
/// orgd 直读（无缓存命中概念——本地副本即数据源）。
#[test]
fn org_read_serves_local_replica() {
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
    // 本地无副本 → None
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
        "本地无副本 → None"
    );
    // 写入后 → 本地直读命中
    kernel
        .data_save(
            "plugin:ai-chat",
            "ai-chat:finance",
            "k1",
            json!({"amt": 9}),
            Some("1.0.0"),
            Some(&org_id),
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
        .expect("本地副本命中");
    assert_eq!(got["amt"], json!(9), "成员读 data-accounts 本地直读");
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
