//! kernel 数据治理集成测试：purge 预览/校验顺序/执行守卫与 usage/cleanup。

mod common;

use serde_json::json;

use spark_core::collection::CollectionConfig;
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::node::system_now_ms;

use common::*;

// ---------------------------------------------------------------------------
// purge：预览 + 校验顺序（管理员 → 导出确认 → P2P 启动）
// ---------------------------------------------------------------------------

#[test]
fn purge_preview_and_execute_guards() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    // 登录即在线：init_identity 已自动启动 p2p；本用例覆盖未启动路径，先停
    kernel.stop_p2p().unwrap();
    let view = kernel
        .create_org(CreateOrganizationInput {
            name: "组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:app".to_string()),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();

    kernel
        .declare_collection("plugin:app", "notes", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put(
            "plugin:app",
            "notes",
            "n1",
            json!({"v": 1}),
            CollectionConfig::default(),
        )
        .unwrap();
    kernel
        .doc_put(
            "plugin:app",
            "notes",
            "n2",
            json!({"v": 2}),
            CollectionConfig::default(),
        )
        .unwrap();

    let before_ts = system_now_ms() + 3_600_000;
    // 预览：两篇受影响；p2p 未启动 → replica 为 None；管理员标记 true
    let preview = kernel.preview_purge(&org_id, before_ts).unwrap();
    assert_eq!(preview.domain, "plugin:app");
    assert_eq!(preview.preview.affected_docs, 2);
    assert!(preview.is_current_user_admin);
    assert!(preview.replica.is_none());
    // 预览不写：文档仍在
    assert!(
        kernel
            .doc_get("plugin:app", "notes", "n1")
            .unwrap()
            .is_some()
    );

    // 未确认导出 → 拒绝（管理员校验在前但已满足）
    let err = kernel.execute_purge(&org_id, before_ts, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Export backup first: confirmExported must be true before purging"
    );
    // p2p 未启动 → 拒绝
    let err = kernel.execute_purge(&org_id, before_ts, true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "P2P network is not started; cannot verify replica sufficiency, purge refused"
    );

    // p2p 启动但副本不足（1/3）→ 拒绝
    kernel.start_p2p().unwrap();
    let err = kernel.execute_purge(&org_id, before_ts, true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Replica insufficient (1/3): purging local copies now may lose organization data. \
         Wait for replicas to replenish or add disk space instead."
    );
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 数据治理：usage / cleanup / export
// ---------------------------------------------------------------------------

#[test]
fn usage_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    kernel
        .doc_put(
            "chat",
            "misc",
            "k1",
            json!({"v": 1}),
            CollectionConfig::default(),
        )
        .unwrap();

    let usage = kernel.get_usage().unwrap();
    assert!(usage.total_keys >= 1);
    assert!(usage.disk.is_some(), "数据目录给定 → 含磁盘信息");

    let cleanup = kernel.run_cleanup_now().unwrap();
    assert_eq!(cleanup.tombstones, 0, "无过期墓碑");

    kernel.shutdown().unwrap();
}
