//! 集成测试共用夹具：kernel 构建、身份初始化与通用声明/配置助手。
//! 各测试 crate 通过 `mod common;` 引入；p2p/sync 专用夹具见同名子模块。
#![allow(dead_code)]

pub mod p2p;
pub mod sync;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use spark_core::collection::CollectionConfig;
use spark_core::kernel::{Kernel, KernelConfig};
use spark_core::p2p::P2pConfig;
use spark_core::p2p::node::system_now_ms;
use spark_core::schema::{CollectionSchemaDeclaration, SyncStrategy};

pub const PASSWORD: &str = "password123";

pub fn test_p2p_config() -> P2pConfig {
    P2pConfig {
        app_version: "0.0.0-test".to_string(),
        preferred_port: Some(0),
        port_scan: false,
        enable_tcp: true,
        enable_ws: false,
        enable_ipv6: false,
        enable_mdns: false,
        enable_upnp: false,
        keepalive_interval: None,
        dht_mode: spark_core::p2p::DhtMode::Server,
        plugin_announce_pow_bits: None,
        plugin_announce_relay_tenure_ms: None,
        now_fn: Arc::new(system_now_ms),
    }
}

pub fn config(dir: &Path) -> KernelConfig {
    KernelConfig {
        data_dir: dir.to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: Some(test_p2p_config()),
    }
}

pub fn fresh_kernel(dir: &Path) -> Kernel {
    Kernel::init(config(dir)).expect("kernel init")
}

pub fn init_identity(kernel: &mut Kernel) -> (String, String) {
    let result = kernel
        .init_identity(PASSWORD, "  小明  ", None)
        .expect("init identity");
    assert_eq!(result.root_id.len(), 64, "rootId 为 64 字符 hex");
    (result.root_id, result.mnemonic)
}

pub fn lww_evidence_declaration() -> CollectionSchemaDeclaration {
    CollectionSchemaDeclaration {
        sync_strategy: Some(SyncStrategy::Lww),
        governance: false,
        enable_evidence: true,
    }
}

pub fn from_indexed_config() -> CollectionConfig {
    CollectionConfig {
        indexed_fields: vec!["from".to_string()],
        ..Default::default()
    }
}

/// 取 kernel 的可拨地址（0.0.0.0 → 127.0.0.1，仅 ip4）。
pub fn dialable_addrs(kernel: &Kernel) -> Vec<String> {
    kernel
        .p2p_status()
        .unwrap()
        .expect("p2p started")
        .addresses
        .iter()
        .filter(|a| a.contains("/ip4/"))
        .map(|a| a.replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/"))
        .collect()
}

/// 轮询直到 cond 成立（默认 20s 预算，200ms 间隔）。
pub fn wait_until(mut cond: impl FnMut() -> bool, budget_ms: u64, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_millis(budget_ms);
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timeout waiting for: {what}");
}
