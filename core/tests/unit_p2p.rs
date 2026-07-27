//! p2p 模块单元测试（自 `src/p2p/**` 迁移：只依赖 spark-core 公开 API 的用例）。

#[path = "unit_p2p/announce.rs"]
mod announce;
#[path = "unit_p2p/challenge.rs"]
mod challenge;
#[path = "unit_p2p/direct.rs"]
mod direct;
#[path = "unit_p2p/identity_store.rs"]
mod identity_store;
#[path = "unit_p2p/keepalive.rs"]
mod keepalive;
#[path = "unit_p2p/listen_port.rs"]
mod listen_port;
#[path = "unit_p2p/overlay_store.rs"]
mod overlay_store;
#[path = "unit_p2p/peer_targets.rs"]
mod peer_targets;
