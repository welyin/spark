//! kernel 网络编排集成测试：双 kernel 反熵对账 + keepalive 注入、
//! 节点名片导入（未验证入池）与名片生成守卫。

mod common;

use std::path::Path;
use std::time::Duration;

use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::org::service::CreateOrganizationInput;
use spark_core::p2p::node::system_now_ms;
use spark_core::p2p::{P2pConfig, P2pEvent};

use common::*;

/// 反熵对账 + keepalive 注入 + clear_peer_records：
/// B 的 nodeInfo 地址故意写错（推送不可达），B 经 sync_peer_organizations
/// 显式反熵收敛；短间隔 keepalive 验证 tick 编排不炸。
#[test]
fn reconcile_and_keepalive_converge() {
    let keepalive_config = |dir: &Path| KernelConfig {
        data_dir: dir.to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: Some(P2pConfig {
            keepalive_interval: Some(Duration::from_millis(800)),
            ..test_p2p_config()
        }),
    };
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = Kernel::init(keepalive_config(dir_a.path())).unwrap();
    let mut kernel_b = Kernel::init(keepalive_config(dir_b.path())).unwrap();
    let (_root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    // A 建组织 + 预录 B：peerId 真实但地址错误（推送不可达，B 只能靠自己回拉）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "反熵组织".to_string(),
            description: None,
            base_plugin_domain: Some("plugin:app".to_string()),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();
    let b_node_broken = spark_core::org::OrganizationNodeInfo {
        peer_id: Some(b_peer.clone()),
        addresses: vec!["/ip4/127.0.0.1/tcp/1".to_string()],
    };
    kernel_a
        .org_add_member(&org_id, &root_b, Some(&b_node_broken))
        .unwrap();
    assert!(
        kernel_b.list_orgs().unwrap().is_empty(),
        "推送不可达，B 尚无记录"
    );

    // B 显式反熵：pull-list（memberAuthStatus 凭 peerId 放行）→ B 无本地记录 → 拉取
    let a_node = spark_core::org::OrganizationNodeInfo {
        peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(&kernel_a),
    };
    let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
    assert_eq!(result.pull_checked, 1);
    assert_eq!(result.pull_synced, 1, "B 拉到组织");
    assert_eq!(result.removed, 0);
    let mine_b = kernel_b.list_orgs().unwrap();
    assert_eq!(mine_b.len(), 1);
    assert_eq!(mine_b[0].member_count, 2);

    // 版本一致后再对账：skip 分支
    let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
    assert_eq!(result.pull_checked, 1);
    assert_eq!(result.pull_synced, 0);
    assert_eq!(result.skipped, 1, "版本一致跳过");

    // A 侧有向 B 的失败拨号记录 → clear_peer_records 清空
    let cleared = kernel_a.clear_peer_records().unwrap();
    assert!(cleared >= 1, "A 的活跃度记录被清除");
    assert_eq!(kernel_a.clear_peer_records().unwrap(), 0, "已清空");

    // keepalive tick 自然驱动（800ms 间隔）：A 继续拨号 B（错地址失败静默）、
    // B 无候选；观察 B 的 KeepaliveTick 事件证明 tick → worker 链路存活
    let mut events = kernel_b.subscribe_p2p_events();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ticked = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(P2pEvent::KeepaliveTick(_)) => return true,
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
        })
        .await
        .unwrap_or(false)
    });
    assert!(ticked, "keepalive tick 事件到达（组织保活已触发）");

    // org_keepalive_once：无候选场景幂等不炸
    kernel_b.org_keepalive_once().unwrap();
    kernel_a.org_keepalive_once().unwrap();

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// 节点名片（org.md §17）：A 生成名片 → B 导入 → 邻居池出现未验证条目并
/// 完成连接；带 orgId 的名片附恢复 token；篡改名片导入被拒。
#[test]
fn node_card_import_lands_unverified_pool_entry() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    init_identity(&mut kernel_a);
    init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();

    // 带组织恢复 token 的名片：A 先建组织（创建时生成 recoverySecret）
    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "名片组织".to_string(),
            description: None,
            base_plugin_domain: Some("plugin:app".to_string()),
        })
        .unwrap();
    let card = kernel_a.make_node_card(Some(&view.record.org_id)).unwrap();

    // 名片可被纯函数验签，且附带恢复 token
    let parsed = spark_core::org::parse_and_verify_node_card(&card, system_now_ms()).unwrap();
    assert_eq!(parsed.peer_id, a_peer);
    assert!(parsed.recovery_token.is_some());

    // B 导入：验签 → 未验证入池 → 连接成功（A 可达）
    let import = kernel_b.import_node_card(&card).unwrap();
    assert_eq!(import.peer_id, a_peer);
    assert!(import.has_recovery_token);
    assert_eq!(import.connect_error, None, "A 可达，连接应成功");
    wait_until(
        || {
            kernel_b
                .p2p_status()
                .map(|s| s.unwrap().connected_peers.contains(&a_peer))
                .unwrap_or(false)
        },
        10_000,
        "B 连接名片节点 A",
    );

    // 篡改名片（换地址重编码）→ 导入拒绝（签名校验失败）
    let mut tampered = parsed.clone();
    tampered.addresses = vec!["/ip4/9.9.9.9/tcp/15002/ws".to_string()];
    let tampered_code = spark_core::org::encode_node_card(&tampered);
    let err = kernel_b.import_node_card(&tampered_code).unwrap_err();
    assert!(
        err.to_string().contains("签名校验失败"),
        "篡改名片报错：{err}"
    );

    // 关停 B 后直接读 sled：邻居池存在 A 的未验证条目（信任边界不变）
    let b_storage_dir = kernel_b.storage_dir().unwrap();
    kernel_b.shutdown().unwrap();
    kernel_a.shutdown().unwrap();
    let mut storage = spark_core::storage::SledStorage::open(&b_storage_dir).unwrap();
    let mut store = spark_core::p2p::OverlayPeerStore::new(&mut storage);
    let entry = store.get(&a_peer).unwrap().expect("A 在 B 的邻居池");
    assert!(!entry.verified, "名片导入一律未验证口径");
    assert!(!entry.addresses.is_empty());
}

/// 名片生成的校验链：P2P 未启动报 `p2p node not started`；组织不存在报
/// `Organization not found`。
#[test]
fn make_node_card_guard_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    // 登录即在线：init_identity 已自动启动 p2p；本用例覆盖未启动路径，先停
    kernel.stop_p2p().unwrap();

    // P2P 未启动
    assert_eq!(
        kernel.make_node_card(None).unwrap_err().to_string(),
        "p2p node not started"
    );

    kernel.start_p2p().unwrap();
    // 组织不存在
    assert_eq!(
        kernel
            .make_node_card(Some("org_0123456789abcdef"))
            .unwrap_err()
            .to_string(),
        "Organization not found"
    );
    // 不带 orgId：成功且不带恢复 token
    let card = kernel.make_node_card(None).unwrap();
    let parsed = spark_core::org::parse_and_verify_node_card(&card, system_now_ms()).unwrap();
    assert_eq!(parsed.recovery_token, None);

    kernel.shutdown().unwrap();
}
