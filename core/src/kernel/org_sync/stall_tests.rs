//! F6（org-sync-stall-fix）回归测试：
//! - KeepaliveTick 注入合并（§3.1：在飞标记，幂等周期任务不积压）；
//! - worker 开始处理 tick 时清在飞标记（合并的放行半边）；
//! - tick 四阶段超时（§3.2）：S2 反熵对账挂起时在预算内放弃，且 S3
//!   orgsync-hello 恒执行不被跳过（假事件循环：org-pull 命令挂起不应答，
//!   复现对端半连接长超时）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use super::{
    OrgSyncContext, OrgSyncRequest, TickStageBudgets, inject_keepalive_tick, spawn_worker,
};
use crate::org::OrganizationService;
use crate::org::types::{
    OrganizationDeviceSet, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
    OrganizationRole,
};
use crate::p2p::P2pNode;
use crate::p2p::node::{Command, LocalP2PNodeInfo};

/// 测试用 OrgSyncContext：tempdir sled 存储 + 桩节点（命令接收端交还测试，
/// 由测试扮演假事件循环选择性应答/挂起）。
struct TestRig {
    ctx: OrgSyncContext,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    /// 保活 tempdir（drop 即删库）。
    _tmp: tempfile::TempDir,
}

fn test_rig(root_id: Option<&str>, budgets: TickStageBudgets) -> TestRig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sled =
        crate::storage::SledStorage::open(tmp.path().join("db")).expect("sled open");
    let storage = crate::sync::versioned::VersionedStorage::new(
        sled,
        Arc::new(Mutex::new("stub-node".to_string())),
    );
    let (node, cmd_rx) = P2pNode::stub_for_test();
    let (event_tx, _) = broadcast::channel(8);
    TestRig {
        ctx: OrgSyncContext {
            storage,
            node: Arc::new(node),
            current_root_id: Arc::new(Mutex::new(root_id.map(str::to_string))),
            signing_key: Arc::new(Mutex::new(Some(
                crate::org::org_address::generate_org_root_signing_key(),
            ))),
            collection_configs: Default::default(),
            org_acks: Default::default(),
            event_tx,
            recovery_trigger: Default::default(),
            org_address_publish: Default::default(),
            data_dir: tmp.path().to_path_buf(),
            self_device_link: Default::default(),
            self_device_links: Default::default(),
            pdsync_capable_self_devices: Default::default(),
            orgsync_capable_member_peers: Default::default(),
            self_hello_state: Default::default(),
            self_hello_immediate: Default::default(),
            filter_caps: Default::default(),
            replica_check: Default::default(),
            recovery_refresh: Default::default(),
            tick_in_flight: Arc::new(AtomicBool::new(false)),
            tick_budgets: budgets,
            io_lock: Arc::new(Mutex::new(())),
        },
        cmd_rx,
        _tmp: tmp,
    }
}

/// 假事件循环：LocalNodeInfo/ConnectPeer 即时应答，OrgPullRequest 挂起
/// 不应答（故障注入：对端半连接长超时），DmDirect 记录信封 kind 后应答。
/// 返回捕获的 dm kind 序列。
fn spawn_fake_event_loop(mut cmd_rx: mpsc::UnboundedReceiver<Command>) -> Arc<Mutex<Vec<String>>> {
    let dm_kinds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let kinds = Arc::clone(&dm_kinds);
    tokio::spawn(async move {
        // 挂起的 pull 应答通道：持有不响应（调用方靠外层阶段预算放弃）
        let mut stalled = Vec::new();
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::LocalNodeInfo { tx } => {
                    let _ = tx.send(LocalP2PNodeInfo {
                        started: true,
                        peer_id: Some("peer-a".to_string()),
                        addresses: Vec::new(),
                        connected_peers: vec!["peer-b".to_string()],
                        spark_sync_subscribers: Vec::new(),
                    });
                }
                Command::ConnectPeer { tx, .. } => {
                    let _ = tx.send(Ok(()));
                }
                Command::OrgPullRequest { tx, .. } => stalled.push(tx),
                Command::DmDirect { payload, tx, .. } => {
                    if let Some(kind) = payload.get("kind").and_then(|v| v.as_str()) {
                        kinds.lock().unwrap().push(kind.to_string());
                    }
                    let _ = tx.send(Ok(None));
                }
                _ => {}
            }
        }
        drop(stalled);
    });
    dm_kinds
}

/// 测试组织：本机 root-a（管理员，非网关）+ root-b（peer-b，显式指定网关
/// ——跳过缺省活跃集轮换的时间依赖，S2 反熵对账恒选中已连接的 peer-b）。
fn test_org_record() -> OrganizationRecord {
    let member = |root_id: &str, role: OrganizationRole, peer_id: Option<&str>| {
        OrganizationMember {
            root_id: root_id.to_string(),
            role,
            joined_at: 1000,
            added_by: "root-a".to_string(),
            node_info: peer_id.map(|p| {
                OrganizationDeviceSet::from_single(OrganizationNodeInfo {
                    device_uid: None,
                    peer_id: Some(p.to_string()),
                    addresses: Vec::new(),
                })
            }),
            nickname: None,
            avatar: None,
            signature: None,
            gender: None,
            region: None,
            use_personal_identity: None,
            access_key: None,
            extra: Default::default(),
        }
    };
    OrganizationRecord {
        org_id: "org_stall".to_string(),
        name: "stall-test".to_string(),
        description: String::new(),
        avatar: String::new(),
        base_plugin_domain: None,
        created_at: 1000,
        created_by: "root-a".to_string(),
        updated_at: 1000,
        members: vec![
            member("root-a", OrganizationRole::Admin, None),
            member("root-b", OrganizationRole::Member, Some("peer-b")),
        ],
        sync: None,
        gateways: vec!["root-b".to_string()],
        data_accounts: vec![],
        org_address: None,
        is_public: false,
        extra: Default::default(),
    }
}

/// §3.1 注入合并：连续注入只入队 1 份；事件语义（PushOrg/SelfHelloNow）
/// 不经合并不受限；worker 清标记后注入再放行。
#[test]
fn tick_injection_coalesces_on_in_flight() {
    let (tx, rx) = mpsc::unbounded_channel::<OrgSyncRequest>();
    let flag = AtomicBool::new(false);

    assert!(inject_keepalive_tick(&tx, &flag), "首份 tick 注入");
    assert!(!inject_keepalive_tick(&tx, &flag), "在飞中重复注入被合并");
    assert!(!inject_keepalive_tick(&tx, &flag), "在飞中重复注入被合并");
    assert_eq!(rx.len(), 1, "3 次注入合并为队列中的 1 份");

    // PushOrg/SelfHelloNow 是事件语义：不经过合并，不受在飞标记限制
    tx.send(OrgSyncRequest::PushOrg {
        org_id: "org1".to_string(),
        actor_root_id: "root-a".to_string(),
    })
    .unwrap();
    tx.send(OrgSyncRequest::SelfHelloNow).unwrap();
    assert_eq!(rx.len(), 3, "事件语义请求不合并");

    // worker 开始处理 tick 时清标记 → 后续 tick 事件可再注入一份
    flag.store(false, Ordering::SeqCst);
    assert!(inject_keepalive_tick(&tx, &flag), "标记清除后注入放行");
    assert_eq!(rx.len(), 4);
}

/// §3.1 的 worker 半边：worker 开始处理 tick 时清在飞标记（处理期间到达
/// 的新 tick 事件可再注入，队列至多积压 1 份）。
#[tokio::test]
async fn worker_clears_tick_in_flight_when_tick_starts() {
    // 无 root_id：tick 本体 no-op 即时完成，只验证标记清除语义
    let rig = test_rig(None, TickStageBudgets::default());
    let (tx, rx) = mpsc::unbounded_channel::<OrgSyncRequest>();
    let flag = Arc::clone(&rig.ctx.tick_in_flight);
    let worker = spawn_worker(&tokio::runtime::Handle::current(), rig.ctx, rx);

    assert!(inject_keepalive_tick(&tx, &flag));
    // 等 worker 取走 tick 并清标记（轮询，不固定 sleep）
    let mut cleared = false;
    for _ in 0..200 {
        if !flag.load(Ordering::SeqCst) {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(cleared, "worker 开始处理 tick 必须清在飞标记");
    assert!(inject_keepalive_tick(&tx, &flag), "标记清除后再注入放行");
    worker.abort();
}

/// §3.2 阶段预算 + S3 恒执行：对端 org-pull 挂起不应答（假事件循环持有
/// 不应答）时，S2 反熵对账在预算内放弃，且 S3 orgsync-hello 仍照常发出
/// ——修复前末段 hello 会被前序长链无限推迟（F6 观测形态）。
#[tokio::test]
async fn reconcile_stall_does_not_skip_orgsync_hello() {
    let budgets = TickStageBudgets {
        gateway_publish: Duration::from_millis(500),
        self_device_link: Duration::from_millis(500),
        reconcile: Duration::from_millis(300),
        orgsync_hello: Duration::from_secs(2),
    };
    let mut rig = test_rig(Some("root-a"), budgets);
    OrganizationService::save_record(&mut rig.ctx.storage.raw().clone(), &test_org_record())
        .expect("save org record");
    let cmd_rx = std::mem::replace(&mut rig.cmd_rx, mpsc::unbounded_channel().1);
    let dm_kinds = spawn_fake_event_loop(cmd_rx);

    let started = std::time::Instant::now();
    rig.ctx.maintain_org_tick().await;
    let elapsed = started.elapsed();

    // S2 在预算（300ms）内放弃，而非挂到 pull 的 API 超时（15s）
    assert!(
        elapsed < Duration::from_secs(3),
        "S2 挂起必须在阶段预算内放弃，实际耗时 {elapsed:?}"
    );
    // S3 恒执行：orgsync-hello 照发（向已连接的复制组成员 peer-b）
    let kinds = dm_kinds.lock().unwrap();
    assert!(
        kinds
            .iter()
            .any(|k| k == crate::kernel::dm_envelope::KIND_ORGSYNC_HELLO),
        "S2 超时放弃不得跳过 S3 orgsync-hello，实际 dm kinds={kinds:?}"
    );
}
