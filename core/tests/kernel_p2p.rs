//! kernel p2p 起停集成测试：事件流与状态查询、登录即在线（init/unlock 启动、
//! lock 停止）、未配置 p2p 跳过、广播守卫。

mod common;

use std::time::Duration;

use serde_json::json;

use spark_core::collection::CollectionConfig;
use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::p2p::P2pEvent;

use common::*;

// ---------------------------------------------------------------------------
// p2p 起停：事件流、状态查询、运行期 doc 广播不炸
// ---------------------------------------------------------------------------

#[test]
fn p2p_start_stop_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    // 登录即在线：init_identity 会自动启动 p2p，先订阅事件再登录以捕获 Started
    let mut events = kernel.subscribe_p2p_events();
    init_identity(&mut kernel);

    let peer_id = kernel.start_p2p().unwrap();
    assert!(!peer_id.is_empty());
    assert!(kernel.p2p_running());

    // Started 事件（单独 runtime 接收；kernel 方法本身同步）。
    // 注意：p2p start 后本机设备采集落库会先 emit DeviceUpdated（设备清单），
    // 故循环 recv 跳过其他事件直到 Started。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = rt
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let event = events.recv().await.expect("event channel open");
                    if matches!(event, P2pEvent::Started { .. }) {
                        break event;
                    }
                }
            })
            .await
        })
        .expect("Started event within 15s");
    match started {
        P2pEvent::Started {
            peer_id: started_peer,
            listen_addresses,
        } => {
            assert_eq!(started_peer, peer_id);
            assert!(!listen_addresses.is_empty());
        }
        other => panic!("expected Started event, got {other:?}"),
    }

    // 状态查询
    let status = kernel.p2p_status().unwrap().expect("started");
    assert!(status.started);
    assert_eq!(status.peer_id.as_deref(), Some(peer_id.as_str()));

    // 幂等 start
    assert_eq!(kernel.start_p2p().unwrap(), peer_id);

    // p2p 运行期写文档（广播无订阅者也不影响本地写入）
    kernel
        .declare_collection("chat", "messages", lww_evidence_declaration())
        .unwrap();
    kernel
        .doc_put(
            "chat",
            "messages",
            "p2p-doc",
            json!({"v": 1}),
            CollectionConfig::default(),
        )
        .unwrap();
    assert!(
        kernel
            .doc_get("chat", "messages", "p2p-doc")
            .unwrap()
            .is_some()
    );

    kernel.stop_p2p().unwrap();
    assert!(!kernel.p2p_running());
    assert!(kernel.p2p_status().unwrap().is_none());
    // 幂等 stop
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}

/// 登录即在线：init/unlock 自动启动 p2p；lock 停止；重新 unlock 再启动。
#[test]
fn login_starts_p2p_and_lock_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    assert!(!kernel.p2p_running());

    let (root_id, _mnemonic) = init_identity(&mut kernel);
    assert!(kernel.p2p_running(), "init_identity 后 p2p 应已自动启动");
    assert!(kernel.p2p_start_error().is_none());
    let peer_id = kernel.p2p_status().unwrap().unwrap().peer_id.unwrap();

    kernel.lock();
    assert!(!kernel.p2p_running(), "lock 应停止 p2p");

    kernel.unlock(PASSWORD, Some(&root_id)).unwrap();
    assert!(kernel.p2p_running(), "unlock 后 p2p 应再次启动");
    // 同一身份同一存储目录 → 同一 libp2p 身份
    assert_eq!(
        kernel.p2p_status().unwrap().unwrap().peer_id.unwrap(),
        peer_id
    );
    kernel.shutdown().unwrap();
}

/// 未配置 p2p 的内核：登录链路不自动启动（guard 按配置跳过）。
#[test]
fn login_skips_p2p_when_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::init(KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    init_identity(&mut kernel);
    assert!(!kernel.p2p_running());
    assert!(kernel.p2p_start_error().is_none());
    kernel.shutdown().unwrap();
}

#[test]
fn p2p_broadcast_requires_started_node() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    // 登录即在线：init_identity 已自动启动 p2p；本用例覆盖未启动路径，先停
    kernel.stop_p2p().unwrap();

    let body = spark_core::p2p::build_update_body(
        "plugin:app",
        "notes",
        "n1",
        json!({"v": 1}),
        json!({"vv": {}, "ts": 1}),
        None,
    );
    // 未启动 → TS `p2p node not started`
    assert_eq!(
        kernel
            .p2p_broadcast("spark-sync", body.clone())
            .unwrap_err()
            .to_string(),
        "p2p node not started"
    );

    // 启动后零订阅者广播成功（allowPublishToZeroTopicPeers 口径）
    kernel.start_p2p().unwrap();
    kernel.p2p_broadcast("spark-sync", body).unwrap();
    kernel.stop_p2p().unwrap();
    kernel.shutdown().unwrap();
}
