//! org 模块单元测试（自 `src/org/**` 迁移：只依赖 spark-core 公开 API 的用例）。

#[path = "unit_org/claim.rs"]
mod claim;
#[path = "unit_org/gateway.rs"]
mod gateway;
#[path = "unit_org/invite.rs"]
mod invite;
#[path = "unit_org/node_card.rs"]
mod node_card;
#[path = "unit_org/org_address.rs"]
mod org_address;
#[path = "unit_org/plugin_docs.rs"]
mod plugin_docs;
#[path = "unit_org/pull.rs"]
mod pull;
#[path = "unit_org/recovery.rs"]
mod recovery;
#[path = "unit_org/replica.rs"]
mod replica;
#[path = "unit_org/service.rs"]
mod service;
#[path = "unit_org/snapshot.rs"]
mod snapshot;
#[path = "unit_org/sync_state.rs"]
mod sync_state;
#[path = "unit_org/tx.rs"]
mod tx;
#[path = "unit_org/types.rs"]
mod types;
