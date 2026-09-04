//! F6（org-sync-stall-fix）回归测试：
//! - KeepaliveTick 注入合并（§3.1：在飞标记，幂等周期任务不积压）；
//! - worker 开始处理 tick 时清在飞标记（合并的放行半边）。
//! （原 S2 反熵对账的两例阶段预算测试随阶段四A P3 legacy pull 出站停发
//! 删除——S2 阶段已不存在。）

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use super::{
    OrgSyncContext, OrgSyncRequest, TickStageBudgets, inject_keepalive_tick, spawn_worker,
};
use crate::p2p::P2pNode;

/// 测试用 OrgSyncContext：tempdir sled 存储 + 桩节点（命令接收端交还测试，
/// 由测试扮演假事件循环选择性应答/挂起）。
struct TestRig {
    ctx: OrgSyncContext,
    /// 保活 tempdir（drop 即删库）。
    _tmp: tempfile::TempDir,
}

fn test_rig(root_id: Option<&str>, budgets: TickStageBudgets) -> TestRig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sled = crate::storage::SledStorage::open(tmp.path().join("db")).expect("sled open");
    let storage = crate::sync::versioned::VersionedStorage::new(
        crate::storage::Backend::Sled(sled),
        Arc::new(Mutex::new("stub-node".to_string())),
    );
    let (node, _cmd_rx) = P2pNode::stub_for_test();
    let (event_tx, _) = broadcast::channel(8);
    TestRig {
        ctx: OrgSyncContext {
            storage,
            node: Arc::new(node),
            current_root_id: Arc::new(Mutex::new(root_id.map(str::to_string))),
            signing_key: Arc::new(Mutex::new(Some(
                crate::org::org_address::generate_org_root_signing_key(),
            ))),
            seed_shared: Arc::new(Mutex::new(None)),
            event_tx,
            org_address_publish: Default::default(),
            data_dir: tmp.path().to_path_buf(),
            self_device_link: Default::default(),
            self_device_links: Default::default(),
            pdsync_capable_self_devices: Default::default(),
            self_hello_state: Default::default(),
            self_hello_immediate: Default::default(),
            filter_caps: Default::default(),
            tick_in_flight: Arc::new(AtomicBool::new(false)),
            tick_budgets: budgets,
        },
        _tmp: tmp,
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
    tx.send(OrgSyncRequest::PushOrg { org_id: "org1".to_string() })
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
